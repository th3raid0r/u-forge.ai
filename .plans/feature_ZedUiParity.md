# Feature Plan: Zed-Structured, DM-Oriented Workspace

## Status and target

Approved and in implementation. Use Zed commit
`381953d44897c53c4d252ae30620bafaa7d060b7` for the entire parity pass.
Reimplement its composition and behavior for GPUI CE; do not import Zed
workspace crates or copy Zed implementation code or assets.

Match Zed's panel composition, interaction model, focus/actions, tabs, menus,
and status behavior while retaining u-forge's palette and identity. Adapt the
experience for non-technical DMs through plain-language labels, guided flows,
explicit content saves, and progressive disclosure of technical controls.
Zed's new chat archive interface is not in scope.

The permanent center workspace is **World Canvas**. Its current view is
**Connections** (`GraphCanvas`). Future, separately planned tabs may add:

- **Timeline** — temporal nodes and relationship lines to related world items.
- **Map** — a user-supplied world/galaxy/system image with affixed location
  nodes and derived placement for entities connected by `located_in`.

This pass must not implement those future views, but naming, item identity, and
tab chrome must not assume that Connections is the only possible center view.

Primary Zed references:

- `crates/workspace/src/dock.rs` — dock and panel behavior
- `crates/workspace/src/pane.rs` and `item.rs` — items and tabs
- `crates/ui/src/components/tab.rs` — tab presentation contract
- `crates/project_panel/src/project_panel.rs` — focus/actions/flattened tree
- `crates/agent_ui/src/agent_panel.rs` — assistant panel toolbar
- `crates/workspace/src/status_bar.rs` — composable status items

## Foundation

- [ ] **UI-01 — Semantic tokens.** Centralize application/panel/elevated/input
  surfaces; borders; primary/muted/disabled text; accent, selected, success,
  warning, danger, and focus colors; spacing, radii, control heights, and
  typography. Seed them with the current u-forge palette. Graph type colors
  remain graph-specific.
- [ ] **UI-02 — Component primitives.** Add reusable Label, Button,
  IconButton, Tab, Tooltip, Menu/ContextMenu, Popover, Dialog, and StatusItem
  components with consistent hover, pressed, disabled, selected, focus,
  keyboard, action, and dismissal behavior.
- [ ] **UI-03 — Icon policy.** Bundle a coherent original monochrome SVG set
  through GPUI's asset source. Replace character stand-ins and give every
  icon-only control an accessible label and tooltip.
- [ ] **UI-04 — GPUI-local panel contracts.** Remove generic panel metadata
  from `u-forge-ui-traits`, leaving it focused on graph drawing. Add GPUI-local
  `PanelId`, `DockPosition`, descriptors, focus contracts, and erased handles.
- [x] **UI-05 — Progressive settings.** Add `show_advanced_controls` to UI
  configuration and a Settings dialog for font size and advanced disclosure.
  Preserve low-level TOML compatibility; do not expose embedding dimensions,
  model load parameters, queue weights, sampling internals, or semantic boost
  values as ordinary UI settings.

## Behavioral dock and World Canvas

- [ ] **UI-06 — Dock reducer.** Add a pure/minimally GPUI-coupled `DockState`
  owning open/active state, focus intent, canonical valid position, size,
  reset, zoom, and panel toggle behavior.
- [x] **UI-07 — Canonical composition.** Left contains World and Search; right
  contains Assistant; bottom contains Details; World Canvas is the permanent
  non-closable center item. First launch opens World + World Canvas. Selection
  opens Details without stealing focus; Assistant opens explicitly.
- [x] **UI-08 — Stable mounting.** Keep expensive panels mounted behind
  clipped zero-size outer containers and stable-size cached inner containers.
  Switching World/Search or toggling a dock must not cold-mount inactive views.
- [x] **UI-09 — Resizing and zoom.** Centralize constraints and double-click
  reset. Defaults/minima are left 280/220 px, right 360/300 px, bottom 320/200
  px, with at least 360 x 240 px retained for World Canvas. One dock panel may
  zoom over the workspace body while menu/status chrome remains visible.
- [x] **UI-10 — Persistence.** Atomically store versioned state at
  `${storage.db_path}/workspace-ui.json`: open/active panels, dock sizes, and
  zoomed panel. Missing, corrupt, or unsupported state degrades to defaults.
  Persist graph node positions at drag completion; Save All also flushes them.

## Focus, actions, menus, and status

