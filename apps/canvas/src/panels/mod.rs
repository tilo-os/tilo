//! The panels around the artwork: the layers, the inspector of what is
//! selected (or of the brush and the artwork), and the history. What they
//! change, they change through the artwork's own actions, the door agents
//! use too.
//!
//! The panels scroll in areas without a semantic key, so their many
//! per-item controls are not offered to agents one by one as UI controls:
//! each is one of the artwork's actions, which an agent already has.

pub mod brushes;
pub mod gradient;
pub mod header;
pub mod history;
pub mod inspector;
pub mod layers;
pub mod pane;
pub mod well;

use ui::icons::Icon;
use ui::prelude::*;

use crate::resource::ArtworkResource;

/// The side of an icon button in a panel row.
pub(crate) const ICON_BUTTON: f32 = 32.0; // a compact icon button's face; the toolkit keeps its pointer target

/// Small dimmed text beside a value.
pub(crate) fn caption(text: &str) -> Element {
    Box::new(Text::new(text).with_role(TextRole::Caption).dimmed())
}

/// A section's heading.
pub(crate) fn heading(id: &str) -> Element {
    Box::new(Text::new(id).with_role(TextRole::Label))
}

/// An icon-only button invoking `invoke`, `name` its tooltip and the name
/// a screen reader announces.
pub(crate) fn icon_button(
    cx: &ViewCx<ArtworkResource>,
    icon: Icon,
    name: &str,
    invoke: Invoke<ArtworkResource>,
) -> Button {
    cx.button("", invoke)
        .with_icon(icon)
        .ghost()
        .with_accessible_name(name)
        .tooltip(name)
        .with_size(ICON_BUTTON, ICON_BUTTON)
}

/// `0..=1` as a whole percentage.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a fraction clamped to 0..=1 times 100 is a small whole number"
)]
pub(crate) fn percent(fraction: f64) -> String {
    format!("{}%", (fraction.clamp(0.0, 1.0) * 100.0).round() as i64)
}
