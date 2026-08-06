//! GPUI-local identities and descriptors for workspace panels.
//!
//! These types intentionally do not live in `u-forge-ui-traits`: docking,
//! focus, and actions are application behavior, while that crate defines only
//! framework-neutral graph drawing contracts.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelId {
    World,
    Search,
    Assistant,
    Details,
}

impl PanelId {
    pub const fn title(self) -> &'static str {
        match self {
            Self::World => "World",
            Self::Search => "Search",
            Self::Assistant => "Assistant",
            Self::Details => "Details",
        }
    }

    pub const fn position(self) -> DockPosition {
        match self {
            Self::World | Self::Search => DockPosition::Left,
            Self::Assistant => DockPosition::Right,
            Self::Details => DockPosition::Bottom,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DockPosition {
    Left,
    Right,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PanelDescriptor {
    pub id: PanelId,
    pub default_size: f32,
    pub min_size: f32,
}

impl PanelDescriptor {
    pub const fn for_panel(id: PanelId) -> Self {
        match id.position() {
            DockPosition::Left => Self {
                id,
                default_size: 280.0,
                min_size: 220.0,
            },
            DockPosition::Right => Self {
                id,
                default_size: 360.0,
                min_size: 300.0,
            },
            DockPosition::Bottom => Self {
                id,
                default_size: 320.0,
                min_size: 200.0,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkspaceItemId {
    WorldCanvas,
}

impl WorkspaceItemId {
    pub const fn title(self) -> &'static str {
        match self {
            Self::WorldCanvas => "World Canvas",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorldCanvasViewId {
    Connections,
}

impl WorldCanvasViewId {
    pub const fn title(self) -> &'static str {
        match self {
            Self::Connections => "Connections",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_positions_and_sizes_are_canonical() {
        assert_eq!(PanelId::World.position(), DockPosition::Left);
        assert_eq!(PanelId::Search.position(), DockPosition::Left);
        assert_eq!(PanelId::Assistant.position(), DockPosition::Right);
        assert_eq!(PanelId::Details.position(), DockPosition::Bottom);

        let details = PanelDescriptor::for_panel(PanelId::Details);
        assert_eq!(details.default_size, 320.0);
        assert_eq!(details.min_size, 200.0);
    }

    #[test]
    fn stable_panel_ids_round_trip_through_json() {
        for id in [
            PanelId::World,
            PanelId::Search,
            PanelId::Assistant,
            PanelId::Details,
        ] {
            let json = serde_json::to_string(&id).unwrap();
            assert_eq!(serde_json::from_str::<PanelId>(&json).unwrap(), id);
        }
        assert_eq!(
            serde_json::to_string(&PanelId::Assistant).unwrap(),
            "\"assistant\""
        );
    }
}
