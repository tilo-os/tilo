//! The customizable toolbar row that sits above the keys.
//!
//! Per the a11y design (§3.5) the toolbar is a *data-driven* strip of
//! action descriptors — candidate ribbon, selection controls, clipboard,
//! emoji, voice toggle, settings, dismiss — and is user-reorderable
//! (persisted in the per-user CRDT later). Here we model the descriptor and
//! ship a sensible default set; the widget renders it as a row of buttons
//! and hit-tests it with the same [`toolbar_rects`] geometry style as the
//! keys.

use ui::icons::Icon;
use vello::kurbo::Rect;

/// What a toolbar button does when tapped. Distinct from
/// [`crate::layout::KeyAction`] because toolbar actions operate on the
/// host text field / clipboard / OSK chrome, not on character output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OskToolbarAction {
    /// Paste clipboard contents into the focused field. Wires to the
    /// existing `arboard` clipboard path (via the host) once IM
    /// injection lands.
    Paste,
    /// Extend/move the selection (or caret) one unit left.
    SelectionLeft,
    /// Extend/move the selection (or caret) one unit right.
    SelectionRight,
    /// Dismiss / hide the keyboard.
    Dismiss,
    /// Escape hatch for app-specific toolbar items kept purely as data
    /// (emoji picker, voice toggle, settings, clipboard history, …).
    Custom(String),
}

/// One toolbar button descriptor.
#[derive(Clone, Debug)]
pub struct OskToolbarItem {
    /// Stable id (for reordering / test addressing).
    pub id: String,
    /// Optional leading vector glyph. `None` falls back to the label.
    pub icon: Option<Icon>,
    /// Human-readable label (also the fallback when `icon` is `None`).
    pub label: String,
    /// What tapping the item does.
    pub action: OskToolbarAction,
}

impl OskToolbarItem {
    pub fn new(id: &str, icon: Option<Icon>, label: &str, action: OskToolbarAction) -> Self {
        Self {
            id: id.to_string(),
            icon,
            label: label.to_string(),
            action,
        }
    }

    /// The default toolbar: paste, selection-left, selection-right, dismiss.
    /// Data-driven — callers can replace this wholesale via
    /// [`crate::widget::OskWidget::with_toolbar`].
    pub fn default_set() -> Vec<OskToolbarItem> {
        vec![
            OskToolbarItem::new(
                "paste",
                Some(Icon::ClipboardPaste),
                "Paste",
                OskToolbarAction::Paste,
            ),
            OskToolbarItem::new(
                "sel-left",
                Some(Icon::ArrowLeft),
                "Select left",
                OskToolbarAction::SelectionLeft,
            ),
            OskToolbarItem::new(
                "sel-right",
                Some(Icon::ArrowRight),
                "Select right",
                OskToolbarAction::SelectionRight,
            ),
            OskToolbarItem::new("dismiss", Some(Icon::X), "Hide", OskToolbarAction::Dismiss),
        ]
    }
}

/// **Pure geometry** for the toolbar row: split `area` into `n` equal-width
/// buttons (in declaration order), returning each item's index + rect.
/// Mirrors [`crate::layout::key_rects`] so the widget hit-tests the toolbar
/// the same way it hit-tests keys.
pub fn toolbar_rects(items: &[OskToolbarItem], area: Rect) -> Vec<(usize, Rect)> {
    let n = items.len();
    if n == 0 || area.width() <= 0.0 || area.height() <= 0.0 {
        return Vec::new();
    }
    let unit_w = area.width() / n as f64;
    let mut out = Vec::with_capacity(n);
    let mut x = area.x0;
    for i in 0..n {
        let rect = Rect::new(x, area.y0, x + unit_w, area.y1);
        out.push((i, rect));
        x += unit_w;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_toolbar_has_four_evenly_split_items() {
        eprintln!("phase default_toolbar_has_four_items");
        {
            let t = OskToolbarItem::default_set();
            assert_eq!(t.len(), 4);
            assert_eq!(t[0].action, OskToolbarAction::Paste);
            assert_eq!(t[3].action, OskToolbarAction::Dismiss);
        }

        eprintln!("phase toolbar_rects_split_evenly_and_fill_width");
        {
            let items = OskToolbarItem::default_set();
            let rects = toolbar_rects(&items, Rect::new(0.0, 0.0, 400.0, 50.0));
            assert_eq!(rects.len(), 4);
            for (_, r) in &rects {
                assert!((r.width() - 100.0).abs() < 1e-6);
            }
            assert!((rects.last().unwrap().1.x1 - 400.0).abs() < 1e-6);
        }
    }
}
