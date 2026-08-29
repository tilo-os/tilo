//! Theme-derived palette for the database client.
//!
//! The toolkit `Theme` is OS-controlled and intentionally generic; this app
//! needs a handful of extra roles (syntax-highlight colours, gutter shades,
//! NULL styling, type-icon tints). Rather than hard-code hex values — which
//! the project forbids, because it breaks theme switching — every role here
//! is *derived* from the active theme's tokens (the neutral/accent ramps and
//! the semantic colours). Swap the OS theme and the whole app re-tints.

use vello::peniko::Color;

use crate::model::TypeClass;
use ui::ColorCompat;
use ui::theme::Theme;

/// A bundle of derived colours for one frame, computed from a [`Theme`].
#[derive(Clone, Copy)]
pub struct DbPalette {
    // Editor surfaces
    pub editor_bg: Color,
    pub gutter_bg: Color,
    pub gutter_fg: Color,
    pub current_line: Color,
    // Syntax highlight
    pub kw: Color,
    pub func: Color,
    pub ty: Color,
    pub num: Color,
    pub str_lit: Color,
    pub comment: Color,
    pub op: Color,
    // Grid
    pub header_bg: Color,
    pub row_alt: Color,
    pub grid_line: Color,
    pub null_fg: Color,
    pub numeric_fg: Color,
    // Sidebar
    pub sidebar_bg: Color,
    pub panel_bg: Color,
    pub selection: Color,
    pub hover: Color,
}

/// Extension trait giving every `Theme` a `.db()` palette.
pub trait ThemeExt {
    fn db(&self) -> DbPalette;
    /// Colour for a type-icon chip, by SQL type class.
    fn type_color(&self, class: TypeClass) -> Color;
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color::from_rgb8(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()))
}

fn with_alpha(c: Color, a: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), a)
}

impl ThemeExt for Theme {
    fn db(&self) -> DbPalette {
        let c = &self.tokens.colors;
        let n = &c.neutral;
        let sem = &c.semantic;
        let accent = self.accent_primary;

        // Editor sits one shade darker than the panel background.
        let editor_bg = mix(c.surfaces.base, Color::BLACK, 0.15);
        DbPalette {
            editor_bg,
            gutter_bg: mix(editor_bg, Color::BLACK, 0.2),
            gutter_fg: self.text_dim,
            current_line: with_alpha(accent, 22),

            // Syntax colours pulled from semantic + ramp tints so they track
            // the theme rather than fixed hexes.
            kw: mix(accent, n[9], 0.1),
            func: sem.info,
            ty: sem.warning,
            num: sem.success,
            str_lit: mix(sem.warning, n[9], 0.35),
            comment: self.text_dim,
            op: mix(self.text_primary, accent, 0.3),

            header_bg: mix(c.surfaces.raised, Color::BLACK, 0.1),
            row_alt: with_alpha(n[7], 8),
            grid_line: self.border_standard,
            null_fg: with_alpha(self.text_dim, 150),
            numeric_fg: mix(self.text_primary, sem.info, 0.25),

            sidebar_bg: mix(c.surfaces.base, Color::BLACK, 0.08),
            panel_bg: c.surfaces.base,
            selection: with_alpha(accent, 60),
            hover: with_alpha(n[7], 18),
        }
    }

    fn type_color(&self, class: TypeClass) -> Color {
        let sem = &self.tokens.colors.semantic;
        match class {
            TypeClass::Integer => sem.info,
            TypeClass::Real => sem.success,
            TypeClass::Numeric => self.accent_primary,
            TypeClass::Text => sem.warning,
            TypeClass::Blob => self.text_dim,
            TypeClass::Boolean => sem.danger,
            TypeClass::DateTime => mix(sem.info, sem.success, 0.5),
            TypeClass::Unknown => self.text_dim,
        }
    }
}
