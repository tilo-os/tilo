//! The Tilo brand's colours, as `docs/brand` and the website use them.
//! Identity, not theme: the mark keeps them under every theme, the way an
//! app icon does.

use vello::peniko::Color;

/// The mark's two tones: its bars and the block between them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarkTones {
    pub bars: Color,
    pub block: Color,
}

/// The mark on a light ground (`mark.svg`).
pub const MARK_ON_LIGHT: MarkTones = MarkTones {
    bars: Color::from_rgb8(0x9f, 0xa6, 0xe0),
    block: Color::from_rgb8(0x2a, 0x3f, 0xd1),
};

/// The mark on a dark ground (`mark-dark.svg`): the light variant's pale
/// bars glare there.
pub const MARK_ON_DARK: MarkTones = MarkTones {
    bars: Color::from_rgb8(0x3b, 0x43, 0x76),
    block: Color::from_rgb8(0x6f, 0x82, 0xff),
};
