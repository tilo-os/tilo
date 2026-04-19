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

impl TerminalPalette {
    pub fn dark() -> Self {
        Self {
            fg_default: Color::rgb8(220, 220, 220),
            bg_default: Color::rgb8(10, 10, 10),
            ansi: [
                Color::BLACK,
                Color::RED,
                Color::GREEN,
                Color::YELLOW,
                Color::BLUE,
                Color::MAGENTA,
                Color::CYAN,
                Color::WHITE,
                Color::rgb8(128, 128, 128),
                Color::rgb8(255, 0, 0),
                Color::rgb8(0, 255, 0),
                Color::rgb8(255, 255, 0),
                Color::rgb8(0, 0, 255),
                Color::rgb8(255, 0, 255),
                Color::rgb8(0, 255, 255),
                Color::WHITE,
            ],
        }
    }

    pub fn light() -> Self {
        // Same ANSI colors; default fg/bg flipped for light surfaces.
        let mut p = Self::dark();
        p.fg_default = Color::rgb8(20, 20, 20);
        p.bg_default = Color::rgb8(245, 245, 245);
        p
    }
}