- [ ] **UI-11 — Focus contracts.** Make panels focusable, attach key contexts,
  preserve last descendant focus, expose focus-visible state, and restore focus
  predictably after closing panels, menus, dialogs, and tabs.
- [x] **UI-12 — Keyboard navigation.** Support dock activation, F6 region
  traversal, list/tree navigation, expand/collapse, Details tab switching and
  closing, and Shift+F10 context-menu invocation through typed actions.
- [ ] **UI-13 — One action source.** Generate native menus, in-app menus,
  shortcuts, enabled state, advanced visibility, tooltips, context entries,
  and status toggles from shared action descriptors. `Ctrl+S` saves the active
  Details item; `Ctrl+Shift+S` saves all dirty items.
- [x] **UI-14 — Composable status bar.** Register status items for docks,
  World Canvas counts, data activity, search/inference state, embedding
  progress, and advanced-only performance diagnostics. Compact or truncate
  low-priority content cleanly at narrow widths.

## Guided workflows and panel adaptation

- [x] **UI-15 — World panel.** Flatten and virtualize ordered group/item rows;
  implement focus-aware selection, expand/collapse, context menus, and
  confirmed deletion. Delete controls stop row-selection propagation. New
  world items start as in-memory pinned Details drafts and reach storage only
  through explicit Save.
- [ ] **UI-16 — Guided import/setup.** Make Import World the normal data flow
  against authoritative loaded schemas, with plain-language validation and a
  structured completion summary. Keep raw schema maintenance advanced. Make
  recommended Lemonade setup the normal path and backend matrices advanced.
- [x] **UI-17 — Search panel.** Use shared fields/buttons/status components,
  retain owned search tasks, virtualize results, and present structured
  degradation outcomes from `bug_AlphaCorrectness.md` with concise hints and
  detailed tooltips.
- [ ] **UI-18 — Details tabs and saving.** Model preview versus pinned items,
  active tab, dirty state, reorder, tooltips, context actions, and active-tab
  scrolling. First edit pins a preview; dirty/pinned tabs are never replaced.
  Add visible Save Changes and Discard Changes controls plus Save/Discard/Cancel
  confirmation on dirty close. Rename visible Edges UI to Relationships and
  block saves with incomplete relationships rather than silently dropping them.
- [ ] **UI-19 — Assistant toolbar.** Match Zed's toolbar semantics for title,
  new conversation, maximize/restore, overflow, tooltips, friendly model
  selection, Think Longer, and explicit reload/busy/failure states. Conflicting
  actions are disabled during streaming or reload rather than silently ignored.
- [ ] **UI-20 — Conversation boundary.** Retain storage, latest-session resume,
  and the current lightweight new/switch/delete selector. Do not add Zed's
  archive presentation. Preserve per-message entities, virtualization,
  collapsed thinking/tool blocks, and targeted streaming invalidation; richer
  markdown/code/link rendering is a later increment.
- [x] **UI-21 — World Canvas integration.** Rename visible Graph chrome to
  World Canvas / Connections while preserving GraphCanvas culling, batching,
  local coordinates, persisted layout, Fit Graph, selection, and legend. Add
  only the minimum center-item/tab boundary needed to avoid baking in a
  single-view assumption; do not implement Timeline or Map.

## Tests and parity acceptance

- Pure tests cover dock transitions, focus intents, resize clamping/reset,
  zoom exclusivity, persistence, flattened World navigation, Details preview
  and dirty-close decisions, menu enablement, and advanced visibility.
- GPUI interaction tests cover adaptive first launch, dock focus restoration,
  stable mounting, keyboard/context menus, delete propagation, explicit save
  and draft discard, assistant reload busy state, corrupt-state fallback,
  settings disclosure, and status items.
- Capture baseline and post-change 60-frame measurements for warmed panel
  toggles and resize. Confirm streaming invalidates only the target message and
  the same local scenario has no greater than a 10% average-frame regression.
- Maintain screenshots and a manual checklist against the pinned Zed commit for
  docks, focus, resizing/reset, zoom, tabs, menus, assistant toolbar, and status
  bar. Composition and behavior are required; pixel identity is not.
- Final verification is `make fmt-check`, `make check`, `make clippy`, and the
  unfiltered `make test` without requiring Lemonade Server.

## Explicit exclusions

- Timeline rendering and temporal relationship visualization.
- User-image maps, location pinning, and `located_in`-derived map placement.
- Agent-following viewport behavior, richer markdown, chat archive UI, theme
  switching, new import formats, and user-movable panels.
