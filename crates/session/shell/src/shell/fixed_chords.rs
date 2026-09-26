//! The shell's fixed keyboard chords: the ones Settings cannot rebind.
//!
//! One table answers every question about them. Client-input routing
//! withholds them from a focused app ([`crate::shell::input_policy::shell_owns_chord`]),
//! the session-lock pre-router accepts the lock alias, the shortcut guide
//! prints their labels and key names, the `GlobalShortcuts` portal refuses to
//! grant them to an app, and `docs/shell-navigation-keyboard-map.md` carries
//! a rendering of this table that a test keeps identical.
//!
//! Editable navigation and search bindings live in
//! `settings_store::hardware::ShortcutConfig`, never here: listing a default
//! here would keep blocking it after the user rebinds the action.

use crate::shell::input_policy::{ChordKey, ChordMods};

/// What a fixed chord does. Several chords may share one action (Alt+F4
/// and Super+W both close the tab).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixedChordAction {
    Screenshot,
    SwitchWindows,
    SwitchWindowsBackwards,
    CloseTab,
    ChangeWorkspace,
    NewWindow,
    AddTab,
    DuplicateTab,
    OpenTerminal,
    GlobalSearch,
    LockSession,
    WorkspaceWheel,
    WirelessDisplay,
    CaptureScreenshot,
    PictureInPicture,
    SaveRecentReplay,
    WorkspaceHistory,
    ExploreWorkspaceHistory,
    ShortcutGuide,
    StopRemoteInput,
}

/// One key under one exact modifier mask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixedChord {
    pub key: ChordKey,
    pub mods: ChordMods,
}

impl FixedChord {
    /// Exact match: an extra modifier is a different chord.
    pub fn matches(self, key: ChordKey, mods: ChordMods) -> bool {
        self.key == key && self.mods == mods
    }

    /// Human key names, modifiers first (`Super+Shift+S`).
    pub fn display(self) -> String {
        let key = key_name(self.key);
        let mut parts: Vec<&str> = Vec::new();
        if self.mods.logo {
            parts.push("Super");
        }
        if self.mods.ctrl {
            parts.push("Ctrl");
        }
        if self.mods.alt {
            parts.push("Alt");
        }
        if self.mods.shift {
            parts.push("Shift");
        }
        parts.push(&key);
        parts.join("+")
    }
}

fn key_name(key: ChordKey) -> String {
    match key {
        ChordKey::Space => "Space".into(),
        ChordKey::Tab => "Tab".into(),
        ChordKey::Enter => "Enter".into(),
        ChordKey::PageUp => "Page Up".into(),
        ChordKey::PageDown => "Page Down".into(),
        ChordKey::PrintScreen => "Print Screen".into(),
        ChordKey::ArrowLeft => "Left".into(),
        ChordKey::ArrowRight => "Right".into(),
        ChordKey::ArrowUp => "Up".into(),
        ChordKey::ArrowDown => "Down".into(),
        ChordKey::Escape => "Esc".into(),
        ChordKey::Delete => "Delete".into(),
        ChordKey::Function(n) => format!("F{n}"),
        ChordKey::Char(c) => c.to_ascii_uppercase().to_string(),
        ChordKey::Modifier | ChordKey::Other => String::new(),
    }
}

/// One action with its chords and the words people see for it.
#[derive(Clone, Copy, Debug)]
pub struct FixedChordEntry {
    pub action: FixedChordAction,
    /// Stable identifier for the label, usable as a localisation key.
    pub label_key: &'static str,
    /// The English label the shortcut guide and the keyboard map show.
    pub label: &'static str,
    pub chords: &'static [FixedChord],
}

impl FixedChordEntry {
    /// Every chord of this action, joined for display. Two chords that
    /// differ only in the last word of the key share the rest
    /// (`Super+Page Up / Down`); unrelated chords are listed in full.
    pub fn display(&self) -> String {
        if let [first, second] = self.chords
            && first.mods == second.mods
        {
            let (a, b) = (first.display(), second.display());
            if let (Some((head_a, tail_a)), Some((head_b, tail_b))) =
                (a.rsplit_once(' '), b.rsplit_once(' '))
                && head_a == head_b
            {
                return format!("{head_a} {tail_a} / {tail_b}");
            }
        }
        self.chords
            .iter()
            .map(|chord| chord.display())
            .collect::<Vec<_>>()
            .join("  /  ")
    }
}

