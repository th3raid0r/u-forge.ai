use std::sync::Arc;

use gpui::{
    App, Context, Entity, FocusHandle, Focusable, ListAlignment, ListState, MouseButton,
    MouseDownEvent, Window, div, list, prelude::*, px, relative, rgb, rgba,
};
use tracing::Instrument;
use u_forge_core::{
    AppConfig, EmbeddingTarget, HybridSearchConfig, KnowledgeGraph, ObjectId, SearchStageOutcomes,
    SearchStageStatus,
    queue::{CancellationToken, InferenceQueue, InferenceQueueBuilder},
    search_hybrid_response_with_cancellation,
};
use u_forge_ui_traits::node_color_for_type;

use crate::selection_model::SelectionModel;
use crate::text_field::{TextFieldView, TextSubmit};
use crate::ui::components::{Button, ButtonStyle, Label, LabelSize, LabelTone, Tab, Tooltip};
use crate::ui::theme::UiTheme;

// ── Search mode ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum SearchMode {
    Fts5,
    Semantic,
    Hybrid,
}

// ── Result entry ──────────────────────────────────────────────────────────────

#[derive(Clone)]
struct SearchResult {
    node_id: ObjectId,
    name: String,
    object_type: String,
}

struct CompletedSearch {
    node_ids: Vec<ObjectId>,
    outcomes: SearchStageOutcomes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SearchPanelStatus {
    Searching,
    Degraded(String),
    Failed(String),
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
        stages.push("words");
    }
    let standard = outcomes.standard_semantic.status;
    let hq = outcomes.high_quality_semantic.status;
    let semantic_failed = standard == SearchStageStatus::Failed || hq == SearchStageStatus::Failed;
    let no_semantic_lane_applied =
        standard != SearchStageStatus::Applied && hq != SearchStageStatus::Applied;
    if semantic_failed || (no_semantic_lane_applied && (degraded(standard) || degraded(hq))) {
        stages.push("meaning");
    }
    if degraded(outcomes.reranking.status) {
        stages.push("ordering");
    }
    if stages.is_empty() {
        None
    } else {
        Some(format!("Search limited · {}", stages.join(", ")))
    }
}

fn stage_status_label(status: SearchStageStatus) -> &'static str {
    match status {
        SearchStageStatus::Applied => "used",
        SearchStageStatus::IntentionallySkipped => "not needed",
        SearchStageStatus::Unavailable => "unavailable",
        SearchStageStatus::Failed => "could not complete",
    }
}

fn stage_detail(label: &str, status: SearchStageStatus, diagnostic: Option<&str>) -> String {
    let mut detail = format!("{label}: {}", stage_status_label(status));
    if let Some(diagnostic) = diagnostic {
        detail.push_str(" — ");
        detail.push_str(diagnostic);
    }
    detail
}

fn degradation_detail(outcomes: &SearchStageOutcomes) -> String {
    let semantic_status = if outcomes.standard_semantic.status == SearchStageStatus::Applied
        || outcomes.high_quality_semantic.status == SearchStageStatus::Applied
    {
        SearchStageStatus::Applied
    } else if outcomes.standard_semantic.status == SearchStageStatus::Failed
        || outcomes.high_quality_semantic.status == SearchStageStatus::Failed
    {
        SearchStageStatus::Failed
    } else if outcomes.standard_semantic.status == SearchStageStatus::Unavailable
        || outcomes.high_quality_semantic.status == SearchStageStatus::Unavailable
    {
        SearchStageStatus::Unavailable
    } else {
        SearchStageStatus::IntentionallySkipped
    };
    let semantic_diagnostic = [&outcomes.standard_semantic, &outcomes.high_quality_semantic]
        .into_iter()
        .find(|outcome| outcome.status == semantic_status)
        .and_then(|outcome| outcome.diagnostic.as_deref());

    [
        stage_detail(
            "Words",
            outcomes.fts.status,
            outcomes.fts.diagnostic.as_deref(),
        ),
        stage_detail("Meaning", semantic_status, semantic_diagnostic),
        stage_detail(
            "Result ordering",
            outcomes.reranking.status,
            outcomes.reranking.diagnostic.as_deref(),
        ),
    ]
    .join("\n")
}

// ── Search panel ─────────────────────────────────────────────────────────────

