use vello::peniko::Color;
use crate::color_compat::ColorCompat;
use super::colors::{Colors, ACCENT_PRIMARY_IDX};

/// Palette used by terminal-style widgets. Exposes default fg/bg and a 16-slot
/// ANSI base palette. Higher-index colors (256/truecolor) are out of scope —
/// consumers still derive those from the 16 anchors or from their own mapping.
#[derive(Clone, Debug)]
pub struct TerminalPalette {
    pub fg_default: Color,
    pub bg_default: Color,
    /// ANSI 0..=15 (standard 8 + bright 8).
    pub ansi: [Color; 16],
}

/// Mix two colours in straight (non-premultiplied) 8-bit sRGB. `t = 0` → `a`.
fn mix8(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| ((x as f32) * (1.0 - t) + (y as f32) * t).round() as u8;
    Color::from_rgb8(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Perceptual luminance in 0..=1 (Rec. 709 coefficients).
fn luma(c: Color) -> f32 {
    (0.2126 * c.r() as f32 + 0.7152 * c.g() as f32 + 0.0722 * c.b() as f32) / 255.0
}

impl TerminalPalette {
    /// Derive the terminal palette from the OS theme's colour tokens.
    ///
    /// The terminal must not carry its own colour scheme — per-app colour
    /// customisation belongs at the shell/theme level, so the default fg/bg and
    /// the ANSI ramp are read from the active [`Colors`] here. The result tracks
    /// whatever theme the OS applies (graphite dark, paper light, Dracula, …)
    /// instead of a fixed palette.
    ///
    /// * `fg_default` / `bg_default` come straight from the theme's primary text
    ///   and ground, so a blank terminal is indistinguishable from any other
    ///   surface of the OS.
    /// * The six chromatic ANSI hues anchor to the theme's semantic colours —
    ///   every qOS theme defines these on-brand (danger→red, success→green,
    ///   warning→yellow, info→cyan) — with the accent supplying blue and magenta
    ///   derived from red+blue. Cyan is nudged toward green so it stays visibly
    ///   distinct from the (often blue-ish) accent.
    /// * The neutral anchors (ANSI black / white and their bright tiers) blend
    ///   from the ground toward the foreground, so both ends stay legible whether
    ///   the theme is light or dark.
    pub fn from_colors(c: &Colors) -> Self {
        let bg = c.surfaces.base;
        let fg = c.text_primary;
        let dark = luma(bg) < 0.5;

        // "Bright" tier of a chromatic anchor: toward white on a dark ground,
        // toward black on a light ground, so brights gain (not lose) contrast.
        let bright = |col: Color| {
            if dark { mix8(col, Color::WHITE, 0.25) } else { mix8(col, Color::BLACK, 0.22) }
        };
        // A grey anchored on the ground→foreground axis; both ends are known
        // legible against the ground because the foreground is.
        let grey = |t: f32| mix8(bg, fg, t);

        let red = c.semantic.danger;
        let green = c.semantic.success;
        let yellow = c.semantic.warning;
        let blue = c.accent[ACCENT_PRIMARY_IDX];
        // Keep cyan distinct from a blue-leaning accent by pulling the theme's
        // "info" hue toward green.
        let cyan = mix8(c.semantic.info, green, 0.35);
        // The theme has no magenta; red + blue lands on a tasteful one.
        let magenta = mix8(red, blue, 0.5);

        Self {
            fg_default: fg,
            bg_default: bg,
            ansi: [
                grey(0.20),      // 0  black — a hair lifted from the ground
                red,             // 1  red
                green,           // 2  green
                yellow,          // 3  yellow
                blue,            // 4  blue
                magenta,         // 5  magenta
                cyan,            // 6  cyan
                grey(0.85),      // 7  white — light grey near the foreground
                grey(0.45),      // 8  bright black — mid grey
                bright(red),     // 9  bright red
                bright(green),   // 10 bright green
                bright(yellow),  // 11 bright yellow
                bright(blue),    // 12 bright blue (also the link hue)
                bright(magenta), // 13 bright magenta
                bright(cyan),    // 14 bright cyan
                fg,              // 15 bright white — the foreground itself
            ],
        }
    }

    /// The terminal palette for the default dark theme, derived from its colours.
    pub fn dark() -> Self {
        Self::from_colors(&Colors::dark())
    }

    /// The terminal palette for the default light theme, derived from its colours.
    pub fn light() -> Self {
        Self::from_colors(&Colors::light())
    }
}