const NONE: ChordMods = ChordMods {
    shift: false,
    ctrl: false,
    alt: false,
    logo: false,
};
const ALT: ChordMods = ChordMods { alt: true, ..NONE };
const ALT_SHIFT: ChordMods = ChordMods { shift: true, ..ALT };
const SUPER: ChordMods = ChordMods { logo: true, ..NONE };
const SUPER_SHIFT: ChordMods = ChordMods {
    shift: true,
    ..SUPER
};

const fn chord(key: ChordKey, mods: ChordMods) -> FixedChord {
    FixedChord { key, mods }
}

const fn entry(
    action: FixedChordAction,
    label_key: &'static str,
    label: &'static str,
    chords: &'static [FixedChord],
) -> FixedChordEntry {
    FixedChordEntry {
        action,
        label_key,
        label,
        chords,
    }
}

/// Every fixed shell chord.
pub static FIXED_CHORDS: &[FixedChordEntry] = &[
    entry(
        FixedChordAction::Screenshot,
        "shortcut-screenshot",
        "Screenshot",
        &[chord(ChordKey::PrintScreen, NONE)],
    ),
    entry(
        FixedChordAction::SwitchWindows,
        "shortcut-switch-windows",
        "Switch windows",
        &[chord(ChordKey::Tab, ALT)],
    ),
    entry(
        FixedChordAction::SwitchWindowsBackwards,
        "shortcut-switch-windows-backwards",
        "Switch windows backwards",
        &[chord(ChordKey::Tab, ALT_SHIFT)],
    ),
    entry(
        FixedChordAction::CloseTab,
        "shortcut-close-tab",
        "Close tab",
        &[
            chord(ChordKey::Function(4), ALT),
            chord(ChordKey::Char('w'), SUPER),
        ],
    ),
    entry(
        FixedChordAction::ChangeWorkspace,
        "shortcut-change-workspace",
        "Change workspace",
        &[
            chord(ChordKey::PageUp, SUPER),
            chord(ChordKey::PageDown, SUPER),
        ],
    ),
    entry(
        FixedChordAction::NewWindow,
        "shortcut-new-window",
        "New window",
        &[chord(ChordKey::Char('n'), SUPER)],
    ),
    entry(
        FixedChordAction::AddTab,
        "shortcut-add-tab",
        "Add tab here",
        &[chord(ChordKey::Char('t'), SUPER)],
    ),
    entry(
        FixedChordAction::DuplicateTab,
        "shortcut-duplicate-tab",
        "Duplicate tab",
        &[chord(ChordKey::Char('d'), SUPER)],
    ),
    entry(
        FixedChordAction::OpenTerminal,
        "shortcut-open-terminal",
        "Open terminal",
        &[chord(ChordKey::Enter, SUPER)],
    ),
    entry(
        FixedChordAction::GlobalSearch,
        "shortcut-global-search",
        "Global search",
        &[chord(ChordKey::Space, SUPER)],
    ),
    entry(
        FixedChordAction::LockSession,
        "shortcut-lock",
        "Lock",
        &[chord(ChordKey::Char('l'), SUPER)],
    ),
    entry(
        FixedChordAction::WorkspaceWheel,
        "shortcut-workspace-wheel",
        "Workspace wheel",
        &[chord(ChordKey::Char('s'), SUPER)],
    ),
    entry(
        FixedChordAction::WirelessDisplay,
        "shortcut-wireless-display",
        "Wireless display",
        &[chord(ChordKey::Char('p'), SUPER)],
    ),
    entry(
        FixedChordAction::CaptureScreenshot,
        "shortcut-capture-screenshot",
        "Capture screenshot",
        &[chord(ChordKey::Char('s'), SUPER_SHIFT)],
    ),
    entry(
        FixedChordAction::PictureInPicture,
        "shortcut-picture-in-picture",
        "Picture in picture",
        &[chord(ChordKey::Char('p'), SUPER_SHIFT)],
    ),
    entry(
        FixedChordAction::SaveRecentReplay,
        "shortcut-save-recent-replay",
        "Save recent replay",
        &[chord(ChordKey::Char('r'), SUPER_SHIFT)],
    ),
    entry(
        FixedChordAction::WorkspaceHistory,
        "shortcut-workspace-history",
        "Workspace history",
        &[chord(ChordKey::Char('h'), SUPER)],
    ),
    entry(
        FixedChordAction::ExploreWorkspaceHistory,
        "shortcut-explore-workspace-history",
        "Explore workspace history",
        &[chord(ChordKey::Char('h'), SUPER_SHIFT)],
    ),
    entry(
        FixedChordAction::ShortcutGuide,
        "shortcut-guide",
        "Keyboard guide",
        &[chord(ChordKey::Function(1), NONE)],
    ),
    entry(
        FixedChordAction::StopRemoteInput,
        "shortcut-stop-remote-input",
        "Stop all remote input",
        &[chord(ChordKey::Escape, SUPER)],
    ),
];

