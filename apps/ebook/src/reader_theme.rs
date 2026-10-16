//! Reading-mode palettes for the *content surface* (the page).
//!
//! A reader's page tint (light / sepia / dark) is a first-class e-reader
//! feature that is deliberately distinct from the OS theme: people read on a
//! warm "paper" even with the rest of the OS in dark mode. So all **chrome**
//! (toolbars, drawers, buttons) reads strictly from the OS [`Theme`], while the
//! **page** uses one of these three palettes. The dark page palette is derived
//! from theme tokens so it follows the workspace; light/sepia are fixed paper
//! tints. Forced colours reach into the page: a sepia paper must not ignore an
//! OS high-contrast theme.

use vello::peniko::Color;

use crate::model::{ColorScheme, HiColor, ReadTheme};
use ui::theme::Theme;

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
    /// The scheme's accent: a custom scheme carries its own; the built-in
    /// pages have none and use their ink.
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
        if theme.colors_are_forced() {
            return Self::forced(theme);
        }
        match mode {
            ReadTheme::Light => Self::paper_light(),
            ReadTheme::Sepia => Self::paper_sepia(),
            ReadTheme::Dark => Self::dark_page(theme),
            ReadTheme::HighContrast => Self::from_theme(&Theme::high_contrast()),
        }
    }

    /// The page an accessibility setting forces: the theme's content
    /// surface and ink.
    fn forced(theme: &Theme) -> Self {
        let surface = theme.content_surface();
        let rule = ui::surface::edge_on(surface.page);
        ReaderPalette {
            page: surface.page,
            page_edge: rule,
            ink: surface.ink,
            ink_dim: surface.ink_dim,
            rule,
            code_bg: theme.tokens.colors.lift_on(surface.page, 1.0),
            accent: surface.ink,
            dark: page_is_dark(surface.page),
        }
    }

    fn paper_light() -> Self {
        let ink = Color::from_rgb8(0x1B, 0x1B, 0x1F);
        ReaderPalette {
            page: Color::from_rgb8(0xFC, 0xFC, 0xFA),
            page_edge: Color::from_rgb8(0xE6, 0xE6, 0xE0),
            ink,
            ink_dim: Color::from_rgb8(0x6B, 0x6B, 0x73),
            rule: Color::from_rgb8(0xDA, 0xDA, 0xD2),
            code_bg: Color::from_rgb8(0xEE, 0xEE, 0xE8),
            accent: ink,
            dark: false,
        }
    }

    fn paper_sepia() -> Self {
        let ink = Color::from_rgb8(0x40, 0x33, 0x22);
        ReaderPalette {
            page: Color::from_rgb8(0xF6, 0xEC, 0xD8),
            page_edge: Color::from_rgb8(0xE3, 0xD3, 0xB3),
            ink,
            ink_dim: Color::from_rgb8(0x8A, 0x76, 0x59),
            rule: Color::from_rgb8(0xD8, 0xC4, 0x9F),
            code_bg: Color::from_rgb8(0xEA, 0xDC, 0xC0),
            accent: ink,
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
            code_bg: c.lift_on(page, 1.0),
            accent: on.ink,
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

    /// Dark paper tracks the OS theme so a tinted workspace carries into the
    /// page; light and sepia paper tints stay fixed; forced colours reach the
    /// page.
    #[test]
    fn page_palettes_split_chrome_from_paper_and_honour_high_contrast() {
        // --- phase: dark_page_tracks_the_os_theme ---
        {
            let dark = Theme::dark();
            let cool = Theme::preset(ui::theme::Preset::DarkCool);
            let from_dark = ReaderPalette::resolve(ReadTheme::Dark, &dark);
            let from_cool = ReaderPalette::resolve(ReadTheme::Dark, &cool);
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
            assert_eq!(
                from_dark.accent, from_dark.ink,
                "a built-in page has no hue"
            );
            assert_ne!(
                rgb(from_cool.page),
                rgb(from_dark.page),
                "a different OS dark theme must yield a different dark page"
            );
            assert_eq!(rgb(from_cool.page), rgb(cool.tokens.colors.surfaces.raised));
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
            assert_eq!(light.accent, light.ink);
            assert_eq!(sepia.accent, sepia.ink);
            assert!(!light.dark && !sepia.dark);
        }

        // --- phase: forced_colours_override_sepia_paper ---
        {
            let hc = Theme::dark().with_high_contrast();
            assert!(hc.colors_are_forced());
            let sepia = ReaderPalette::resolve(ReadTheme::Sepia, &hc);
            let light = ReaderPalette::resolve(ReadTheme::Light, &hc);
            let surface = hc.content_surface();
            assert_eq!(
                rgb(sepia.page),
                rgb(surface.page),
                "sepia must not keep warm paper under forced colours"
            );
            assert_eq!(rgb(sepia.ink), rgb(surface.ink));
            assert_eq!(rgb(light.page), rgb(sepia.page));
            assert!(sepia.dark);
        }

        // --- phase: high_contrast_reading_mode_derives_from_the_hc_theme ---
        {
            let pal = ReaderPalette::resolve(ReadTheme::HighContrast, &Theme::dark());
            let hc = Theme::high_contrast();
            assert_eq!(rgb(pal.page), rgb(hc.tokens.colors.surfaces.raised));
            assert_eq!(rgb(pal.ink), rgb(hc.on_surface(pal.page).ink));
            assert!(pal.dark);
        }

        // --- phase: custom_scheme_keeps_its_paper_unless_os_is_high_contrast ---
        {
            let scheme = ColorScheme::from_bg("ocean", "Ocean", [0x0A, 0x22, 0x33]);
            let kept = ReaderPalette::from_scheme(&scheme);
            assert_eq!(rgb(kept.page), (0x0A, 0x22, 0x33));
            let hc = ReaderPalette::resolve(ReadTheme::Sepia, &Theme::dark().with_high_contrast());
            assert_ne!(
                rgb(hc.page),
                (0x0A, 0x22, 0x33),
                "high contrast still wins over a warm or custom paper"
            );
        }
    }
}
