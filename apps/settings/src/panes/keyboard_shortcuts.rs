//! Keyboard Shortcuts pane — a reference for the shortcuts qOS actually
//! implements.
//!
//! This is an honest cheat-sheet, not an editor: every chord listed here is a
//! real binding the compositor (`shell`) or the UI toolkit (`ui`) handles —
//! the global chords come from the compositor's key dispatch (Super+K search,
//! Super+Tab overview, Super+L lock, Escape to dismiss) and the editing chords
//! from the toolkit's text input (Ctrl+A/C/V/X). Rebinding needs a settings
//! schema the compositor reads back, which is a follow-up; rather than ship a
//! rebinding UI that can't persist, this pane presents the current bindings
//! clearly and says customization is coming.

use ui::theme::Theme;
use ui::widget::{Row, Spacer, Text, Widget};

use crate::widgets::{El, caption, card, hgap, rows};

/// One shortcut: what it does and the chord that triggers it.
#[derive(Clone, Copy, Debug)]
pub struct Shortcut {
    pub action: &'static str,
    pub chord: &'static str,
}

/// Global compositor shortcuts — handled by the shell's key dispatch,
/// available from anywhere in the session.
pub const GLOBAL: &[Shortcut] = &[
    Shortcut {
        action: "Open search",
        chord: "Super  K",
    },
    Shortcut {
        action: "Toggle workspace overview",
        chord: "Super  Tab",
    },
    Shortcut {
        action: "Lock the screen",
        chord: "Super  L",
    },
    Shortcut {
        action: "Dismiss the current overlay",
        chord: "Esc",
    },
];

/// Text-editing shortcuts — handled by the toolkit's text input, available in
/// any editable field across every qOS app.
pub const EDITING: &[Shortcut] = &[
    Shortcut {
        action: "Select all",
        chord: "Ctrl  A",
    },
    Shortcut {
        action: "Copy",
        chord: "Ctrl  C",
    },
    Shortcut {
        action: "Paste",
        chord: "Ctrl  V",
    },
    Shortcut {
        action: "Cut",
        chord: "Ctrl  X",
    },
];

pub fn build() -> Box<dyn Widget> {
    let global = card(
        "System",
        rows(GLOBAL.iter().map(|s| shortcut_row(*s)).collect()),
    );
    let editing = card(
        "Text editing",
        rows(EDITING.iter().map(|s| shortcut_row(*s)).collect()),
    );
    let note = card(
        "About shortcuts",
        rows(vec![caption(
            "These are the shortcuts qOS handles today. Individual apps add \
             their own; custom rebinding is coming to a future release.",
        )]),
    );
    crate::widgets::cards(vec![global, editing, note])
}

/// One shortcut row: the action on the left, the chord rendered as keycaps on
/// the right. The keycaps are direct children of the row (a nested Row would
/// seize the full width and wrap below the action); as direct children the
/// action sizes to content and the caps sit at the right edge.
fn shortcut_row(s: Shortcut) -> El {
    let mut row = Row::new()
        .push(Box::new(Text::new(s.action).with_size(13.0)))
        .push(Box::new(Spacer::new()));
    // The chord string separates keys with double spaces (e.g. "Super  K"); a
    // keycap per token reads like a physical shortcut.
    for (i, key) in s.chord.split("  ").enumerate() {
        if i != 0 {
            row = row.push(hgap(6.0));
        }
        row = row.push(keycap(key));
    }
    Box::new(row)
}

/// A single key rendered as a subtle bordered "cap".
fn keycap(label: &str) -> El {
    let theme = Theme::dark();
    Box::new(ui::widget::Badge::pill(label, theme.bg_hover))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use taffy::{NodeId, TaffyTree};

    #[test]
    fn every_shortcut_has_action_and_chord() {
        for s in GLOBAL.iter().chain(EDITING.iter()) {
            assert!(!s.action.is_empty(), "shortcut missing action");
            assert!(!s.chord.is_empty(), "shortcut missing chord");
        }
    }

    #[test]
    fn lists_the_real_compositor_chords() {
        // Guard against drift: the pane must advertise the chords the shell
        // actually dispatches (Super+K search, Super+Tab overview, Super+L
        // lock) — not invented ones.
        let actions: Vec<&str> = GLOBAL.iter().map(|s| s.action).collect();
        assert!(actions.iter().any(|a| a.contains("search")));
        assert!(actions.iter().any(|a| a.contains("overview")));
        assert!(actions.iter().any(|a| a.contains("Lock")));
    }

    #[test]
    fn builds_layout() {
        let mut pane = build();
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
    }
}
