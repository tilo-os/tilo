//! Reading-mode palettes for the *article reader pane* (the page).
//!
//! Adapted from the ebook reader's `reader_theme` so the two apps present a
//! consistent reading experience. A reader's page tint (light / sepia / dark)
//! is a first-class reading feature that is deliberately distinct from the OS
//! theme: people read on a warm "paper" even with the rest of the OS in dark
//! mode. So all **chrome** (toolbars, sidebar, buttons) reads strictly from the
//! OS [`Theme`], while the **reader page** uses one of these three palettes.
//! The dark page palette is derived from theme tokens so it stays on-brand;
//! light/sepia are fixed paper tints. High contrast / forced colours reach
//! into the page: a sepia paper must not ignore an OS high-contrast theme.

use vello::peniko::Color;

use crate::model::ReadTheme;
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
    /// Secondary ink (byline, captions, kicker).
    pub ink_dim: Color,
    /// Accent ink for the feed kicker + links.
    pub accent: Color,
    /// Rules: blockquote bar, separators, heading underline.
    pub rule: Color,
    /// Inline-code chip background.
    pub code_bg: Color,
    /// Whether this is a dark reading mode.
    pub dark: bool,
}

impl ReaderPalette {
    pub fn resolve(mode: ReadTheme, theme: &Theme) -> Self {
        if is_high_contrast(theme) {
            return Self::from_theme(theme);
        }
        match mode {
            ReadTheme::Light => Self::paper_light(theme),
            ReadTheme::Sepia => Self::paper_sepia(theme),
            ReadTheme::Dark => Self::dark_page(theme),
        }
    }

    fn paper_light(theme: &Theme) -> Self {
        ReaderPalette {
            page: Color::from_rgb8(0xFC, 0xFC, 0xFA),
            page_edge: Color::from_rgb8(0xE6, 0xE6, 0xE0),
            ink: Color::from_rgb8(0x1B, 0x1B, 0x1F),
            ink_dim: Color::from_rgb8(0x6B, 0x6B, 0x73),
            accent: theme.accent_primary,
            rule: Color::from_rgb8(0xDA, 0xDA, 0xD2),
            code_bg: Color::from_rgb8(0xEE, 0xEE, 0xE8),
            dark: false,
        }
    }

    fn paper_sepia(theme: &Theme) -> Self {
        ReaderPalette {
            page: Color::from_rgb8(0xF6, 0xEC, 0xD8),
            page_edge: Color::from_rgb8(0xE3, 0xD3, 0xB3),
            ink: Color::from_rgb8(0x40, 0x33, 0x22),
            ink_dim: Color::from_rgb8(0x8A, 0x76, 0x59),
            accent: theme.accent_primary,
            rule: Color::from_rgb8(0xD8, 0xC4, 0x9F),
            code_bg: Color::from_rgb8(0xEA, 0xDC, 0xC0),
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
            accent: theme.accent_primary,
            rule,
            code_bg: c.neutral[0],
            dark: page_is_dark(page),
        }
    }
}

fn is_high_contrast(theme: &Theme) -> bool {
    theme.name == Theme::high_contrast().name
}

fn page_is_dark(page: Color) -> bool {
    ui::a11y::relative_luminance(page) < 0.5
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ReadTheme;
    use ui::ColorCompat;

    fn rgb(c: Color) -> (u8, u8, u8) {
        (c.r(), c.g(), c.b())
    }

    /// Dark paper tracks the OS theme; light/sepia paper stays fixed; high
    /// contrast reaches the article page so sepia cannot ignore it.
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
            assert_eq!(rgb(from_dark.ink), rgb(dark.on_surface(from_dark.page).ink));
            assert_eq!(from_dark.accent, dark.accent_primary);
            assert_ne!(rgb(from_nord.page), rgb(from_dark.page));
            assert_eq!(rgb(from_nord.page), rgb(nord.tokens.colors.surfaces.raised));
        }

        // --- phase: dark_reading_on_a_light_os_still_uses_a_dark_theme_page ---
        {
            let pal = ReaderPalette::resolve(ReadTheme::Dark, &Theme::light());
            assert!(pal.dark);
            assert_eq!(
                rgb(pal.page),
                rgb(Theme::dark().tokens.colors.surfaces.raised)
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
    }
}
