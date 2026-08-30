//! Reading-mode palettes for the *content surface* (the page).
//!
//! A reader's page tint (light / sepia / dark) is a first-class e-reader
//! feature that is deliberately distinct from the OS theme: people read on a
//! warm "paper" even with the rest of the OS in dark mode. So all **chrome**
//! (toolbars, drawers, buttons) reads strictly from the OS [`Theme`], while the
//! **page** uses one of these three palettes. The dark page palette is derived
//! from theme tokens so it stays on-brand; light/sepia are fixed paper tints.

use vello::peniko::Color;

use crate::model::{ColorScheme, HiColor, ReadTheme};
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
    /// The scheme accent — tints on-page affordances (progress bar, selection).
    /// Built-in tints inherit the OS accent; custom schemes carry their own.
    pub accent: Color,
    /// Whether this is a dark reading mode (affects highlight alpha).
    pub dark: bool,
}

impl ReaderPalette {
    /// Build a palette from a custom [`ColorScheme`] (its derived parts + its
    /// accent). Built-in tints still route through [`Self::resolve`] to keep
    /// their hand-tuned look; custom schemes come through here.
    pub fn from_scheme(s: &ColorScheme) -> Self {
        let (page, page_edge, ink, ink_dim, rule, code_bg) = s.palette_parts();
        let rgb = |c: [u8; 3]| Color::from_rgb8(c[0], c[1], c[2]);
        ReaderPalette {
            page: rgb(page),
            page_edge: rgb(page_edge),
            ink: rgb(ink),
            ink_dim: rgb(ink_dim),
            rule: rgb(rule),
            code_bg: rgb(code_bg),
            accent: rgb(s.accent),
            dark: s.dark,
        }
    }

    pub fn resolve(mode: ReadTheme, theme: &Theme) -> Self {
        match mode {
            ReadTheme::Light => ReaderPalette {
                page: Color::from_rgb8(0xFC, 0xFC, 0xFA),
                page_edge: Color::from_rgb8(0xE6, 0xE6, 0xE0),
                ink: Color::from_rgb8(0x1B, 0x1B, 0x1F),
                ink_dim: Color::from_rgb8(0x6B, 0x6B, 0x73),
                rule: Color::from_rgb8(0xDA, 0xDA, 0xD2),
                code_bg: Color::from_rgb8(0xEE, 0xEE, 0xE8),
                accent: theme.accent_primary,
                dark: false,
            },
            ReadTheme::Sepia => ReaderPalette {
                page: Color::from_rgb8(0xF6, 0xEC, 0xD8),
                page_edge: Color::from_rgb8(0xE3, 0xD3, 0xB3),
                ink: Color::from_rgb8(0x40, 0x33, 0x22),
                ink_dim: Color::from_rgb8(0x8A, 0x76, 0x59),
                rule: Color::from_rgb8(0xD8, 0xC4, 0x9F),
                code_bg: Color::from_rgb8(0xEA, 0xDC, 0xC0),
                accent: theme.accent_primary,
                dark: false,
            },
            ReadTheme::Dark => {
                // A warm-neutral charcoal paper, deliberately not derived from
                // the OS surface tokens (which skew cool/blue): channels sit
                // close together with just a hair of warmth (R ≥ B).
                ReaderPalette {
                    page: Color::from_rgb8(0x1E, 0x1D, 0x1C),
                    page_edge: Color::from_rgb8(0x33, 0x31, 0x2F),
                    ink: Color::from_rgb8(0xE7, 0xE5, 0xE1),
                    ink_dim: Color::from_rgb8(0x9A, 0x97, 0x92),
                    rule: Color::from_rgb8(0x38, 0x36, 0x33),
                    code_bg: Color::from_rgb8(0x2A, 0x28, 0x26),
                    accent: theme.accent_primary,
                    dark: true,
                }
            }
            ReadTheme::HighContrast => {
                // True black paper, pure-white ink, vivid amber accent, for
                // low-vision reading. Secondary ink stays bright so
                // captions/footers never fade out.
                ReaderPalette {
                    page: Color::from_rgb8(0x00, 0x00, 0x00),
                    page_edge: Color::from_rgb8(0x3A, 0x3A, 0x3A),
                    ink: Color::from_rgb8(0xFF, 0xFF, 0xFF),
                    ink_dim: Color::from_rgb8(0xCF, 0xCF, 0xCF),
                    rule: Color::from_rgb8(0x6A, 0x6A, 0x6A),
                    code_bg: Color::from_rgb8(0x1A, 0x1A, 0x1A),
                    accent: Color::from_rgb8(0xFF, 0xD5, 0x2E),
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
