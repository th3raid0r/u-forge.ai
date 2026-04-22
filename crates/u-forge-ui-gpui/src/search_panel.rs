use std::sync::Arc;

use gpui::{div, prelude::*, px, rgb, rgba, relative, Context, Entity, MouseButton, MouseDownEvent, Window};
use tracing::Instrument;
use u_forge_core::{
    AppConfig, HybridSearchConfig, KnowledgeGraph, ObjectId,
    queue::InferenceQueue,
    search_hybrid,
};
use u_forge_ui_traits::node_color_for_type;

use crate::selection_model::SelectionModel;
use crate::text_field::{TextFieldView, TextSubmit};

// ── Search mode ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum SearchMode {
    Fts5,
    Semantic,
    Hybrid,
}

// ── Result entry ──────────────────────────────────────────────────────────────

struct SearchResult {
    node_id: ObjectId,
    name: String,
    object_type: String,
}

// ── Search panel ─────────────────────────────────────────────────────────────

pub(crate) struct SearchPanel {
    selection: Entity<SelectionModel>,
    graph: Arc<KnowledgeGraph>,
    query_field: Entity<TextFieldView>,
    mode: SearchMode,
    results: Vec<SearchResult>,
    searching: bool,
    error: Option<String>,
    search_limit: usize,
    inference_queue: Option<InferenceQueue>,
    hq_queue: Option<InferenceQueue>,
    app_config: Arc<AppConfig>,
    tokio_rt: Arc<tokio::runtime::Runtime>,
    #[allow(dead_code)]
    submit_sub: gpui::Subscription,
}

impl SearchPanel {
    pub(crate) fn new(
        selection: Entity<SelectionModel>,
        graph: Arc<KnowledgeGraph>,
        app_config: Arc<AppConfig>,
        tokio_rt: Arc<tokio::runtime::Runtime>,
        cx: &mut Context<Self>,
    ) -> Self {
        let search_limit = app_config.chat.search_limit;
        let query_field = cx.new(|cx| TextFieldView::new(false, "Search nodes...", cx));

        // Trigger search when Enter is pressed in the query field.
        let submit_sub = cx.subscribe(&query_field, |this, _field, _event: &TextSubmit, cx| {
            this.do_search(cx);
        });

        Self {
            selection,
            graph,
            query_field,
            mode: SearchMode::Fts5,
            results: Vec::new(),
            searching: false,
            error: None,
            search_limit,
            inference_queue: None,
            hq_queue: None,
            app_config,
            tokio_rt,
            submit_sub,
        }
    }

    /// Update the InferenceQueue references after Lemonade initializes.
    pub(crate) fn set_queues(&mut self, queue: Option<InferenceQueue>, hq: Option<InferenceQueue>) {
        self.inference_queue = queue;
        self.hq_queue = hq;
    }

