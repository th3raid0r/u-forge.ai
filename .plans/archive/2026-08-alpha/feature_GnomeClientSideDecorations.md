# Feature Plan: Negotiated Client-Side Window Decorations

> Archived completion brief. Source and current descriptive documentation are
> authoritative where implementation details differ from this checklist.

## Status and target

Implemented and user-validated on supported Linux sessions. GNOME X11 is an
unsupported configuration and has no validation requirement. This completed
the client-decoration release gate alongside `feature_InferenceLifecycle.md`
and `feature_AgentBudgets.md`.

The implementation provides complete native-window behavior on Linux
compositors that delegate window chrome to the application, especially GNOME
on Wayland. Native or server-side decorations are preserved everywhere GPUI
reports them, and the app follows negotiated GPUI decoration mode rather than
desktop-name environment variables.

## Decoration contract

- [x] **CSD-01 — Negotiated mode.** Give the window a stable title and
  application identity, then render application chrome only when
  `Window::window_decorations()` reports client-side decorations. Server-side
  mode must retain the current workspace bounds without duplicate chrome.
- [x] **CSD-02 — Title bar.** Add a theme-aligned title bar with the u-forge
  identity, a draggable region, and minimize, maximize/restore, and close
  controls that reflect the window manager's available controls and active
  state.
- [x] **CSD-03 — Native interactions.** Support drag-to-move, title-bar
  double-click behavior, the native title-bar context menu, and resize hit
  regions on untiled edges and corners through GPUI's window APIs.
- [x] **CSD-04 — Frame geometry.** Apply client insets, borders, corner radii,
  and shadows according to the reported tiling/maximized/fullscreen state.
  Client chrome must not reduce or overlap the existing menu, workspace, or
  status-bar content unexpectedly.
- [x] **CSD-05 — Accessible controls.** Use semantic icons, labels, tooltips,
  focus-visible behavior, and interface-scale tokens. Window controls must work
  by pointer and keyboard without entering the normal workspace focus cycle.
- [x] **CSD-06 — Platform preservation.** Keep macOS, Windows, and Linux
  server-decorated behavior unchanged. X11 is outside the supported
  configuration set; GPUI may still fall back to server-side decorations.

## Tests and acceptance

- Pure tests cover decoration-mode selection and tiling-aware frame geometry.
- GPUI interaction tests cover each window control, drag regions, double-click,
  context-menu invocation, focus behavior, and interface scaling.
- Manual acceptance covers GNOME Wayland at normal, maximized, fullscreen, and
  tiled sizes and at least one server-decorated Linux session to confirm that
  duplicate chrome is never rendered.
- The window remains movable and resizable at every supported scale, and no
  resize hit region intercepts application controls.
- Final verification is `make fmt-check`, `make check`, `make clippy`, and the
  unfiltered `make test`.

## User validation

Completed by the user on 2026-08-09 across the supported Linux configurations.

- [X] GNOME Wayland, floating: one title bar only; centered `u-forge.ai`;
  controls work; empty title-bar space drags; double-click toggles maximize;
  right-click opens the native menu; every free edge/corner resizes.
- [X] GNOME Wayland, maximized, fullscreen, and left/right tiled: chrome and
  resize regions follow the state; application menus, workspace, and status bar
  remain fully usable.
- [X] Settings: left/right placement applies after Save Settings, survives a
  restart, and remains usable at interface sizes 14, 22, and 32.
- GNOME X11: intentionally unsupported; no validation required.
- [X] At least one server-decorated Linux session: no duplicate application
  title bar and no change to workspace bounds.

Validation result: complete on GNOME Wayland and a server-decorated Linux
session. GNOME X11 is out of scope.
