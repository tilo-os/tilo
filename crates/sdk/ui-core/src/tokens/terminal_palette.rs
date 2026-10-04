use super::colors::Colors;
use crate::color_compat::ColorCompat;
use vello::peniko::Color;

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
    let l = |x: u8, y: u8| {
        // x, y are u8 channels and t is clamped to 0.0..=1.0, so this convex
        // combination is already bounded to 0.0..=255.0; the explicit clamp
        // just makes that provable to the lint (and immune to fp rounding
        // at the edges) rather than changing the result.
        let v = (f32::from(x) * (1.0 - t) + f32::from(y) * t)
            .clamp(0.0, 255.0)
            .round();
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "v is clamped to 0.0..=255.0 immediately above, so this can never truncate or wrap"
        )]
        let channel = v as u8;
        channel
    };
    Color::from_rgb8(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Perceptual luminance in 0..=1 (Rec. 709 coefficients).
fn luma(c: Color) -> f32 {
    (0.2126 * f32::from(c.r()) + 0.7152 * f32::from(c.g()) + 0.0722 * f32::from(c.b())) / 255.0
}

impl TerminalPalette {
    /// The terminal must not carry its own colour scheme — customisation
    /// belongs at the shell/theme level, so fg/bg and the ANSI ramp are
    /// read from the active [`Colors`], tracking whatever theme the OS
    /// applies. `fg_default`/`bg_default` come straight from the theme's
    /// primary text and ground; the neutral ANSI anchors blend
    /// ground→foreground so both ends stay legible in light or dark.
    pub fn from_colors(c: &Colors) -> Self {
        let bg = c.surfaces.base;
        let fg = c.text_primary;
        let dark = luma(bg) < 0.5;

        // "Bright" tier of a chromatic anchor: toward white on a dark ground,
        // toward black on a light ground, so brights gain (not lose) contrast.
        let bright = |col: Color| {
            if dark {
                mix8(col, Color::WHITE, 0.25)
            } else {
                mix8(col, Color::BLACK, 0.22)
            }
        };
        // A grey anchored on the ground→foreground axis; both ends are known
        // legible against the ground because the foreground is.
        let grey = |t: f32| mix8(bg, fg, t);

        let red = c.semantic.danger;
        let green = c.semantic.success;
        let yellow = c.semantic.warning;
        // Blue, magenta and cyan come from the presence palette (265°, 325°
        // and 205°), so they sit at the same lightness as each other.
        let blue = c.presence.get(8);
        let magenta = c.presence.get(10);
        let cyan = c.presence.get(6);

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

    pub fn dark() -> Self {
        Self::from_colors(&Colors::dark())
    }

    pub fn light() -> Self {
        Self::from_colors(&Colors::light())
    }
}
