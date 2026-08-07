//! Deterministic workspace dock state.
//!
//! This reducer intentionally contains no GPUI entities. Rendering and focus
//! execution consume its state, while transitions and persistence can be
//! tested without constructing a window.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::panel_contracts::{DockPosition, PanelDescriptor, PanelId};

pub(crate) const RESIZE_HANDLE_SIZE: f32 = 6.0;
pub(crate) const MIN_WORLD_CANVAS_WIDTH: f32 = 360.0;
pub(crate) const MIN_WORLD_CANVAS_HEIGHT: f32 = 240.0;
const WORKSPACE_SNAPSHOT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DockFocusIntent {
    Panel(PanelId),
    WorldCanvas,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceSnapshotV1 {
    version: u32,
    open_panels: Vec<PanelId>,
    active_left: PanelId,
    left_size: f32,
    right_size: f32,
    bottom_size: f32,
    zoomed_panel: Option<PanelId>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct DockSlot {
    open: bool,
    active: PanelId,
    size: f32,
}

impl DockSlot {
    fn new(open: bool, active: PanelId) -> Self {
        Self {
            open,
            active,
            size: PanelDescriptor::for_panel(active).default_size,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DockState {
    left: DockSlot,
    right: DockSlot,
    bottom: DockSlot,
    zoomed: Option<PanelId>,
}

impl Default for DockState {
    fn default() -> Self {
        Self {
            left: DockSlot::new(true, PanelId::World),
            right: DockSlot::new(false, PanelId::Assistant),
            bottom: DockSlot::new(false, PanelId::Details),
            zoomed: None,
        }
    }
}

impl DockState {
    pub(crate) fn state_path(storage_path: &Path) -> PathBuf {
        storage_path.join("workspace-ui.json")
    }

    pub(crate) fn load(path: &Path) -> Self {
        match Self::try_load(path) {
            Ok(state) => state,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "Workspace UI state ignored");
                Self::default()
            }
        }
    }

    fn try_load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading workspace state: {}", path.display()))?;
        let snapshot: WorkspaceSnapshotV1 = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing workspace state: {}", path.display()))?;
        if snapshot.version != WORKSPACE_SNAPSHOT_VERSION {
            anyhow::bail!("unsupported workspace state version {}", snapshot.version);
        }
        if !matches!(snapshot.active_left, PanelId::World | PanelId::Search) {
            anyhow::bail!("invalid active left panel");
        }

        let mut state = Self::default();
        state.left.active = snapshot.active_left;
        state.left.open = snapshot
            .open_panels
            .iter()
            .any(|panel| panel.position() == DockPosition::Left);
        state.right.open = snapshot.open_panels.contains(&PanelId::Assistant);
        state.bottom.open = snapshot.open_panels.contains(&PanelId::Details);
        state.left.size = valid_size(snapshot.left_size, PanelId::World);
        state.right.size = valid_size(snapshot.right_size, PanelId::Assistant);
        state.bottom.size = valid_size(snapshot.bottom_size, PanelId::Details);
        if let Some(panel) = snapshot.zoomed_panel
            && state.is_panel_active(panel)
        {
            state.zoomed = Some(panel);
        }
        Ok(state)
    }

    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        let snapshot = self.snapshot();
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("workspace state path has no parent"))?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating workspace state directory: {}", parent.display()))?;
        let bytes = serde_json::to_vec_pretty(&snapshot)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(&bytes)?;
        temporary.as_file_mut().sync_all()?;
        temporary
            .persist(path)
            .map_err(|error| error.error)
            .with_context(|| format!("persisting workspace state: {}", path.display()))?;
        Ok(())
    }

    fn snapshot(&self) -> WorkspaceSnapshotV1 {
        let mut open_panels = Vec::with_capacity(3);
        for position in [
            DockPosition::Left,
            DockPosition::Right,
            DockPosition::Bottom,
        ] {
            if self.is_open(position) {
                open_panels.push(self.active_panel(position));
            }
        }
        WorkspaceSnapshotV1 {
            version: WORKSPACE_SNAPSHOT_VERSION,
            open_panels,
            active_left: self.left.active,
            left_size: self.left.size,
            right_size: self.right.size,
            bottom_size: self.bottom.size,
            zoomed_panel: self.zoomed,
        }
    }

    fn slot(&self, position: DockPosition) -> &DockSlot {
        match position {
            DockPosition::Left => &self.left,
            DockPosition::Right => &self.right,
            DockPosition::Bottom => &self.bottom,
        }
    }

    fn slot_mut(&mut self, position: DockPosition) -> &mut DockSlot {
        match position {
            DockPosition::Left => &mut self.left,
            DockPosition::Right => &mut self.right,
            DockPosition::Bottom => &mut self.bottom,
        }
    }

    pub(crate) fn is_open(&self, position: DockPosition) -> bool {
        self.slot(position).open
    }

    pub(crate) fn is_panel_active(&self, panel: PanelId) -> bool {
        let slot = self.slot(panel.position());
        slot.open && slot.active == panel
    }

    pub(crate) fn active_panel(&self, position: DockPosition) -> PanelId {
        self.slot(position).active
    }

    pub(crate) fn size(&self, position: DockPosition) -> f32 {
        self.slot(position).size
    }

    /// Activate a panel and open its canonical dock. Unlike `toggle_panel`,
    /// this never closes an already active panel.
    pub(crate) fn activate_panel(&mut self, panel: PanelId) {
        let slot = self.slot_mut(panel.position());
        slot.active = panel;
        slot.open = true;
    }

    /// Zed-style panel toggle: activating a different panel switches to it;
    /// invoking the already-visible panel closes its dock.
    pub(crate) fn toggle_panel(&mut self, panel: PanelId) -> DockFocusIntent {
        let slot = self.slot_mut(panel.position());
        if slot.open && slot.active == panel {
            slot.open = false;
            if self.zoomed == Some(panel) {
                self.zoomed = None;
            }
            DockFocusIntent::WorldCanvas
        } else {
            slot.active = panel;
            slot.open = true;
            DockFocusIntent::Panel(panel)
        }
    }

    pub(crate) fn reset_size(&mut self, position: DockPosition) {
        let active = self.active_panel(position);
        self.slot_mut(position).size = PanelDescriptor::for_panel(active).default_size;
    }

    pub(crate) fn resize_horizontal(
        &mut self,
        position: DockPosition,
        requested: f32,
        body_width: f32,
    ) {
        debug_assert!(matches!(position, DockPosition::Left | DockPosition::Right));
        let other = match position {
            DockPosition::Left => DockPosition::Right,
            DockPosition::Right => DockPosition::Left,
            DockPosition::Bottom => unreachable!(),
        };
        let other_width = if self.is_open(other) {
            self.size(other) + RESIZE_HANDLE_SIZE
        } else {
            0.0
        };
        let active = self.active_panel(position);
        let min = PanelDescriptor::for_panel(active).min_size;
        let max = (body_width - MIN_WORLD_CANVAS_WIDTH - other_width).max(min);
        self.slot_mut(position).size = requested.clamp(min, max);
    }

    pub(crate) fn resize_bottom(&mut self, requested: f32, body_height: f32) {
        let min = PanelDescriptor::for_panel(PanelId::Details).min_size;
        let max = (body_height - MIN_WORLD_CANVAS_HEIGHT - RESIZE_HANDLE_SIZE).max(min);
        self.bottom.size = requested.clamp(min, max);
    }

    pub(crate) fn toggle_zoom(&mut self, panel: PanelId) {
        self.activate_panel(panel);
        self.zoomed = if self.zoomed == Some(panel) {
            None
        } else {
            Some(panel)
        };
    }

    pub(crate) fn zoomed_panel(&self) -> Option<PanelId> {
        self.zoomed
    }
}

