//! Reading-mode palettes for the *content surface* (the page).
//!
//! A reader's page tint (light / sepia / dark) is a first-class e-reader
//! feature that is deliberately distinct from the OS theme: people read on a
//! warm "paper" even with the rest of the OS in dark mode. So all **chrome**
//! (toolbars, drawers, buttons) reads strictly from the OS [`Theme`], while the
//! **page** uses one of these three palettes. The dark page palette is derived
//! from theme tokens so it stays on-brand; light/sepia are fixed paper tints.

use vello::peniko::Color;

use crate::model::{HiColor, ReadTheme};
use ui::theme::Theme;

/// Resolved colours for one reading mode.
#[derive(Clone, Copy)]
pub struct ReaderPalette {
    /// Page (paper) fill.
    pub page: Color,
    /// A faint inner border / page edge.
    pub page_edge: Color,
    /// Primary body ink.
    pub ink: Color,
    /// Secondary ink (captions, footer, list bullets).
    pub ink_dim: Color,
    /// Rules: blockquote bar, separators, heading underline.
    pub rule: Color,
    /// Inline-code chip background.
    pub code_bg: Color,
    /// Whether this is a dark reading mode (affects highlight alpha).
    pub dark: bool,
}

impl ReaderPalette {
    pub fn resolve(mode: ReadTheme, theme: &Theme) -> Self {
        match mode {
            ReadTheme::Light => ReaderPalette {
                page: Color::from_rgb8(0xFC, 0xFC, 0xFA),
                page_edge: Color::from_rgb8(0xE6, 0xE6, 0xE0),
                ink: Color::from_rgb8(0x1B, 0x1B, 0x1F),
                ink_dim: Color::from_rgb8(0x6B, 0x6B, 0x73),
                rule: Color::from_rgb8(0xDA, 0xDA, 0xD2),
                code_bg: Color::from_rgb8(0xEE, 0xEE, 0xE8),
                dark: false,
            },
            ReadTheme::Sepia => ReaderPalette {
                page: Color::from_rgb8(0xF6, 0xEC, 0xD8),
                page_edge: Color::from_rgb8(0xE3, 0xD3, 0xB3),
                ink: Color::from_rgb8(0x40, 0x33, 0x22),
                ink_dim: Color::from_rgb8(0x8A, 0x76, 0x59),
                rule: Color::from_rgb8(0xD8, 0xC4, 0x9F),
                code_bg: Color::from_rgb8(0xEA, 0xDC, 0xC0),
                dark: false,
            },
            ReadTheme::Dark => {
                // Derive from OS theme tokens so the dark page tracks the
                // active OS palette (Dracula, Nord, etc.).
                let c = &theme.tokens.colors;
                ReaderPalette {
                    page: c.surfaces.raised,
                    page_edge: c.border_standard,
                    ink: c.text_primary,
                    ink_dim: c.text_dim,
                    rule: c.border_standard,
                    code_bg: c.neutral[2],
                    dark: true,
                }
            }
        }
    }

    /// Translucent fill for a highlight band of `color` over this page.
    pub fn highlight_fill(&self, color: HiColor) -> Color {
        let (r, g, b) = color.rgb();
        let a = if self.dark { 110 } else { 150 };
        Color::from_rgba8(r, g, b, a)
    }

    /// Opaque swatch (for sidebar dots / picker chips).
    pub fn highlight_swatch(color: HiColor) -> Color {
        let (r, g, b) = color.rgb();
        Color::from_rgb8(r, g, b)
    }
}
