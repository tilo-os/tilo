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
    /// Dark palette tuned to match `nirios-concept/src/components/apps/TerminalApp.tsx`:
    /// pure-black background, green-400 default text (Tailwind `#4ade80`),
    /// and Tailwind's `-400` ramp for the ANSI anchors so the 8-color ANSI
    /// output harmonises with the rest of the concept's UI (slate-300
    /// neutrals, cyan-400 links, purple-400 paths, yellow-400 warnings).
    pub fn dark() -> Self {
        // Tailwind-400 ramp so the terminal's ANSI colors sit in the same
        // perceptual band as the concept's JSX classes. Bright variants use
        // the `-300` ramp for a softer but clearly distinct tier.
        let red_400 = Color::rgb8(0xf8, 0x71, 0x71);
        let green_400 = Color::rgb8(0x4a, 0xde, 0x80);
        let yellow_400 = Color::rgb8(0xfa, 0xcc, 0x15);
        let blue_400 = Color::rgb8(0x60, 0xa5, 0xfa);
        let purple_400 = Color::rgb8(0xc0, 0x84, 0xfc);
        let cyan_400 = Color::rgb8(0x22, 0xd3, 0xee);
        let slate_300 = Color::rgb8(0xcb, 0xd5, 0xe1);
        let slate_500 = Color::rgb8(0x64, 0x74, 0x8b);

        let red_300 = Color::rgb8(0xfc, 0xa5, 0xa5);
        let green_300 = Color::rgb8(0x86, 0xef, 0xac);
        let yellow_300 = Color::rgb8(0xfd, 0xe0, 0x47);
        let blue_300 = Color::rgb8(0x93, 0xc5, 0xfd);
        let purple_300 = Color::rgb8(0xd8, 0xb4, 0xfe);
        let cyan_300 = Color::rgb8(0x67, 0xe8, 0xf9);
        let slate_100 = Color::rgb8(0xf1, 0xf5, 0xf9);

        Self {
            fg_default: green_400,
            bg_default: Color::BLACK,
            ansi: [
                Color::BLACK,
                red_400,
                green_400,
                yellow_400,
                blue_400,
                purple_400,
                cyan_400,
                slate_300,
                // Bright tier (intensities 8..=15).
                slate_500,
                red_300,
                green_300,
                yellow_300,
                blue_300,
                purple_300,
                cyan_300,
                slate_100,
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
