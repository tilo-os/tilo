//! The shell's fixed keyboard chords: the ones Settings cannot rebind.
//!
//! The chords themselves, their ids and their names are
//! [`settings_store::keyboard_shortcuts::FIXED_SHORTCUTS`], the catalogue the
//! action door, Settings and the shortcut guide list. This module turns that
//! table into the form input routing matches against and answers every
//! routing question about it: client-input routing withholds these chords
//! from a focused app ([`crate::shell::input_policy::shell_owns_chord`]), the
//! `GlobalShortcuts` portal refuses to grant them to an app, and
//! `docs/shell-navigation-keyboard-map.md` carries a rendering of the table
//! that a test keeps identical.
//!
//! Editable navigation, search and lock bindings live in
//! `settings_store::hardware::ShortcutConfig`, never here: listing a default
//! here would keep blocking it after the user rebinds the action.

use std::sync::LazyLock;

use settings_store::keyboard_shortcuts::{FIXED_SHORTCUTS, FixedKey};

use crate::shell::input_policy::{ChordKey, ChordMods};

/// What a fixed chord does. Several chords may share one action (Alt+F4
/// and Super+W both close the tab).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixedChordAction {
    Screenshot,
    SwitchWindows,
    SwitchWindowsBackwards,
    SwitchAppWindows,
    CloseTab,
    ChangeWorkspace,
    NewWindow,
    AddTab,
    DuplicateTab,
    OpenTerminal,
    StripSwitcher,
    DisplayLayout,
    CaptureScreenshot,
    PictureInPicture,
    SaveRecentReplay,
    WorkspaceHistory,
    ExploreWorkspaceHistory,
    ShortcutGuide,
    StopRemoteInput,
    ToggleMouseKeys,
}

impl FixedChordAction {
    /// The action the catalogue's fixed shortcut `id` performs.
    fn from_fixed_id(id: &str) -> Option<Self> {
        Some(match id {
            "fixed-screenshot" => Self::Screenshot,
            "fixed-switch-windows" => Self::SwitchWindows,
            "fixed-switch-windows-backwards" => Self::SwitchWindowsBackwards,
            "fixed-switch-app-windows" => Self::SwitchAppWindows,
            "fixed-close-tab" => Self::CloseTab,
            "fixed-change-workspace" => Self::ChangeWorkspace,
            "fixed-new-window" => Self::NewWindow,
            "fixed-add-tab" => Self::AddTab,
            "fixed-duplicate-tab" => Self::DuplicateTab,
            "fixed-open-terminal" => Self::OpenTerminal,
            "fixed-strip-switcher" => Self::StripSwitcher,
            "fixed-display-layout" => Self::DisplayLayout,
            "fixed-capture-screenshot" => Self::CaptureScreenshot,
            "fixed-picture-in-picture" => Self::PictureInPicture,
            "fixed-save-recent-replay" => Self::SaveRecentReplay,
            "fixed-workspace-history" => Self::WorkspaceHistory,
            "fixed-explore-workspace-history" => Self::ExploreWorkspaceHistory,
            "fixed-shortcut-guide" => Self::ShortcutGuide,
            "fixed-stop-remote-input" => Self::StopRemoteInput,
            "fixed-toggle-mouse-keys" => Self::ToggleMouseKeys,
            _ => return None,
        })
    }
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

    /// The chord as the active keyboard layout's keycaps spell it, modifiers
    /// first (`Super+Shift+S`, `Strg+Alt+Entf` on a German layout).
    pub fn display(self) -> String {
        let modifiers = [
            (self.mods.logo, "Super"),
            (self.mods.ctrl, "Ctrl"),
            (self.mods.alt, "Alt"),
            (self.mods.shift, "Shift"),
        ];
        ui::keycaps::active().chord(
            modifiers
                .into_iter()
                .filter_map(|(held, name)| held.then_some(name)),
            &key_name(self.key),
        )
    }
}

/// A catalogue chord as routing matches it, when routing can match it at
/// all.
pub fn routed(chord: &settings_store::hardware::KeyChord) -> Option<FixedChord> {
    route(chord.modifiers.iter().map(String::as_str), &chord.key)
}