pub(crate) struct SearchPanel {
    focus: FocusHandle,
    selection: Entity<SelectionModel>,
    graph: Arc<KnowledgeGraph>,
    query_field: Entity<TextFieldView>,
    mode: SearchMode,
    results: Vec<SearchResult>,
    results_list: ListState,
    searching: bool,
    empty: bool,
    error: Option<String>,
    stage_outcomes: Option<SearchStageOutcomes>,
    degradation_hint: Option<String>,
    /// Monotonically identifies the latest requested search so a slower prior
    /// request cannot overwrite newer results.
    search_generation: u64,
    /// Retaining the task lets a new search cancel its predecessor promptly.
    search_task: Option<gpui::Task<()>>,
    search_cancellation: Option<CancellationToken>,
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
            mode: SearchMode::Hybrid,
            results: Vec::new(),
            results_list: ListState::new(0, ListAlignment::Top, px(22.0)),
            searching: false,
            empty: false,
            error: None,
            stage_outcomes: None,
            degradation_hint: None,
            search_generation: 0,
            search_task: None,
            search_cancellation: None,
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

    pub(crate) fn status(&self) -> Option<SearchPanelStatus> {
        if self.searching {
            Some(SearchPanelStatus::Searching)
        } else if let Some(error) = &self.error {
            Some(SearchPanelStatus::Failed(error.clone()))
        } else {
            self.degradation_hint
                .clone()
                .map(SearchPanelStatus::Degraded)
        }
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
            self.error =
                Some("Meaning search is not ready. Try Words or Best Match instead.".to_string());
            cx.notify();
            return;
        }