    /// Execute a search using the current query and mode.
    pub(crate) fn do_search(&mut self, cx: &mut Context<Self>) {
        let query = self.query_field.read(cx).content.clone();
        if query.trim().is_empty() {
            return;
        }

        // Validate queue availability for modes that need it.
        match self.mode {
            SearchMode::Semantic | SearchMode::Hybrid if self.inference_queue.is_none() => {
                self.error = Some("Lemonade not available — use FTS5".to_string());
                cx.notify();
                return;
            }
            _ => {}
        }

        self.searching = true;
        self.error = None;
        self.results.clear();
        cx.notify();

        let graph = self.graph.clone();
        let mode = self.mode;
        let query_len = query.len();
        let limit = self.search_limit;
        let queue = self.inference_queue.clone();
        let hq_queue = self.hq_queue.clone();
        let app_config = self.app_config.clone();
        let tokio_rt = self.tokio_rt.clone();
        let mode_str = match mode {
            SearchMode::Fts5 => "fts5",
            SearchMode::Semantic => "semantic",
            SearchMode::Hybrid => "hybrid",
        };

        cx.spawn(async move |this, cx| {
            let result: Result<Vec<ObjectId>, anyhow::Error> = cx
                .background_executor()
                .spawn(
                    async move {
                    tokio_rt.block_on(async move {
                        let r: anyhow::Result<Vec<ObjectId>> = match mode {
                            SearchMode::Fts5 => {
                                // FTS5: call directly, no embedding needed.
                                let fts_limit = limit * 4;
                                let raw = graph.search_chunks_fts(&query, fts_limit)?;
                                // Deduplicate by ObjectId, preserving first-seen order.
                                let mut seen = std::collections::HashSet::new();
                                let mut node_ids = Vec::new();
                                for (_, obj_id, _) in raw {
                                    if seen.insert(obj_id) {
                                        node_ids.push(obj_id);
                                        if node_ids.len() >= limit {
                                            break;
                                        }
                                    }
                                }
                                Ok(node_ids)
                            }
                            SearchMode::Semantic => {
                                let q = queue.as_ref().unwrap();
                                // Prefer HQ (4096-dim) embeddings when available — they
                                // were used during bulk indexing if HQ is enabled, so
                                // querying them gives meaningfully better recall.
                                let raw: Vec<(_, ObjectId, _, _)> =
                                    if let Some(hq_q) = hq_queue.as_ref() {
                                        let embedding: Vec<f32> =
                                            hq_q.embed(&query).await?;
                                        graph
                                            .search_chunks_semantic_hq(&embedding, limit * 4)?
                                    } else {
                                        let embedding: Vec<f32> = q.embed(&query).await?;
                                        graph
                                            .search_chunks_semantic(&embedding, limit * 4)?
                                    };
                                let mut seen = std::collections::HashSet::new();
                                let mut node_ids = Vec::new();
                                for (_, obj_id, _, _) in raw {
                                    if seen.insert(obj_id) {
                                        node_ids.push(obj_id);
                                        if node_ids.len() >= limit {
                                            break;
                                        }
                                    }
                                }
                                Ok(node_ids)
                            }
                            SearchMode::Hybrid => {
                                let q = queue.as_ref().unwrap();
                                let cfg = HybridSearchConfig {
                                    alpha: if q.has_embedding() {
                                        app_config.chat.alpha
                                    } else {
                                        0.0
                                    },
                                    fts_limit: limit * 4,
                                    semantic_limit: limit * 4,
                                    rerank: q.has_reranking(),
                                    limit,
                                    hq_semantic_boost: app_config.chat.hq_semantic_boost,
                                };
                                let results =
                                    search_hybrid(&graph, q, hq_queue.as_ref(), &query, &cfg)
                                        .await?;
                                Ok(results.into_iter().map(|r| r.node.id).collect::<Vec<_>>())
                            }
                        };
                        r
                    })
                }
                .instrument(tracing::info_span!("search_kickoff", mode = mode_str, query_len)),
                )
                .await;

            this.update(cx, |panel, cx| {
                panel.searching = false;
                match result {
                    Ok(node_ids) => {
                        // Resolve node names from the graph.
                        panel.results = node_ids
                            .iter()
                            .filter_map(|id| {
                                panel.graph.get_object(*id).ok().flatten().map(|meta| {
                                    SearchResult {
                                        node_id: *id,
                                        name: meta.name,
                                        object_type: meta.object_type,
                                    }
                                })
                            })
                            .collect();
                        if panel.results.is_empty() {
                            panel.error = Some("No results found.".to_string());
                        }
                    }
                    Err(e) => {
                        panel.error = Some(format!("Search error: {e}"));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

// ── Type color helper (same as node panel) ────────────────────────────────────

fn result_type_color(object_type: &str) -> u32 {
    let [r, g, b, _] = node_color_for_type(object_type);
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

// ── Rendering ─────────────────────────────────────────────────────────────────

impl Render for SearchPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected_id = self.selection.read(cx).selected_node_id;
        let has_queue = self.inference_queue.is_some();
        let mode = self.mode;

        let mut panel = div()
            .id("search-panel")
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .h_full()
            .min_h_0()
            .bg(rgb(0x181825))
            .border_r_1()
            .border_color(rgb(0x313244));

        // ── Header ────────────────────────────────────────────────────────────
        panel = panel.child(
            div()
                .id("search-header")
                .flex()
                .items_center()
                .h(px(28.0))
                .px_3()
                .flex_none()
                .border_b_1()
                .border_color(rgb(0x313244))
                .text_color(rgba(0xcdd6f4ff))
                .text_xs()
                .child("SEARCH"),
        );

        // ── Mode selector ─────────────────────────────────────────────────────
        let mode_row = div()
            .id("search-mode-row")
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .h(px(28.0))
            .px_2()
            .gap(px(2.0))
            .border_b_1()
            .border_color(rgb(0x313244))
            .child(
                div()
                    .id("mode-fts5")
                    .flex()
                    .items_center()
                    .px_2()
                    .h(px(20.0))
                    .rounded(px(3.0))
                    .cursor_pointer()
                    .text_xs()
                    .text_color(if mode == SearchMode::Fts5 {
                        rgba(0xcdd6f4ff)
                    } else {
                        rgba(0x6c7086ff)
                    })
                    .when(mode == SearchMode::Fts5, |el| el.bg(rgba(0x45475a88)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                            this.mode = SearchMode::Fts5;
                            cx.notify();
                        }),
                    )
                    .child("FTS5"),
            )
            .child(
                div()
                    .id("mode-semantic")
                    .flex()
                    .items_center()
                    .px_2()
                    .h(px(20.0))
                    .rounded(px(3.0))
                    .text_xs()
                    .text_color(if !has_queue {
                        rgba(0x45475aff)
                    } else if mode == SearchMode::Semantic {
                        rgba(0xcdd6f4ff)
                    } else {
                        rgba(0x6c7086ff)
                    })
                    .when(mode == SearchMode::Semantic && has_queue, |el| {
                        el.bg(rgba(0x45475a88))
                    })
                    .when(has_queue, |el| {
                        el.cursor_pointer().on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                this.mode = SearchMode::Semantic;
                                cx.notify();
                            }),
                        )
                    })
                    .child("Semantic"),
            )
            .child(
                div()
                    .id("mode-hybrid")
                    .flex()
                    .items_center()
                    .px_2()
                    .h(px(20.0))
                    .rounded(px(3.0))
                    .text_xs()
                    .text_color(if !has_queue {
                        rgba(0x45475aff)
                    } else if mode == SearchMode::Hybrid {
                        rgba(0xcdd6f4ff)
                    } else {
                        rgba(0x6c7086ff)
                    })
                    .when(mode == SearchMode::Hybrid && has_queue, |el| {
                        el.bg(rgba(0x45475a88))
                    })
                    .when(has_queue, |el| {
                        el.cursor_pointer().on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                                this.mode = SearchMode::Hybrid;
                                cx.notify();
                            }),
                        )
                    })
                    .child("Hybrid"),
            );

        panel = panel.child(mode_row);

        // ── Query input + Search button ───────────────────────────────────────
        // Field container grows to fill remaining space; search button is fixed width.
        let mut field_container = div().flex().overflow_hidden();
        field_container.style().flex_grow = Some(1.0);
        field_container.style().flex_shrink = Some(1.0);
        field_container.style().flex_basis = Some(relative(0.).into());

        let input_row = div()
            .id("search-input-row")
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(4.0))
            .px_2()
            .py(px(4.0))
            .border_b_1()
            .border_color(rgb(0x313244))
            .child(field_container.child(self.query_field.clone()))
            .child(
                div()
                    .id("search-btn")
                    .flex()
                    .flex_none()
                    .items_center()
                    .px_2()
                    .h(px(24.0))
                    .rounded(px(4.0))
                    .bg(rgb(0x313244))
                    .text_xs()
                    .text_color(rgba(0xcdd6f4ff))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                            this.do_search(cx);
                        }),
                    )
                    .child("Search"),
            );

        panel = panel.child(input_row);

        // ── Status (searching / error) ────────────────────────────────────────
        if self.searching {
            panel = panel.child(
                div()
                    .id("search-status")
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(22.0))
                    .px_3()
                    .text_xs()
                    .text_color(rgba(0xa6adc8ff))
                    .child("Searching…"),
            );
        } else if let Some(err) = &self.error {
            let err = err.clone();
            panel = panel.child(
                div()
                    .id("search-error")
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(22.0))
                    .px_3()
                    .text_xs()
                    .text_color(rgba(0xf38ba8ff))
                    .child(err),
            );
        }

        // ── Results list ──────────────────────────────────────────────────────
        let mut scroll_area = div()
            .id("search-results")
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .min_h_0();
        scroll_area.style().flex_grow = Some(1.0);
        scroll_area.style().flex_shrink = Some(1.0);
        scroll_area.style().flex_basis = Some(relative(0.).into());

        for (idx, result) in self.results.iter().enumerate() {
            let node_id = result.node_id;
            let is_selected = selected_id == Some(node_id);
            let type_color = result_type_color(&result.object_type);
            let display_name = if result.name.len() > 24 {
                let mut s: String = result.name.chars().take(23).collect();
                s.push('…');
                s
            } else {
                result.name.clone()
            };

            scroll_area = scroll_area.child(
                div()
                    .id(("search-result", idx))
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(22.0))
                    .pl(px(8.0))
                    .pr(px(4.0))
                    .flex_none()
                    .gap(px(6.0))
                    .text_xs()
                    .cursor_pointer()
                    .text_color(if is_selected {
                        rgba(0xffffffff)
                    } else {
                        rgba(0xa6adc8ff)
                    })
                    .when(is_selected, |el| el.bg(rgba(0x45475aaa)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                            this.selection.update(cx, |sel, cx| {
                                sel.select_by_id(Some(node_id), cx);
                            });
                        }),
                    )
                    // Colored type dot
                    .child(
                        div()
                            .flex_none()
                            .w(px(6.0))
                            .h(px(6.0))
                            .rounded_full()
                            .bg(gpui::rgb(type_color)),
                    )
                    .child(display_name),
            );
        }

        panel.child(scroll_area)
    }
}
