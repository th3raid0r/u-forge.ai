use std::sync::Arc;

use gpui::{
    Context, Entity, FocusHandle, Focusable, MouseButton, MouseDownEvent, Window, div, prelude::*,
    px, relative, rgb, rgba,
};
use tracing::Instrument;
use u_forge_core::{
    AppConfig, EmbeddingTarget, HybridSearchConfig, KnowledgeGraph, ObjectId, SearchStageOutcomes,
    SearchStageStatus,
    queue::{InferenceQueue, InferenceQueueBuilder},
    search_hybrid_response,
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

struct CompletedSearch {
    node_ids: Vec<ObjectId>,
    outcomes: SearchStageOutcomes,
}

fn degradation_hint(outcomes: &SearchStageOutcomes) -> Option<String> {
    let degraded = |status| {
        matches!(
            status,
            SearchStageStatus::Unavailable | SearchStageStatus::Failed
        )
    };
    let mut stages = Vec::new();
    if degraded(outcomes.fts.status) {
        stages.push("FTS5");
    }
    let standard = outcomes.standard_semantic.status;
    let hq = outcomes.high_quality_semantic.status;
    let semantic_failed = standard == SearchStageStatus::Failed || hq == SearchStageStatus::Failed;
    let no_semantic_lane_applied =
        standard != SearchStageStatus::Applied && hq != SearchStageStatus::Applied;
    if semantic_failed || (no_semantic_lane_applied && (degraded(standard) || degraded(hq))) {
        stages.push("semantic");
    }
    if degraded(outcomes.reranking.status) {
        stages.push("reranking");
    }
    if stages.is_empty() {
        None
    } else {
        Some(format!("Results degraded · {}", stages.join(", ")))
    }
}

// ── Search panel ─────────────────────────────────────────────────────────────

pub(crate) struct SearchPanel {
    focus: FocusHandle,
    selection: Entity<SelectionModel>,
    graph: Arc<KnowledgeGraph>,
    query_field: Entity<TextFieldView>,
    mode: SearchMode,
    results: Vec<SearchResult>,
    searching: bool,
    error: Option<String>,
    stage_outcomes: Option<SearchStageOutcomes>,
    degradation_hint: Option<String>,
    /// Monotonically identifies the latest requested search so a slower prior
    /// request cannot overwrite newer results.
    search_generation: u64,
    /// Retaining the task lets a new search cancel its predecessor promptly.
    search_task: Option<gpui::Task<()>>,
    search_limit: usize,
    inference_queue: Option<InferenceQueue>,
    hq_queue: Option<InferenceQueue>,
    compatible_semantic_lane: Option<EmbeddingTarget>,
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
            focus: cx.focus_handle(),
            selection,
            graph,
            query_field,
            mode: SearchMode::Fts5,
            results: Vec::new(),
            searching: false,
            error: None,
            stage_outcomes: None,
            degradation_hint: None,
            search_generation: 0,
            search_task: None,
            search_limit,
            inference_queue: None,
            hq_queue: None,
            compatible_semantic_lane: None,
            app_config,
            tokio_rt,
            submit_sub,
        }
    }

    /// Update the InferenceQueue references after Lemonade initializes.
    pub(crate) fn set_queues(&mut self, queue: Option<InferenceQueue>, hq: Option<InferenceQueue>) {
        self.inference_queue = queue;
        self.hq_queue = hq;
        self.compatible_semantic_lane = self.resolve_compatible_semantic_lane();
    }

    fn resolve_compatible_semantic_lane(&self) -> Option<EmbeddingTarget> {
        let lanes = [
            (EmbeddingTarget::HighQuality, self.hq_queue.as_ref()),
            (EmbeddingTarget::Standard, self.inference_queue.as_ref()),
        ];
        lanes.into_iter().find_map(|(target, queue)| {
            let queue = queue?;
            if !queue.has_embedding() {
                return None;
            }
            let fingerprint = queue.embedding_space_fingerprint()?;
            match self.graph.ensure_embedding_space(target, fingerprint) {
                Ok(()) => Some(target),
                Err(error) => {
                    tracing::warn!(?target, %error, "Search embedding lane is incompatible");
                    None
                }
            }
        })
    }

    /// Execute a search using the current query and mode.
    pub(crate) fn do_search(&mut self, cx: &mut Context<Self>) {
        let query = self.query_field.read(cx).content.clone();
        if query.trim().is_empty() {
            return;
        }

        // Validate queue availability for modes that need it.
        if self.mode == SearchMode::Semantic && self.compatible_semantic_lane.is_none() {
            self.error = Some("No compatible semantic index — use FTS5 or Hybrid".to_string());
            cx.notify();
            return;
        }

        self.searching = true;
        self.error = None;
        self.stage_outcomes = None;
        self.degradation_hint = None;
        self.results.clear();
        self.search_generation = self.search_generation.wrapping_add(1);
        let generation = self.search_generation;
        self.search_task.take();
        cx.notify();

        let graph = self.graph.clone();
        let mode = self.mode;
        let query_len = query.len();
        let limit = self.search_limit;
        let queue = self.inference_queue.clone();
        let hq_queue = self.hq_queue.clone();
        let semantic_available = self.compatible_semantic_lane.is_some();
        let app_config = self.app_config.clone();
        let tokio_rt = self.tokio_rt.clone();
        let mode_str = match mode {
            SearchMode::Fts5 => "fts5",
            SearchMode::Semantic => "semantic",
            SearchMode::Hybrid => "hybrid",
        };

        let task = cx.spawn(async move |this, cx| {
            let result: Result<CompletedSearch, anyhow::Error> = cx
                .background_executor()
                .spawn(
                    async move {
                        tokio_rt.block_on(async move {
                            let fallback_queue;
                            let queue = match queue.as_ref() {
                                Some(queue) => queue,
                                None => {
                                    fallback_queue = InferenceQueueBuilder::new().build();
                                    &fallback_queue
                                }
                            };
                            let (alpha, rerank) = match mode {
                                SearchMode::Fts5 => (0.0, false),
                                SearchMode::Semantic => (1.0, false),
                                SearchMode::Hybrid => (app_config.chat.alpha, true),
                            };
                            debug_assert!(mode != SearchMode::Semantic || semantic_available);
                            let config = HybridSearchConfig {
                                alpha,
                                fts_limit: limit * 4,
                                semantic_limit: limit * 4,
                                rerank,
                                limit,
                                hq_semantic_boost: app_config.chat.hq_semantic_boost,
                            };
                            let response = search_hybrid_response(
                                &graph,
                                queue,
                                hq_queue.as_ref(),
                                &query,
                                &config,
                            )
                            .await?;
                            Ok(CompletedSearch {
                                node_ids: response
                                    .results
                                    .into_iter()
                                    .map(|result| result.node.id)
                                    .collect(),
                                outcomes: response.outcomes,
                            })
                        })
                    }
                    .instrument(tracing::info_span!(
                        "search_kickoff",
                        mode = mode_str,
                        query_len
                    )),
                )
                .await;

            this.update(cx, |panel, cx| {
                if panel.search_generation != generation {
                    return;
                }
                panel.searching = false;
                match result {
                    Ok(completed) => {
                        panel.degradation_hint = degradation_hint(&completed.outcomes);
                        if panel.degradation_hint.is_some() {
                            tracing::warn!(
                                outcomes = ?completed.outcomes,
                                "Search completed with degraded stages"
                            );
                        }
                        panel.stage_outcomes = Some(completed.outcomes);
                        // Resolve node names from the graph.
                        panel.results = completed
                            .node_ids
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
        });
        self.search_task = Some(task);
    }
}

impl Focusable for SearchPanel {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

// ── Type color helper (same as node panel) ────────────────────────────────────

fn result_type_color(object_type: &str) -> u32 {
    let [r, g, b, _] = node_color_for_type(object_type);
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

// ── Rendering ─────────────────────────────────────────────────────────────────

impl Render for SearchPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected_id = self.selection.read(cx).selected_node_id;
        let panel_focused = self.focus.contains_focused(window, cx);
        let semantic_available = self.compatible_semantic_lane.is_some();
        let mode = self.mode;

        let mut panel = div()
            .id("search-panel")
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .h_full()
            .min_h_0()
            .key_context("SearchPanel")
            .track_focus(&self.focus)
            .bg(rgb(0x181825))
            .border_r_1()
            .border_color(rgb(0x313244))
            .when(panel_focused, |panel| {
                panel.border_1().border_color(rgba(0xb4befeff))
            });

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
                .text_base()
                .child("Search"),
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
                    .text_base()
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
                    .text_base()
                    .text_color(if !semantic_available {
                        rgba(0x45475aff)
                    } else if mode == SearchMode::Semantic {
                        rgba(0xcdd6f4ff)
                    } else {
                        rgba(0x6c7086ff)
                    })
                    .when(mode == SearchMode::Semantic && semantic_available, |el| {
                        el.bg(rgba(0x45475a88))
                    })
                    .when(semantic_available, |el| {
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
                    .text_base()
                    .text_color(if mode == SearchMode::Hybrid {
                        rgba(0xcdd6f4ff)
                    } else {
                        rgba(0x6c7086ff)
                    })
                    .when(mode == SearchMode::Hybrid, |el| el.bg(rgba(0x45475a88)))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                            this.mode = SearchMode::Hybrid;
                            cx.notify();
                        }),
                    )
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
                    .text_base()
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
                    .text_base()
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
                    .text_base()
                    .text_color(rgba(0xf38ba8ff))
                    .child(err),
            );
        } else if let Some(hint) = &self.degradation_hint {
            panel = panel.child(
                div()
                    .id("search-degradation")
                    .flex()
                    .flex_none()
                    .items_center()
                    .min_h(px(22.0))
                    .px_3()
                    .text_xs()
                    .text_color(rgba(0xf9e2afff))
                    .child(hint.clone()),
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
                    .text_base()
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

#[cfg(test)]
mod tests {
    use super::degradation_hint;
    use u_forge_core::{SearchStageOutcome, SearchStageOutcomes, SearchStageStatus};

    fn outcome(status: SearchStageStatus, diagnostic: Option<&str>) -> SearchStageOutcome {
        SearchStageOutcome {
            status,
            diagnostic: diagnostic.map(str::to_string),
        }
    }

    #[test]
    fn degradation_hint_lists_only_unavailable_or_failed_stages() {
        let outcomes = SearchStageOutcomes {
            fts: outcome(SearchStageStatus::Applied, None),
            standard_semantic: outcome(SearchStageStatus::Failed, Some("safe detail")),
            high_quality_semantic: outcome(SearchStageStatus::Unavailable, Some("safe detail")),
            reranking: outcome(SearchStageStatus::IntentionallySkipped, None),
        };

        assert_eq!(
            degradation_hint(&outcomes).as_deref(),
            Some("Results degraded · semantic")
        );
    }

    #[test]
    fn degradation_hint_is_absent_for_applied_and_intentional_stages() {
        let outcomes = SearchStageOutcomes {
            fts: outcome(SearchStageStatus::Applied, None),
            standard_semantic: outcome(SearchStageStatus::IntentionallySkipped, None),
            high_quality_semantic: outcome(SearchStageStatus::IntentionallySkipped, None),
            reranking: outcome(SearchStageStatus::Applied, None),
        };

        assert_eq!(degradation_hint(&outcomes), None);
    }

    #[test]
    fn compatible_hq_lane_prevents_false_semantic_degradation() {
        let outcomes = SearchStageOutcomes {
            fts: outcome(SearchStageStatus::IntentionallySkipped, None),
            standard_semantic: outcome(SearchStageStatus::Unavailable, Some("safe detail")),
            high_quality_semantic: outcome(SearchStageStatus::Applied, None),
            reranking: outcome(SearchStageStatus::IntentionallySkipped, None),
        };

        assert_eq!(degradation_hint(&outcomes), None);
    }
}
