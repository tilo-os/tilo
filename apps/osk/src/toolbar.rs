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

use crate::inject::{Modifiers, Symbol};

/// What a toolbar button does when tapped. Distinct from
/// [`crate::layout::KeyAction`] because toolbar actions operate on the
/// host text field / clipboard / OSK chrome, not on character output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OskToolbarAction {
    /// Paste literal text from the shared host clipboard into the focused field.
    Paste,
    /// Press a real key (Esc, Tab, an arrow) with the latched modifiers.
    Key(Symbol),
    /// Latch a modifier for the next key; a second tap locks it.
    Modifier(ModifierKey),
    /// Opaque focused-field slot; the authenticated session resolves its action.
    Contextual { generation: u32, slot: u32 },
    /// Move the caret one unit left; a custom host may extend its selection.
    SelectionLeft,
    /// Move the caret one unit right; a custom host may extend its selection.
    SelectionRight,
    /// Dismiss / hide the keyboard.
    Dismiss,
    /// Escape hatch for app-specific toolbar items kept purely as data
    /// (emoji picker, voice toggle, settings, clipboard history, …).
    Custom(String),
}

/// A modifier the terminal row latches for the next key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ModifierKey {
    Ctrl,
    Alt,
}

impl ModifierKey {
    /// The chord this modifier alone holds.
    #[must_use]
    pub fn modifiers(self) -> Modifiers {
        Modifiers {
            ctrl: self == Self::Ctrl,
            alt: self == Self::Alt,
        }
    }
}

/// One modifier's state, cycled by taps the way Shift is: one tap holds it
/// for the next key, a second locks it, a third releases it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Latch {
    #[default]
    Off,
    /// Held for the next key only.
    Once,
    /// Held for every key until tapped again.
    Locked,
}

impl Latch {
    #[must_use]
    pub fn tapped(self) -> Self {
        match self {
            Self::Off => Self::Once,
            Self::Once => Self::Locked,
            Self::Locked => Self::Off,
        }
    }

    #[must_use]
    pub fn is_on(self) -> bool {
        self != Self::Off
    }
}

/// The terminal row's Ctrl and Alt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModifierLatches {
    pub ctrl: Latch,
    pub alt: Latch,
}

impl ModifierLatches {
    #[must_use]
    pub fn get(self, key: ModifierKey) -> Latch {
        match key {
            ModifierKey::Ctrl => self.ctrl,
            ModifierKey::Alt => self.alt,
        }
    }

    pub fn tap(&mut self, key: ModifierKey) {
        let latch = match key {
            ModifierKey::Ctrl => &mut self.ctrl,
            ModifierKey::Alt => &mut self.alt,
        };
        *latch = latch.tapped();
    }

    /// The modifiers the next key is pressed with.
    #[must_use]
    pub fn held(self) -> Modifiers {
        Modifiers {
            ctrl: self.ctrl.is_on(),
            alt: self.alt.is_on(),
        }
    }

    /// A key was pressed: release what was held for it alone.
    pub fn spend(&mut self) {
        for latch in [&mut self.ctrl, &mut self.alt] {
            if *latch == Latch::Once {
                *latch = Latch::Off;
            }
        }
    }
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
    pub(crate) fn semantic_target(&self, index: usize) -> String {
        match self.action {
            OskToolbarAction::Contextual { generation, slot } => {
                format!("osk:context:{generation}:{slot}:{}", self.label)
            }
            _ => format!("osk:tool:{index}:{}", self.label),
        }
    }

    pub fn new(id: &str, icon: Option<Icon>, label: &str, action: OskToolbarAction) -> Self {
        Self {
            id: id.to_string(),
            icon,
            label: label.to_string(),
            action,
        }
    }

    /// The keys a terminal needs that the letters lack: Esc, Tab, Ctrl and
    /// Alt (which latch for the next key) and the arrows, then dismissal.
    pub fn terminal_set() -> Vec<Self> {
        let key = |id: &str, icon: Option<Icon>, label: &str, symbol: Symbol| {
            Self::new(
                id,
                icon,
                &crate::i18n::t(label),
                OskToolbarAction::Key(symbol),
            )
        };
        let modifier = |id: &str, label: &str, modifier: ModifierKey| {
            Self::new(
                id,
                None,
                &crate::i18n::t(label),
                OskToolbarAction::Modifier(modifier),
            )
        };
        vec![
            key("esc", None, "osk-key-esc", Symbol::Escape),
            key("tab", None, "osk-key-tab", Symbol::Tab),
            modifier("ctrl", "osk-key-ctrl", ModifierKey::Ctrl),
            modifier("alt", "osk-key-alt", ModifierKey::Alt),
            key("left", Some(Icon::ArrowLeft), "osk-key-left", Symbol::Left),
            key("down", Some(Icon::ArrowDown), "osk-key-down", Symbol::Down),
            key("up", Some(Icon::ArrowUp), "osk-key-up", Symbol::Up),
            key(
                "right",
                Some(Icon::ArrowRight),
                "osk-key-right",
                Symbol::Right,
            ),
            Self::new(
                "dismiss",
                Some(Icon::ChevronDown),
                &ui::i18n::I18nManager::lookup_plain("osk-toolbar-dismiss"),
                OskToolbarAction::Dismiss,
            ),
        ]
    }

    /// Whether the item is a key of the terminal row, painted as a key cell
    /// rather than a toolbar button.
    #[must_use]
    pub fn is_key(&self) -> bool {
        matches!(
            self.action,
            OskToolbarAction::Key(_) | OskToolbarAction::Modifier(_)
        )
    }

    /// The default toolbar: paste, selection-left, selection-right, dismiss.
    /// Data-driven — callers can replace this wholesale via
    /// [`crate::widget::OskWidget::with_toolbar`].
    pub fn default_set() -> Vec<OskToolbarItem> {
        vec![
            OskToolbarItem::new(
                "paste",
                Some(Icon::ClipboardPaste),
                &crate::i18n::t("osk-toolbar-paste"),
                OskToolbarAction::Paste,
            ),
            OskToolbarItem::new(
                "sel-left",
                Some(Icon::ArrowLeft),
                &crate::i18n::t("osk-toolbar-left"),
                OskToolbarAction::SelectionLeft,
            ),
            OskToolbarItem::new(
                "sel-right",
                Some(Icon::ArrowRight),
                &crate::i18n::t("osk-toolbar-right"),
                OskToolbarAction::SelectionRight,
            ),
            OskToolbarItem::new(
                "dismiss",
                Some(Icon::X),
                &crate::i18n::t("osk-toolbar-hide"),
                OskToolbarAction::Dismiss,
            ),
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