        self.searching = true;
        self.empty = false;
        self.error = None;
        self.stage_outcomes = None;
        self.degradation_hint = None;
        self.results.clear();
        self.results_list.reset(0);
        self.search_generation = self.search_generation.wrapping_add(1);
        let generation = self.search_generation;
        if let Some(previous) = self.search_cancellation.take() {
            previous.supersede();
        }
        self.search_task.take();
        let cancellation = CancellationToken::new();
        self.search_cancellation = Some(cancellation.clone());
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
                            let response = search_hybrid_response_with_cancellation(
                                &graph,
                                queue,
                                hq_queue.as_ref(),
                                &query,
                                &config,
                                cancellation,
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
                panel.search_cancellation = None;
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
                        panel.results_list.reset(panel.results.len());
                        panel.empty = panel.results.is_empty();
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

impl Drop for SearchPanel {
    fn drop(&mut self) {
        if let Some(cancellation) = self.search_cancellation.take() {
            cancellation.cancel();
        }
        self.search_task.take();
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
        let theme = *UiTheme::get(cx);
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
                .h(theme.metrics.panel_header_height)
                .px_3()
                .flex_none()
                .border_b_1()
                .border_color(rgb(0x313244))
                .text_color(rgba(0xcdd6f4ff))
                .text_base()
                .child("Search"),
        );

        // ── Mode selector ─────────────────────────────────────────────────────
        let words = cx.weak_entity();
        let meaning = cx.weak_entity();
        let best_match = cx.weak_entity();
        let mode_row = div()
            .id("search-mode-row")
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .h(theme.metrics.control_height)
            .px_2()
            .gap(px(2.0))
            .border_b_1()
            .border_color(theme.colors.border_subtle)
            .child(
                Tab::new("mode-words", "Words", mode == SearchMode::Fts5)
                    .tooltip("Find exact words and phrases. This always works offline.")
                    .on_click(move |_, _, cx| {
                        words
                            .update(cx, |panel, cx| {
                                panel.mode = SearchMode::Fts5;
                                cx.notify();
                            })
                            .ok();
                    }),
            )
            .child(
                Tab::new("mode-meaning", "Meaning", mode == SearchMode::Semantic)
                    .disabled(!semantic_available)
                    .tooltip(if semantic_available {
                        "Find related ideas even when they use different words."
                    } else {
                        "Meaning search becomes available after AI search is set up."
                    })
                    .on_click(move |_, _, cx| {
                        meaning
                            .update(cx, |panel, cx| {
                                panel.mode = SearchMode::Semantic;
                                cx.notify();
                            })
                            .ok();
                    }),
            )
            .child(
                Tab::new("mode-best-match", "Best Match", mode == SearchMode::Hybrid)
                    .tooltip("Combine exact words and related ideas, using what is available.")
                    .on_click(move |_, _, cx| {
                        best_match
                            .update(cx, |panel, cx| {
                                panel.mode = SearchMode::Hybrid;
                                cx.notify();
                            })
                            .ok();
                    }),
            );

        panel = panel.child(mode_row);

        // ── Query input + Search button ───────────────────────────────────────
        // Field container grows to fill remaining space; search button is fixed width.
        let mut field_container = div().flex().overflow_hidden();
        field_container.style().flex_grow = Some(1.0);
        field_container.style().flex_shrink = Some(1.0);
        field_container.style().flex_basis = Some(relative(0.).into());

        let search = cx.weak_entity();
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
            .border_color(theme.colors.border_subtle)
            .child(field_container.child(self.query_field.clone()))
            .child(
                Button::new("search-btn", "Search")
                    .style(ButtonStyle::Filled)
                    .tooltip("Search the world (Enter)")
                    .on_click(move |_, _, cx| {
                        search.update(cx, |panel, cx| panel.do_search(cx)).ok();
                    }),
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
                    .child(
                        Label::new("Searching…")
                            .size(LabelSize::Small)
                            .tone(LabelTone::Muted),
                    ),
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
                    .child(
                        Label::new(err)
                            .size(LabelSize::Small)
                            .tone(LabelTone::Danger),
                    ),
            );
        } else if self.empty {
            panel = panel.child(
                div()
                    .id("search-empty")
                    .flex()
                    .flex_none()
                    .items_center()
                    .min_h(px(22.0))
                    .px_3()
                    .child(
                        Label::new("No matches. Try fewer or different words.")
                            .size(LabelSize::Small)
                            .tone(LabelTone::Muted),
                    ),
            );
        } else if let Some(hint) = &self.degradation_hint {
            let detail = self
                .stage_outcomes
                .as_ref()
                .map(degradation_detail)
                .unwrap_or_else(|| hint.clone());
            panel = panel.child(
                div()
                    .id("search-degradation")
                    .flex()
                    .flex_none()
                    .items_center()
                    .min_h(px(22.0))
                    .px_3()
                    .child(
                        Label::new(hint.clone())
                            .size(LabelSize::Small)
                            .tone(LabelTone::Warning),
                    )
                    .tooltip(Tooltip::text(detail)),
            );
        }

        // ── Results list ──────────────────────────────────────────────────────
        let entity = cx.entity().clone();
        let mut result_rows = list(
            self.results_list.clone(),
            move |idx, _window, cx: &mut App| {
                let panel = entity.read(cx);
                let Some(result) = panel.results.get(idx).cloned() else {
                    return div().into_any_element();
                };
                let node_id = result.node_id;
                let is_selected = panel.selection.read(cx).selected_node_id == Some(node_id);
                let type_color = result_type_color(&result.object_type);
                let display_name = if result.name.chars().count() > 24 {
                    let mut name = result.name.chars().take(23).collect::<String>();
                    name.push('…');
                    name
                } else {
                    result.name
                };
                let select_entity = entity.clone();

                div()
                    .id(("search-result", idx))
                    .flex()
                    .flex_row()
                    .items_center()
                    .h(px(22.0))
                    .pl(px(8.0))
                    .pr(px(4.0))
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
                        move |_event: &MouseDownEvent, window, cx: &mut App| {
                            select_entity.update(cx, |panel, cx| {
                                panel.focus.focus(window);
                                panel.selection.update(cx, |selection, cx| {
                                    selection.select_by_id(Some(node_id), cx);
                                });
                            });
                        },
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(6.0))
                            .h(px(6.0))
                            .rounded_full()
                            .bg(gpui::rgb(type_color)),
                    )
                    .child(display_name)
                    .into_any_element()
            },
        );
        result_rows.style().flex_grow = Some(1.0);
        result_rows.style().flex_shrink = Some(1.0);
        result_rows.style().flex_basis = Some(relative(0.0).into());

        let mut scroll_area = div()
            .id("search-results")
            .flex()
            .flex_col()
            .min_h_0()
            .overflow_hidden()
            .child(result_rows);
        scroll_area.style().flex_grow = Some(1.0);
        scroll_area.style().flex_shrink = Some(1.0);
        scroll_area.style().flex_basis = Some(relative(0.).into());
        panel.child(scroll_area)
    }
}

#[cfg(test)]
mod tests {
    use super::{degradation_detail, degradation_hint};
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
            Some("Search limited · meaning")
        );
        assert_eq!(
            degradation_detail(&outcomes),
            "Words: used\nMeaning: could not complete — safe detail\nResult ordering: not needed"
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
