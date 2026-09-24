//! Width-driven responsiveness for Contacts.
//!
//! Contacts is a master-detail layout: a searchable, sortable contact
//! **list** (the master) beside a **detail pane** that shows the selected
//! person's card or the edit form. On a desktop-width window both panes sit
//! side by side with a draggable divider. That stops working once the
//! window is narrow enough that neither pane has room to breathe — so below
//! a width breakpoint the address book switches to single-pane **push
//! navigation**: it shows the list OR the detail, and selecting a contact
//! pushes the detail in (with a Back affordance) over an eased slide.
//! Returning Back (or Esc) slides the list back. The pane state and slide
//! are the toolkit's [`ui::widget::PushNav`]; this module owns only the
//! width decision.
//!
//! The breakpoint is decided from the app's *allocated* width, not a fixed
//! config, and the eased slide is the shared toolkit kernel
//! ([`ui::widget::FlapState`]) — the exact algorithm proven in the weather
//! and chat flaps — so the transition matches the rest of the OS.

use ui::widget::SplitLayout;

/// Below this allocated width (logical px) the list+detail split collapses
/// into single-pane push navigation. Chosen so the contact list and a
/// readable detail/edit pane stop fitting comfortably side by side (the
/// detail form's two-column rows need ~360px and the list ~280px plus the
/// divider).
pub const PUSH_BREAKPOINT: f64 = 760.0;

/// Resolve the split layout purely from the allocated width.
pub fn layout(total_w: f64) -> SplitLayout {
    layout_at(total_w, 1.0)
}

/// Resolve the split layout for a window of `total_w` logical pixels at
/// the theme's `text_scale`. The breakpoint tracks the scale because the
/// list and detail columns are sized by the text they hold: at 2× a
/// 1200px window has the room a 600px window has at 1×, so it pushes.
pub fn layout_at(total_w: f64, text_scale: f32) -> SplitLayout {
    SplitLayout::from_width(total_w, PUSH_BREAKPOINT * f64::from(text_scale.max(1.0)))
}

/// `true` when the window is narrow enough for single-pane push.
pub fn is_narrow(total_w: f64) -> bool {
    layout(total_w) == SplitLayout::Overlay
}

/// [`is_narrow`] at the theme's text scale.
pub fn is_narrow_at(total_w: f64, text_scale: f32) -> bool {
    layout_at(total_w, text_scale) == SplitLayout::Overlay
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_window_is_docked_narrow_is_push() {
        assert_eq!(layout(1280.0), SplitLayout::Docked);
        assert_eq!(layout(700.0), SplitLayout::Overlay);
        // Boundary is inclusive at the breakpoint → still docked.
        assert_eq!(layout(PUSH_BREAKPOINT), SplitLayout::Docked);
        assert_eq!(layout(PUSH_BREAKPOINT - 1.0), SplitLayout::Overlay);
        assert!(!is_narrow(1280.0));
        assert!(is_narrow(700.0));
        // At 2× the same 1280px window has the room a 640px window has at 1×,
        // so it pushes; a scale below 1× never lowers the 1× breakpoint.
        assert!(is_narrow_at(1280.0, 2.0));
        assert!(!is_narrow_at(2.0 * PUSH_BREAKPOINT, 2.0));
        assert_eq!(layout_at(PUSH_BREAKPOINT, 0.5), SplitLayout::Docked);
        assert_eq!(layout_at(PUSH_BREAKPOINT - 1.0, 0.5), SplitLayout::Overlay);
    }
}
