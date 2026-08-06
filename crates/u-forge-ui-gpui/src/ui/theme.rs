//! Semantic UI tokens for the desktop application.
//!
//! Graph-specific node colors deliberately remain in `u-forge-ui-traits`;
//! these tokens describe application chrome and interactive controls.

use gpui::{App, Global, Rgba, rgba};

#[derive(Debug, Clone, Copy)]
pub struct UiColors {
    pub app_surface: Rgba,
    pub panel_surface: Rgba,
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
    pub control_height_small: f32,
    pub control_height: f32,
    pub panel_header_height: f32,
    pub status_bar_height: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct UiTheme {
    pub colors: UiColors,
    pub metrics: UiMetrics,
}

impl Global for UiTheme {}

impl Default for UiTheme {
    fn default() -> Self {
        Self {
            colors: UiColors {
                app_surface: rgba(0x1e1e2eff),
                panel_surface: rgba(0x181825ff),
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
                space_1: 2.0,
                space_2: 4.0,
                space_3: 6.0,
                space_4: 8.0,
                space_6: 12.0,
                radius_small: 3.0,
                radius_medium: 6.0,
                control_height_small: 22.0,
                control_height: 28.0,
                panel_header_height: 28.0,
                status_bar_height: 24.0,
            },
        }
    }
}

impl UiTheme {
    pub fn init(cx: &mut App) {
        cx.set_global(Self::default());
    }

    pub fn get(cx: &App) -> &Self {
        cx.global::<Self>()
    }
}