fn valid_size(size: f32, panel: PanelId) -> f32 {
    let descriptor = PanelDescriptor::for_panel(panel);
    if size.is_finite() {
        size.max(descriptor.min_size)
    } else {
        descriptor.default_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adaptive_defaults_open_only_world() {
        let state = DockState::default();
        assert!(state.is_panel_active(PanelId::World));
        assert!(!state.is_panel_active(PanelId::Search));
        assert!(!state.is_open(DockPosition::Right));
        assert!(!state.is_open(DockPosition::Bottom));
    }

    #[test]
    fn switching_left_panels_does_not_close_the_dock() {
        let mut state = DockState::default();
        assert_eq!(
            state.toggle_panel(PanelId::Search),
            DockFocusIntent::Panel(PanelId::Search)
        );
        assert!(state.is_panel_active(PanelId::Search));
        assert_eq!(
            state.toggle_panel(PanelId::Search),
            DockFocusIntent::WorldCanvas
        );
        assert!(!state.is_open(DockPosition::Left));
    }

    #[test]
    fn horizontal_resize_preserves_world_canvas() {
        let mut state = DockState::default();
        state.activate_panel(PanelId::Assistant);
        state.resize_horizontal(DockPosition::Right, 900.0, 1_200.0);
        assert_eq!(state.size(DockPosition::Right), 554.0);
        state.resize_horizontal(DockPosition::Left, 10.0, 1_200.0);
        assert_eq!(state.size(DockPosition::Left), 220.0);
    }

    #[test]
    fn bottom_resize_and_reset_use_descriptor_limits() {
        let mut state = DockState::default();
        state.resize_bottom(900.0, 800.0);
        assert_eq!(state.size(DockPosition::Bottom), 554.0);
        state.reset_size(DockPosition::Bottom);
        assert_eq!(state.size(DockPosition::Bottom), 320.0);
    }

    #[test]
    fn only_one_panel_is_zoomed_and_toggling_it_restores_the_workspace() {
        let mut state = DockState::default();
        state.toggle_zoom(PanelId::Assistant);
        assert_eq!(state.zoomed_panel(), Some(PanelId::Assistant));
        state.toggle_zoom(PanelId::Details);
        assert_eq!(state.zoomed_panel(), Some(PanelId::Details));
        state.toggle_zoom(PanelId::Details);
        assert_eq!(state.zoomed_panel(), None);
    }

    #[test]
    fn workspace_snapshot_round_trips() {
        let temp = tempfile::tempdir().unwrap();
        let path = DockState::state_path(temp.path());
        let mut state = DockState::default();
        state.toggle_panel(PanelId::Search);
        state.activate_panel(PanelId::Assistant);
        state.activate_panel(PanelId::Details);
        state.toggle_zoom(PanelId::Assistant);
        state.save(&path).unwrap();

        assert_eq!(DockState::load(&path), state);
    }

    #[test]
    fn corrupt_and_unsupported_snapshots_fall_back_to_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let path = DockState::state_path(temp.path());
        std::fs::write(&path, b"not json").unwrap();
        assert_eq!(DockState::load(&path), DockState::default());

        std::fs::write(
            &path,
            br#"{"version":99,"open_panels":[],"active_left":"world","left_size":280.0,"right_size":360.0,"bottom_size":320.0,"zoomed_panel":null}"#,
        )
        .unwrap();
        assert_eq!(DockState::load(&path), DockState::default());
    }
}