fn routed_chord(key: FixedKey) -> Option<FixedChord> {
    route(key.modifiers.iter().copied(), key.key)
}

fn route<'a>(modifiers: impl Iterator<Item = &'a str>, key: &str) -> Option<FixedChord> {
    let mut mods = NONE;
    for modifier in modifiers {
        match modifier {
            "Super" => mods.logo = true,
            "Ctrl" => mods.ctrl = true,
            "Alt" => mods.alt = true,
            "Shift" => mods.shift = true,
            _ => return None,
        }
    }
    let routed = match key {
        "space" => ChordKey::Space,
        "tab" => ChordKey::Tab,
        "enter" => ChordKey::Enter,
        "pageup" => ChordKey::PageUp,
        "pagedown" => ChordKey::PageDown,
        "printscreen" => ChordKey::PrintScreen,
        "left" => ChordKey::ArrowLeft,
        "right" => ChordKey::ArrowRight,
        "up" => ChordKey::ArrowUp,
        "down" => ChordKey::ArrowDown,
        "escape" => ChordKey::Escape,
        "delete" => ChordKey::Delete,
        "numlock" => ChordKey::NumLock,
        name => {
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(character), None) => ChordKey::Char(character.to_ascii_lowercase()),
                (Some('f'), Some(_)) => ChordKey::Function(name.get(1..)?.parse().ok()?),
                _ => return None,
            }
        }
    };
    Some(chord(routed, mods))
}

/// The canonical name of `key`, the one [`route`] reads back.
fn key_name(key: ChordKey) -> String {
    match key {
        ChordKey::Space => "space".into(),
        ChordKey::Tab => "tab".into(),
        ChordKey::Enter => "enter".into(),
        ChordKey::PageUp => "pageup".into(),
        ChordKey::PageDown => "pagedown".into(),
        ChordKey::PrintScreen => "printscreen".into(),
        ChordKey::ArrowLeft => "left".into(),
        ChordKey::ArrowRight => "right".into(),
        ChordKey::ArrowUp => "up".into(),
        ChordKey::ArrowDown => "down".into(),
        ChordKey::Escape => "escape".into(),
        ChordKey::Delete => "delete".into(),
        ChordKey::NumLock => "numlock".into(),
        ChordKey::Function(n) => format!("f{n}"),
        ChordKey::Char(c) => c.to_string(),
        ChordKey::Modifier | ChordKey::Other => String::new(),
    }
}

/// One action with its chords and the words people see for it.
#[derive(Clone, Debug)]
pub struct FixedChordEntry {
    pub action: FixedChordAction,
    /// The catalogue id ([`settings_store::keyboard_shortcuts::FixedShortcut::id`]).
    pub id: &'static str,
    /// The shell catalogue key of the words the shortcut guide shows.
    pub label_key: &'static str,
    pub chords: Vec<FixedChord>,
}

impl FixedChordEntry {
    /// What this action is called in the user's language.
    pub fn label(&self) -> String {
        crate::shell::i18n::t(self.label_key)
    }

