//! Cross-platform policy and rendering primitives for application-owned window chrome.
//!
//! The compositor remains authoritative: callers request client decorations where
//! supported, then select this chrome only from GPUI's negotiated [`Decorations`]
//! value. Desktop-name environment variables are deliberately absent.

use gpui::{Decorations, Pixels, Point, ResizeEdge, Size, Tiling};

pub const APPLICATION_NAME: &str = "u-forge.ai";
pub const APPLICATION_ID: &str = "ai.u-forge.u-forge";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecorationMode {
    Server,
    Client,
}

impl DecorationMode {
    pub fn negotiated(decorations: Decorations) -> Self {
        match decorations {
            Decorations::Server => Self::Server,
            Decorations::Client { .. } => Self::Client,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowControlSide {
    Left,
    Right,
}

impl WindowControlSide {
    pub fn from_left_preference(left: bool) -> Self {
        if left { Self::Left } else { Self::Right }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EdgeValues {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CornerValues {
    pub top_left: f32,
    pub top_right: f32,
    pub bottom_right: f32,
    pub bottom_left: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameMetrics {
    pub inset: f32,
    pub border: f32,
    pub corner_radius: f32,
    pub resize_width: f32,
}

impl FrameMetrics {
    pub fn for_interface_size(interface_size: f32) -> Self {
        let scale = interface_size.clamp(14.0, 32.0) / 16.0;
        Self {
            inset: 10.0 * scale,
            border: 1.0 * scale,
            corner_radius: 8.0 * scale,
            resize_width: 8.0 * scale,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameGeometry {
    pub inset: EdgeValues,
    pub border: EdgeValues,
    pub corners: CornerValues,
    pub resize_width: f32,
    pub draw_shadow: bool,
    pub show_title_bar: bool,
    pub tiling: Tiling,
}

impl FrameGeometry {
    pub fn for_window(
        reported_tiling: Tiling,
        maximized: bool,
        fullscreen: bool,
        metrics: FrameMetrics,
    ) -> Self {
        if fullscreen {
            return Self {
                inset: EdgeValues::default(),
                border: EdgeValues::default(),
                corners: CornerValues::default(),
                resize_width: 0.0,
                draw_shadow: false,
                show_title_bar: false,
                tiling: Tiling::tiled(),
            };
        }

        let tiling = if maximized {
            Tiling::tiled()
        } else {
            reported_tiling
        };
        let free = |tiled: bool, value: f32| if tiled { 0.0 } else { value };

        Self {
            inset: EdgeValues {
                top: free(tiling.top, metrics.inset),
                right: free(tiling.right, metrics.inset),
                bottom: free(tiling.bottom, metrics.inset),
                left: free(tiling.left, metrics.inset),
            },
            border: EdgeValues {
                top: free(tiling.top, metrics.border),
                right: free(tiling.right, metrics.border),
                bottom: free(tiling.bottom, metrics.border),
                left: free(tiling.left, metrics.border),
            },
            corners: CornerValues {
                top_left: free(tiling.top || tiling.left, metrics.corner_radius),
                top_right: free(tiling.top || tiling.right, metrics.corner_radius),
                bottom_right: free(tiling.bottom || tiling.right, metrics.corner_radius),
                bottom_left: free(tiling.bottom || tiling.left, metrics.corner_radius),
            },
            resize_width: metrics.resize_width,
            draw_shadow: !tiling.is_tiled(),
            show_title_bar: true,
            tiling,
        }
    }

    pub fn resize_edge(&self, position: Point<Pixels>, size: Size<Pixels>) -> Option<ResizeEdge> {
        if self.resize_width <= 0.0 {
            return None;
        }

        let x = f32::from(position.x);
        let y = f32::from(position.y);
        let width = f32::from(size.width);
        let height = f32::from(size.height);
        let near_top = !self.tiling.top && y < self.resize_width;
        let near_right = !self.tiling.right && x > width - self.resize_width;
        let near_bottom = !self.tiling.bottom && y > height - self.resize_width;
        let near_left = !self.tiling.left && x < self.resize_width;

        match (near_top, near_right, near_bottom, near_left) {
            (true, _, _, true) => Some(ResizeEdge::TopLeft),
            (true, true, _, _) => Some(ResizeEdge::TopRight),
            (_, true, true, _) => Some(ResizeEdge::BottomRight),
            (_, _, true, true) => Some(ResizeEdge::BottomLeft),
            (true, _, _, _) => Some(ResizeEdge::Top),
            (_, true, _, _) => Some(ResizeEdge::Right),
            (_, _, true, _) => Some(ResizeEdge::Bottom),
            (_, _, _, true) => Some(ResizeEdge::Left),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Decorations, Tiling, point, px, size};

    use super::{DecorationMode, FrameGeometry, FrameMetrics, ResizeEdge, WindowControlSide};

    #[test]
    fn decoration_mode_uses_the_negotiated_gpui_value() {
        assert_eq!(
            DecorationMode::negotiated(Decorations::Server),
            DecorationMode::Server
        );
        assert_eq!(
            DecorationMode::negotiated(Decorations::Client {
                tiling: Tiling::default()
            }),
            DecorationMode::Client
        );
    }

    #[test]
    fn window_control_preference_defaults_to_right_semantics() {
        assert_eq!(
            WindowControlSide::from_left_preference(false),
            WindowControlSide::Right
        );
        assert_eq!(
            WindowControlSide::from_left_preference(true),
            WindowControlSide::Left
        );
    }

    #[test]
    fn floating_frame_has_insets_rounding_shadow_and_resize_edges() {
        let geometry = FrameGeometry::for_window(
            Tiling::default(),
            false,
            false,
            FrameMetrics::for_interface_size(16.0),
        );

        assert_eq!(geometry.inset.top, 10.0);
        assert_eq!(geometry.border.left, 1.0);
        assert_eq!(geometry.corners.top_right, 8.0);
        assert!(geometry.draw_shadow);
        assert!(geometry.show_title_bar);
        assert_eq!(
            geometry.resize_edge(point(px(2.0), px(2.0)), size(px(800.0), px(600.0))),
            Some(ResizeEdge::TopLeft)
        );
    }

    #[test]
    fn tiled_edges_remove_only_their_frame_and_resize_regions() {
        let geometry = FrameGeometry::for_window(
            Tiling {
                top: true,
                left: true,
                right: false,
                bottom: false,
            },
            false,
            false,
            FrameMetrics::for_interface_size(16.0),
        );

        assert_eq!(geometry.inset.top, 0.0);
        assert_eq!(geometry.inset.left, 0.0);
        assert_eq!(geometry.inset.right, 10.0);
        assert_eq!(geometry.corners.top_right, 0.0);
        assert_eq!(geometry.corners.bottom_right, 8.0);
        assert!(!geometry.draw_shadow);
        assert_eq!(
            geometry.resize_edge(point(px(2.0), px(300.0)), size(px(800.0), px(600.0))),
            None
        );
        assert_eq!(
            geometry.resize_edge(point(px(798.0), px(300.0)), size(px(800.0), px(600.0))),
            Some(ResizeEdge::Right)
        );
    }

    #[test]
    fn maximized_and_fullscreen_frames_cannot_resize() {
        let metrics = FrameMetrics::for_interface_size(16.0);
        let maximized = FrameGeometry::for_window(Tiling::default(), true, false, metrics);
        let fullscreen = FrameGeometry::for_window(Tiling::default(), false, true, metrics);

        assert_eq!(maximized.inset, Default::default());
        assert!(!maximized.draw_shadow);
        assert!(maximized.show_title_bar);
        assert_eq!(
            maximized.resize_edge(point(px(0.0), px(0.0)), size(px(800.0), px(600.0))),
            None
        );
        assert!(!fullscreen.show_title_bar);
        assert_eq!(fullscreen.border, Default::default());
        assert_eq!(fullscreen.corners, Default::default());
    }

    #[test]
    fn interface_scale_expands_frame_hit_targets() {
        let compact = FrameMetrics::for_interface_size(16.0);
        let large = FrameMetrics::for_interface_size(24.0);

        assert_eq!(large.inset, compact.inset * 1.5);
        assert_eq!(large.corner_radius, compact.corner_radius * 1.5);
        assert_eq!(large.resize_width, compact.resize_width * 1.5);
    }
}
