//! Reference geometry for the shell's hand-composed chrome.
//!
//! These are the sizes and distances that are not a control height, a panel
//! width or a spacing step: a resize grip, a pinned avatar, a strip switcher
//! card. They live here, beside the other tokens, so a shell file names the
//! token instead of restating the number. [`Layout::BASE`] is the unscaled
//! reference, readable from a `const` context.

/// Reference geometry (unscaled) for the shell's hand-composed chrome.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    /// How far from a timeline event's edge a pointer can grab to resize it.
    pub resize_grip: f32,
    /// One detent of a jump gesture.
    pub jump_px_per_detent: f32,
    /// A pinned participant's face on the desk.
    pub pin: f32,
    /// Between two pinned participant faces.
    pub avatar_step: f32,
    /// A compact avatar in the strip switcher or a pairing sheet.
    pub avatar: f32,
    /// Between two compact avatars.
    pub compact_avatar_step: f32,
    /// One header row above a desk card's list.
    pub row_header: f32,
    /// One row of the desk's team list.
    pub team_row: f32,
    /// A compact section row in the strip switcher.
    pub section_height: f32,
    /// The rule between two strip switcher sections.
    pub divider_height: f32,
    /// A strip switcher card.
    pub strip_card_width: f32,
    /// The pairing sheet's height.
    pub pairing_sheet_height: f32,
    /// The pairing QR code's side.
    pub pairing_qr_side: f32,
    /// One step of a display's brightness control.
    pub display_brightness_step: f32,
    /// One line-stepping wheel notch.
    pub wheel_step: f32,
}

impl Layout {
    /// The unscaled reference geometry.
    pub const BASE: Self = Self {
        resize_grip: 6.0,
        jump_px_per_detent: 50.0,
        pin: 26.0,
        avatar_step: 22.0,
        avatar: 18.0,
        compact_avatar_step: 13.0,
        row_header: 36.0,
        team_row: 62.0,
        section_height: 26.0,
        divider_height: 9.0,
        strip_card_width: 272.0,
        pairing_sheet_height: 576.0,
        pairing_qr_side: 232.0,
        display_brightness_step: 10.0,
        wheel_step: 15.0,
    };
}

impl Default for Layout {
    fn default() -> Self {
        Self::BASE
    }
}
