//! Reading-mode palettes for the *content surface* (the page).
//!
//! A reader's page tint (light / sepia / dark) is a first-class e-reader
//! feature that is deliberately distinct from the OS theme: people read on a
//! warm "paper" even with the rest of the OS in dark mode. So all **chrome**
//! (toolbars, drawers, buttons) reads strictly from the OS [`Theme`], while the
//! **page** uses one of these three palettes. The dark page palette is derived
//! from theme tokens so it stays on-brand; light/sepia are fixed paper tints.
//! High contrast / forced colours reach into the page: a sepia paper must not
//! ignore an OS high-contrast theme.

use vello::peniko::Color;

use crate::model::{ColorScheme, HiColor, ReadTheme};
use ui::theme::Theme;

fn is_high_contrast(theme: &Theme) -> bool {
    theme.name == Theme::high_contrast().name
}

fn page_is_dark(page: Color) -> bool {
    ui::a11y::relative_luminance(page) < 0.5
}

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
        if is_high_contrast(theme) {
            return Self::from_theme(theme);
        }
        match mode {
            ReadTheme::Light => Self::paper_light(theme),
            ReadTheme::Sepia => Self::paper_sepia(theme),
            ReadTheme::Dark => Self::dark_page(theme),
            ReadTheme::HighContrast => Self::from_theme(&Theme::high_contrast()),
        }
    }

    fn paper_light(theme: &Theme) -> Self {
        ReaderPalette {
            page: Color::from_rgb8(0xFC, 0xFC, 0xFA),
            page_edge: Color::from_rgb8(0xE6, 0xE6, 0xE0),
            ink: Color::from_rgb8(0x1B, 0x1B, 0x1F),
            ink_dim: Color::from_rgb8(0x6B, 0x6B, 0x73),
            rule: Color::from_rgb8(0xDA, 0xDA, 0xD2),
            code_bg: Color::from_rgb8(0xEE, 0xEE, 0xE8),
            accent: theme.accent_primary,
            dark: false,
        }
    }

    fn paper_sepia(theme: &Theme) -> Self {
        ReaderPalette {
            page: Color::from_rgb8(0xF6, 0xEC, 0xD8),
            page_edge: Color::from_rgb8(0xE3, 0xD3, 0xB3),
            ink: Color::from_rgb8(0x40, 0x33, 0x22),
            ink_dim: Color::from_rgb8(0x8A, 0x76, 0x59),
            rule: Color::from_rgb8(0xD8, 0xC4, 0x9F),
            code_bg: Color::from_rgb8(0xEA, 0xDC, 0xC0),
            accent: theme.accent_primary,
            dark: false,
        }
    }

    /// Dark paper from the active OS theme when that theme is already dark;
    /// otherwise [`Theme::dark`] so a light desktop still gets a dark page.
    fn dark_page(theme: &Theme) -> Self {
        if page_is_dark(theme.tokens.colors.surfaces.raised) {
            Self::from_theme(theme)
        } else {
            Self::from_theme(&Theme::dark())
        }
    }

    /// Page / ink / rules measured from theme tokens (HIG §9.5).
    fn from_theme(theme: &Theme) -> Self {
        let c = &theme.tokens.colors;
        let page = c.surfaces.raised;
        let rule = ui::surface::edge_on(page);
        let on = theme.on_surface(page);
        ReaderPalette {
            page,
            page_edge: rule,
            ink: on.ink,
            ink_dim: on.ink_dim,
            rule,
            code_bg: c.neutral[0],
            accent: theme.accent_primary,
            dark: page_is_dark(page),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ColorScheme, ReadTheme};
    use ui::ColorCompat;

    fn rgb(c: Color) -> (u8, u8, u8) {
        (c.r(), c.g(), c.b())
    }

    /// Dark paper tracks the OS theme so a Nord/Dracula desktop stays on-brand;
    /// light and sepia paper tints stay fixed; high contrast reaches the page.
    #[test]
    fn page_palettes_split_chrome_from_paper_and_honour_high_contrast() {
        // --- phase: dark_page_tracks_the_os_theme ---
        {
            let dark = Theme::dark();
            let nord = Theme::nord();
            let from_dark = ReaderPalette::resolve(ReadTheme::Dark, &dark);
            let from_nord = ReaderPalette::resolve(ReadTheme::Dark, &nord);
            assert_eq!(
                rgb(from_dark.page),
                rgb(dark.tokens.colors.surfaces.raised),
                "dark paper is the OS raised surface"
            );
            assert_eq!(
                rgb(from_dark.ink),
                rgb(dark.on_surface(from_dark.page).ink),
                "dark ink is readable on that surface"
            );
            assert_eq!(from_dark.accent, dark.accent_primary);
            assert_ne!(
                rgb(from_nord.page),
                rgb(from_dark.page),
                "a different OS dark theme must yield a different dark page"
            );
            assert_eq!(rgb(from_nord.page), rgb(nord.tokens.colors.surfaces.raised));
        }

        // --- phase: dark_reading_on_a_light_os_still_uses_a_dark_theme_page ---
        {
            let light = Theme::light();
            let pal = ReaderPalette::resolve(ReadTheme::Dark, &light);
            assert!(pal.dark, "dark reading mode stays a dark page");
            assert_eq!(
                rgb(pal.page),
                rgb(Theme::dark().tokens.colors.surfaces.raised),
                "light OS tokens are paper-white; the dark page falls back to Theme::dark"
            );
        }

        // --- phase: light_and_sepia_paper_stay_fixed_on_a_dark_os ---
        {
            let dark = Theme::dark();
            let light = ReaderPalette::resolve(ReadTheme::Light, &dark);
            let sepia = ReaderPalette::resolve(ReadTheme::Sepia, &dark);
            assert_eq!(rgb(light.page), (0xFC, 0xFC, 0xFA));
            assert_eq!(rgb(sepia.page), (0xF6, 0xEC, 0xD8));
            assert_eq!(light.accent, dark.accent_primary);
            assert_eq!(sepia.accent, dark.accent_primary);
            assert!(!light.dark && !sepia.dark);
        }

        // --- phase: high_contrast_os_theme_overrides_sepia_paper ---
        {
            let hc = Theme::high_contrast();
            let sepia = ReaderPalette::resolve(ReadTheme::Sepia, &hc);
            let light = ReaderPalette::resolve(ReadTheme::Light, &hc);
            assert_eq!(
                rgb(sepia.page),
                rgb(hc.tokens.colors.surfaces.raised),
                "sepia must not keep warm paper under high contrast"
            );
            assert_eq!(rgb(sepia.ink), rgb(hc.on_surface(sepia.page).ink));
            assert_eq!(rgb(light.page), rgb(sepia.page));
            assert_eq!(sepia.accent, hc.accent_primary);
            assert!(sepia.dark);
        }

        // --- phase: high_contrast_reading_mode_derives_from_the_hc_theme ---
        {
            let pal = ReaderPalette::resolve(ReadTheme::HighContrast, &Theme::dark());
            let hc = Theme::high_contrast();
            assert_eq!(rgb(pal.page), rgb(hc.tokens.colors.surfaces.raised));
            assert_eq!(rgb(pal.ink), rgb(hc.on_surface(pal.page).ink));
            assert_eq!(pal.accent, hc.accent_primary);
            assert!(pal.dark);
        }

        // --- phase: custom_scheme_keeps_its_paper_unless_os_is_high_contrast ---
        {
            let scheme = ColorScheme::from_bg("ocean", "Ocean", [0x0A, 0x22, 0x33]);
            let kept = ReaderPalette::from_scheme(&scheme);
            assert_eq!(rgb(kept.page), (0x0A, 0x22, 0x33));
            let hc = ReaderPalette::resolve(ReadTheme::Sepia, &Theme::high_contrast());
            assert_ne!(
                rgb(hc.page),
                (0x0A, 0x22, 0x33),
                "high contrast still wins over a warm or custom paper"
            );
        }
    }
}
