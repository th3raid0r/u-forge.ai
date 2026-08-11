//! Semantic UI tokens for the desktop application.
//!
//! Graph-specific node colors deliberately remain in `u-forge-ui-traits`;
//! these tokens describe application chrome and interactive controls.

use gpui::{App, Global, Pixels, Rems, Rgba, px, rems, rgba};
use u_forge_core::config::DEFAULT_UI_INTERFACE_SIZE;

/// Zed's UI metrics are authored against a 16 px baseline. u-forge keeps that
/// ratio while allowing interface geometry to be sized independently from
/// content text.
const BASE_INTERFACE_SIZE: f32 = 16.0;

#[derive(Debug, Clone, Copy)]
pub struct UiColors {
    pub app_surface: Rgba,
    pub panel_surface: Rgba,
    pub title_bar_surface: Rgba,
    pub title_bar_surface_inactive: Rgba,
    pub elevated_surface: Rgba,
    pub input_surface: Rgba,
    pub overlay: Rgba,
    pub border: Rgba,
    pub border_subtle: Rgba,
    pub text: Rgba,
    pub text_muted: Rgba,
    pub text_disabled: Rgba,
    pub text_inverse: Rgba,
    pub accent: Rgba,
    pub selected: Rgba,
    pub success: Rgba,
    pub warning: Rgba,
    pub danger: Rgba,
    pub focus: Rgba,
}

#[derive(Debug, Clone, Copy)]
pub struct UiMetrics {
    pub space_1: f32,
    pub space_2: f32,
    pub space_3: f32,
    pub space_4: f32,
    pub space_6: f32,
    pub radius_small: f32,
    pub radius_medium: f32,
    pub control_height_small: Pixels,
    /// Standard height shared by editable fields, dropdown triggers, and
    /// full-size buttons so adjacent controls align exactly.
    pub control_height: Pixels,
    pub menu_bar_height: Pixels,
    pub panel_header_height: Pixels,
    pub status_bar_height: Pixels,
    pub title_bar_height: Pixels,
}

/// Content type sizes remain relative to the text setting. Icon sizes are
/// interface metrics so readable controls do not require oversized body copy.
#[derive(Debug, Clone, Copy)]
pub struct UiTypography {
    pub body: Rems,
    pub label: Rems,
    pub chrome: Rems,
    pub caption: Rems,
    pub icon_small: Pixels,
    pub icon_medium: Pixels,
    pub icon_large: Pixels,
}

#[derive(Debug, Clone, Copy)]
pub struct UiTheme {
    pub colors: UiColors,
    pub metrics: UiMetrics,
    pub typography: UiTypography,
    pub interface_size: f32,
}

impl Global for UiTheme {}

impl Default for UiTheme {
    fn default() -> Self {
        Self::for_interface_size(DEFAULT_UI_INTERFACE_SIZE)
    }
}

impl UiTheme {
    pub fn for_interface_size(interface_size: f32) -> Self {
        let interface_size = interface_size.clamp(14.0, 32.0);
        let scale = interface_size / BASE_INTERFACE_SIZE;
        let scaled = |base: f32| base * scale;

        Self {
            colors: UiColors {
                app_surface: rgba(0x1e1e2eff),
                panel_surface: rgba(0x181825ff),
                title_bar_surface: rgba(0x292a3eff),
                title_bar_surface_inactive: rgba(0x222334ff),
                elevated_surface: rgba(0x313244ff),
                input_surface: rgba(0x11111bff),
                overlay: rgba(0x0000008c),
                border: rgba(0x45475aff),
                border_subtle: rgba(0x313244ff),
                text: rgba(0xcdd6f4ff),
                text_muted: rgba(0xa6adc8ff),
                text_disabled: rgba(0x6c7086ff),
                text_inverse: rgba(0x1e1e2eff),
                accent: rgba(0x89b4faff),
                selected: rgba(0x45475aaa),
                success: rgba(0xa6e3a1ff),
                warning: rgba(0xf9e2afff),
                danger: rgba(0xf38ba8ff),
                focus: rgba(0xb4befeff),
            },
            metrics: UiMetrics {
                space_1: scaled(2.0),
                space_2: scaled(4.0),
                space_3: scaled(6.0),
                space_4: scaled(8.0),
                space_6: scaled(12.0),
                radius_small: scaled(3.0),
                radius_medium: scaled(6.0),
                control_height_small: px(scaled(22.0)),
                control_height: px(scaled(28.0)),
                menu_bar_height: px(scaled(28.0)),
                panel_header_height: px(scaled(32.0)),
                status_bar_height: px(scaled(30.0)),
                title_bar_height: px(scaled(34.0)),
            },
            typography: UiTypography {
                body: rems(1.0),
                label: rems(0.875),
                // Halfway between GPUI's old 0.75-rem extra-small chrome and
                // the 0.875-rem labels, while remaining below body copy.
                chrome: rems(0.8125),
                caption: rems(0.75),
                icon_small: px(scaled(14.0)),
                icon_medium: px(scaled(16.0)),
                icon_large: px(scaled(18.0)),
            },
            interface_size,
        }
    }

    pub fn init(cx: &mut App) {
        cx.set_global(Self::default());
    }

    pub fn set_interface_size(cx: &mut App, interface_size: f32) {
        cx.set_global(Self::for_interface_size(interface_size));
    }

    pub fn get(cx: &App) -> &Self {
        cx.global::<Self>()
    }
}

#[cfg(test)]
mod tests {
    use super::UiTheme;
    use u_forge_core::config::DEFAULT_UI_INTERFACE_SIZE;

    #[test]
    fn chrome_is_larger_than_captions_but_smaller_than_body_copy() {
        let typography = UiTheme::default().typography;

        assert!(typography.chrome.0 > typography.caption.0);
        assert!(typography.chrome.0 < typography.body.0);
    }

    #[test]
    fn interface_metrics_scale_independently_from_content_type() {
        let default = UiTheme::default();
        let compact = UiTheme::for_interface_size(16.0);

        assert_eq!(default.interface_size, DEFAULT_UI_INTERFACE_SIZE);
        assert_eq!(f32::from(default.typography.icon_small), 19.25);
        assert_eq!(f32::from(default.typography.icon_medium), 22.0);
        assert_eq!(f32::from(default.typography.icon_large), 24.75);
        assert_eq!(f32::from(default.metrics.panel_header_height), 44.0);
        assert_eq!(f32::from(compact.metrics.panel_header_height), 32.0);
        assert_eq!(default.typography.body, compact.typography.body);
    }
}