/// The table entry for `action`. Every action has exactly one entry
/// (pinned by a test), so `None` never happens for a listed action.
pub fn entry_for(action: FixedChordAction) -> Option<&'static FixedChordEntry> {
    FIXED_CHORDS.iter().find(|entry| entry.action == action)
}

/// The fixed action `key` under exactly `mods` performs, if any.
pub fn fixed_action(key: ChordKey, mods: ChordMods) -> Option<FixedChordAction> {
    FIXED_CHORDS.iter().find_map(|entry| {
        entry
            .chords
            .iter()
            .any(|chord| chord.matches(key, mods))
            .then_some(entry.action)
    })
}

/// Every fixed chord, flattened.
pub fn all_chords() -> impl Iterator<Item = FixedChord> {
    FIXED_CHORDS
        .iter()
        .flat_map(|entry| entry.chords.iter().copied())
}

/// The keyboard-map section of `docs/shell-navigation-keyboard-map.md`
/// between its fixed-chord markers, rendered from the table.
pub fn keyboard_map_markdown() -> String {
    use std::fmt::Write as _;
    let mut out = String::from("| Intent | Keyboard |\n|---|---|\n");
    for entry in FIXED_CHORDS {
        let _ = writeln!(out, "| {} | {} |", entry.label, entry.display());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = include_str!("../../../../../docs/shell-navigation-keyboard-map.md");
    const START: &str = "<!-- fixed-shell-chords:start -->\n";
    const END: &str = "<!-- fixed-shell-chords:end -->";

    #[test]
    fn keyboard_map_doc_matches_the_table() {
        let start = DOC
            .find(START)
            .expect("the keyboard map carries the fixed-chord start marker")
            + START.len();
        let end = DOC
            .find(END)
            .expect("the keyboard map carries the end marker");
        let documented = DOC.get(start..end).unwrap_or_default();
        let expected = keyboard_map_markdown();
        assert_eq!(
            documented, expected,
            "docs/shell-navigation-keyboard-map.md is out of date; replace the fixed-chord section with:\n{expected}"
        );
    }

    #[test]
    fn every_action_has_one_entry_and_no_chord_is_listed_twice() {
        for entry in FIXED_CHORDS {
            assert_eq!(
                FIXED_CHORDS
                    .iter()
                    .filter(|other| other.action == entry.action)
                    .count(),
                1,
                "{:?} appears twice",
                entry.action
            );
            assert!(!entry.chords.is_empty(), "{:?} has no chord", entry.action);
        }
        let chords: Vec<_> = all_chords().collect();
        for (index, chord) in chords.iter().enumerate() {
            assert!(
                !chords.iter().skip(index + 1).any(|other| other == chord),
                "{} is bound to two actions",
                chord.display()
            );
        }
    }

    #[test]
    fn display_names_match_what_the_guide_prints() {
        let display = |action| entry_for(action).map(FixedChordEntry::display);
        assert_eq!(
            display(FixedChordAction::CloseTab).as_deref(),
            Some("Alt+F4  /  Super+W")
        );
        assert_eq!(
            display(FixedChordAction::ChangeWorkspace).as_deref(),
            Some("Super+Page Up / Down")
        );
        assert_eq!(
            display(FixedChordAction::CaptureScreenshot).as_deref(),
            Some("Super+Shift+S")
        );
        assert_eq!(
            display(FixedChordAction::Screenshot).as_deref(),
            Some("Print Screen")
        );
    }

    #[test]
    fn lookup_is_exact_about_modifiers() {
        assert_eq!(
            fixed_action(ChordKey::Char('l'), SUPER),
            Some(FixedChordAction::LockSession)
        );
        assert_eq!(fixed_action(ChordKey::Char('l'), SUPER_SHIFT), None);
        assert_eq!(
            fixed_action(ChordKey::Tab, ALT_SHIFT),
            Some(FixedChordAction::SwitchWindowsBackwards)
        );
    }
}