    /// Every chord of this action, joined for display. Two chords that
    /// differ only in the last word of the key share the rest
    /// (`Super+Page Up / Down`); unrelated chords are listed in full.
    pub fn display(&self) -> String {
        if let [first, second] = self.chords.as_slice()
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

const fn chord(key: ChordKey, mods: ChordMods) -> FixedChord {
    FixedChord { key, mods }
}

/// Every fixed shell chord, in the catalogue's order.
pub static FIXED_CHORDS: LazyLock<Vec<FixedChordEntry>> = LazyLock::new(|| {
    FIXED_SHORTCUTS
        .iter()
        .filter_map(|fixed| {
            Some(FixedChordEntry {
                action: FixedChordAction::from_fixed_id(fixed.id)?,
                id: fixed.id,
                label_key: fixed.label_key,
                chords: fixed
                    .keys
                    .iter()
                    .copied()
                    .filter_map(routed_chord)
                    .collect(),
            })
        })
        .collect()
});

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
#[cfg(test)]
fn keyboard_map_markdown() -> String {
    use std::fmt::Write as _;
    let mut out = String::from("| Intent | Keyboard |\n|---|---|\n");
    for entry in FIXED_CHORDS.iter() {
        let _ = writeln!(out, "| {} | {} |", entry.label(), entry.display());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUPER: ChordMods = ChordMods { logo: true, ..NONE };
    const SUPER_SHIFT: ChordMods = ChordMods {
        shift: true,
        ..SUPER
    };
    const ALT_SHIFT: ChordMods = ChordMods {
        alt: true,
        shift: true,
        ..NONE
    };

    #[test]
    fn every_catalogue_chord_is_routed() {
        assert_eq!(FIXED_CHORDS.len(), FIXED_SHORTCUTS.len());
        for (entry, fixed) in FIXED_CHORDS.iter().zip(FIXED_SHORTCUTS) {
            assert_eq!(entry.id, fixed.id);
            assert_eq!(
                entry.chords.len(),
                fixed.keys.len(),
                "{} has a chord routing cannot match",
                fixed.id
            );
        }
        for action in [
            FixedChordAction::Screenshot,
            FixedChordAction::SwitchWindows,
            FixedChordAction::SwitchWindowsBackwards,
            FixedChordAction::SwitchAppWindows,
            FixedChordAction::CloseTab,
            FixedChordAction::ChangeWorkspace,
            FixedChordAction::NewWindow,
            FixedChordAction::AddTab,
            FixedChordAction::DuplicateTab,
            FixedChordAction::OpenTerminal,
            FixedChordAction::StripSwitcher,
            FixedChordAction::DisplayLayout,
            FixedChordAction::CaptureScreenshot,
            FixedChordAction::PictureInPicture,
            FixedChordAction::SaveRecentReplay,
            FixedChordAction::WorkspaceHistory,
            FixedChordAction::ExploreWorkspaceHistory,
            FixedChordAction::ShortcutGuide,
            FixedChordAction::StopRemoteInput,
            FixedChordAction::ToggleMouseKeys,
        ] {
            assert!(entry_for(action).is_some(), "{action:?} has no chord");
        }
    }

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
        for entry in FIXED_CHORDS.iter() {
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
        assert_eq!(
            display(FixedChordAction::ToggleMouseKeys).as_deref(),
            Some("Alt+Shift+Num Lock")
        );
    }

    #[test]
    fn display_names_follow_the_active_keyboard_layout() {
        let display = |action| entry_for(action).map(FixedChordEntry::display);
        let german = {
            let _german = ui::keycaps::scoped(ui::Keycaps::from_xkb("de", ""));
            [
                display(FixedChordAction::ChangeWorkspace),
                display(FixedChordAction::ToggleMouseKeys),
                display(FixedChordAction::Screenshot),
                display(FixedChordAction::CloseTab),
            ]
        };
        let mac = {
            let _mac = ui::keycaps::scoped(ui::Keycaps::from_xkb("us", "mac"));
            display(FixedChordAction::CaptureScreenshot)
        };
        assert_eq!(
            german.map(Option::unwrap_or_default),
            [
                "Super+Bild↑  /  Super+Bild↓",
                "Alt+Umschalt+Num",
                "Druck",
                "Alt+F4  /  Super+W",
            ]
        );
        assert_eq!(mac.as_deref(), Some("⌘⇧S"));
        // Routing is untouched: the chord is still the canonical one.
        assert_eq!(
            fixed_action(ChordKey::PageUp, SUPER),
            Some(FixedChordAction::ChangeWorkspace)
        );
    }

    #[test]
    fn lookup_is_exact_about_modifiers() {
        assert_eq!(
            fixed_action(ChordKey::Char('w'), SUPER),
            Some(FixedChordAction::CloseTab)
        );
        assert_eq!(fixed_action(ChordKey::Char('w'), SUPER_SHIFT), None);
        assert_eq!(
            fixed_action(ChordKey::Tab, ALT_SHIFT),
            Some(FixedChordAction::SwitchWindowsBackwards)
        );
    }
}
