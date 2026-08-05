# Feature Plan: Zed-Style UI Structure and Behavior

## Status and target

Approved, not implemented. Match Zed's panel composition, interaction model,
focus/actions, tabs, menus, and status behavior while retaining u-forge's
palette and identity through semantic tokens. The graph remains a distinct
center item. Zed's new chat-history/archive interface is not in scope.

At implementation start, record one Zed commit in this file and use it for the
entire parity pass. Reimplement behavior for GPUI CE; do not import Zed workspace
crates or copy implementation code.

Primary references:

- `crates/workspace/src/dock.rs` — dock and panel behavior
- `crates/workspace/src/pane.rs` and `item.rs` — items and tabs
- `crates/ui/src/components/tab.rs` — tab presentation contract
- `crates/project_panel/src/project_panel.rs` — focus/actions/flattened tree
- `crates/agent_ui/src/agent_panel.rs` — chat panel toolbar
- `crates/workspace/src/status_bar.rs` — composable status items

## Foundation

- [ ] **UI-01 — Semantic tokens.** Centralize surface, border, text, muted,
  accent, selected, warning, danger, focus, spacing, radius, and typography
  tokens. Seed them with the current u-forge palette.
- [ ] **UI-02 — Component primitives.** Add reusable Label, IconButton, Tab,
  Tooltip, Menu/ContextMenu, Popover, and StatusItem components with consistent
  hover, active, disabled, focus, and action behavior.
- [ ] **UI-03 — Icon policy.** Use a coherent icon set and accessible tooltips;
  remove single-character stand-ins where a component icon is available.
- [ ] **UI-04 — Remove unused generic panel metadata.** Keep
  `u-forge-ui-traits` focused on graph drawing contracts. Panel behavior belongs
  in the GPUI application layer.

## Behavioral dock/workspace

- [ ] **UI-05 — Dock model.** Add UI-internal `PanelId`, `DockPosition`,
  `DockPanel`/type-erased handle, and `DockState`. Own open/active state, focus,
  valid position, min/default size, zoom state, and toggle action centrally.
- [ ] **UI-06 — Canonical composition.** Left dock contains Nodes and Search;
  right dock contains Chat; bottom dock contains Node Editor; Graph is the
  non-closable center workspace item.
- [ ] **UI-07 — Stable mounting.** Keep expensive panels mounted behind clipped
  zero-size outer containers and stable-size cached inner containers. Switching
  left tabs must not cold-mount the inactive panel.
- [ ] **UI-08 — Resizing.** Move resize constraints and double-click reset into
  DockState, including minimum center-workspace size and per-panel defaults.
- [ ] **UI-09 — Persistence.** Atomically store a versioned dock snapshot at
  `${storage.db_path}/workspace-ui.json`. Persist open panels, active left tab,
  sizes, and zoom state. Missing/corrupt state degrades to defaults.

## Focus, actions, and menus

- [ ] **UI-10 — Focus contracts.** Make interactive panels focusable, attach
  key contexts, restore focus on activation, and expose visible focus state.
- [ ] **UI-11 — Keyboard navigation.** Support panel activation, list/tree
  navigation, tab switching/closing, and context-menu invocation by actions.
- [ ] **UI-12 — One action source.** Build native menus, in-app menus,
  shortcuts, enabled state, tooltips, and status toggles from the same actions.
- [ ] **UI-13 — Composable status bar.** Replace hard-coded status-bar branches
  with status items for docks, graph counts, inference/search state, embedding
  progress, and performance overlay.

## Panel adaptation

- [ ] **UI-14 — Nodes panel.** Flatten and virtualize visible groups/nodes;
  implement focus-aware selection, expand/collapse, create/delete actions, and
  context menus. Delete controls must stop row-selection propagation.
- [ ] **UI-15 — Search panel.** Use shared fields/buttons/status components,
  retain owned search tasks, and present structured degradation outcomes from
  `bug_AlphaCorrectness.md`.
- [ ] **UI-16 — Pane/item editor tabs.** Model preview versus pinned items,
  active tab, dirty state, close confirmation, reorder, tooltips, context
  actions, and active-tab scrolling. Preserve schema-driven editor behavior.
- [ ] **UI-17 — Chat toolbar.** Match Zed's agent-panel toolbar semantics for
  title, new chat, maximize/restore, overflow menu, tooltips, model selection,
  and reasoning reload/busy state.
- [ ] **UI-18 — Chat history boundary.** Retain storage and latest-session
  resume. Remove hidden history-list construction and dead navigation state;
  do not add Zed history/archive presentation.
- [ ] **UI-19 — Message rendering.** Preserve per-message entities,
  virtualization, collapse-by-default thinking blocks, and targeted streaming
  invalidation. Treat richer markdown/code/link rendering as a later parity
  increment after dock/focus behavior is stable.
- [ ] **UI-20 — Graph integration.** Keep GraphCanvas's current culling,
  batching, local coordinates, persisted layout, Fit Graph, and graph-specific
  legend. Only its surrounding workspace chrome follows Zed.

## Tests and parity acceptance

- Separate DockState, tab state, menu enablement, and persistence into pure or
  minimally GPUI-coupled reducers with deterministic unit tests.
- Add interaction/smoke coverage for dock activation/focus, stable mounting,
  keyboard actions, delete propagation, dirty close, chat reload busy state,
  corrupt-state fallback, and status items.
- Maintain a manual checklist and screenshots against the pinned Zed commit for
  left/right/bottom docks, focus, resizing, tabs, menus, chat toolbar, and status
  bar. Pixel identity is not required; composition and behavior are.
- Profile panel toggles and resize with the existing perf overlay before and
  after the refactor.
