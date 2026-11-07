//! The desktop's keyboard shortcuts as one typed catalogue.
//!
//! Every surface that lists or changes a desktop shortcut reads this module:
//! the shell publishes it on the action door as the `keyboard-shortcuts`
//! resource, the shortcut guide and Settings render it, and the shell's
//! fixed-chord router is built from [`FIXED_SHORTCUTS`]. An entry has a
//! stable id, a Fluent label key (resolved by whoever shows it), the chords
//! it answers to now and by default, whether it can be rebound, and a
//! category.
//!
//! Rebinding goes through [`ShortcutConfig::set`], [`ShortcutConfig::reset`]
//! and [`ShortcutConfig::reset_all`], which refuse a chord another shortcut
//! holds and name the holder; nothing here ever takes a chord away from
//! another shortcut.

use std::collections::BTreeSet;

use crate::hardware::{KeyChord, ShortcutAction, ShortcutConfig, ShortcutConfigError};

/// What part of the desktop a shortcut belongs to; Settings groups by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ShortcutCategory {
    System,
    Navigation,
    Windows,
    Apps,
    Capture,
    Accessibility,
}

/// Who carries out a shortcut: the desktop's own windows, strips and
/// windows' places, or the session under them (the device, the surfaces over
/// the desk, the people and agents here).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShortcutHome {
    Desk,
    Session,
}

impl ShortcutCategory {
    /// Every category, in the order Settings shows them.
    pub const ALL: [Self; 6] = [
        Self::System,
        Self::Navigation,
        Self::Windows,
        Self::Apps,
        Self::Capture,
        Self::Accessibility,
    ];

    /// Stable id, as the action door publishes it.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Navigation => "navigation",
            Self::Windows => "windows",
            Self::Apps => "apps",
            Self::Capture => "capture",
            Self::Accessibility => "accessibility",
        }
    }

    /// Fluent key of the category's name.
    #[must_use]
    pub fn label_key(self) -> String {
        format!("shortcut-category-{}", self.id())
    }
}

/// One key under an exact set of modifiers, spelled as [`KeyChord`] spells
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixedKey {
    pub modifiers: &'static [&'static str],
    pub key: &'static str,
}

impl FixedKey {
    #[must_use]
    pub fn chord(self) -> KeyChord {
        KeyChord {
            modifiers: self
                .modifiers
                .iter()
                .map(|modifier| (*modifier).to_string())
                .collect::<BTreeSet<_>>(),
            key: self.key.to_string(),
        }
    }
}

/// A shortcut the desktop answers before any application and nobody can
/// rebind. Several chords may share one (Alt+F4 and Super+W both close the
/// tab).
#[derive(Clone, Copy, Debug)]
pub struct FixedShortcut {
    /// Stable id; `fixed-` keeps it apart from every rebindable action's.
    pub id: &'static str,
    /// Fluent key of what it is called.
    pub label_key: &'static str,
    pub category: ShortcutCategory,
    pub keys: &'static [FixedKey],
}

impl FixedShortcut {
    #[must_use]
    pub fn chords(&self) -> Vec<KeyChord> {
        self.keys.iter().map(|key| key.chord()).collect()
    }

    #[must_use]
    pub fn owns(&self, chord: &KeyChord) -> bool {
        self.keys.iter().any(|key| key.chord() == *chord)
    }
}

const fn key(modifiers: &'static [&'static str], key: &'static str) -> FixedKey {
    FixedKey { modifiers, key }
}

const fn fixed(
    id: &'static str,
    label_key: &'static str,
    category: ShortcutCategory,
    keys: &'static [FixedKey],
) -> FixedShortcut {
    FixedShortcut {
        id,
        label_key,
        category,
        keys,
    }
}

/// Every fixed desktop shortcut. The shell routes exactly these chords
/// before a focused application sees them.
pub static FIXED_SHORTCUTS: &[FixedShortcut] = &[
    fixed(
        "fixed-screenshot",
        "shortcut-screenshot",
        ShortcutCategory::Capture,
        &[key(&[], "printscreen")],
    ),
    fixed(
        "fixed-switch-windows",
        "shortcut-switch-windows",
        ShortcutCategory::Navigation,
        &[key(&["Alt"], "tab")],
    ),
    fixed(
        "fixed-switch-windows-backwards",
        "shortcut-switch-windows-backwards",
        ShortcutCategory::Navigation,
        &[key(&["Alt", "Shift"], "tab")],
    ),
    fixed(
        "fixed-switch-app-windows",
        "shortcut-switch-app-windows",
        ShortcutCategory::Navigation,
        &[key(&["Alt"], "`")],
    ),
    fixed(
        "fixed-close-tab",
        "shortcut-close-tab",
        ShortcutCategory::Windows,
        &[key(&["Alt"], "f4"), key(&["Super"], "w")],
    ),
    fixed(
        "fixed-change-workspace",
        "shortcut-change-workspace",
        ShortcutCategory::Navigation,
        &[key(&["Super"], "pageup"), key(&["Super"], "pagedown")],
    ),
    fixed(
        "fixed-new-window",
        "shortcut-new-window",
        ShortcutCategory::Windows,
        &[key(&["Super"], "n")],
    ),
    fixed(
        "fixed-duplicate-tab",
        "shortcut-duplicate-tab",
        ShortcutCategory::Windows,
        &[key(&["Super"], "t")],
    ),
    fixed(
        "fixed-open-terminal",
        "shortcut-open-terminal",
        ShortcutCategory::Apps,
        &[key(&["Super"], "enter")],
    ),
    fixed(
        "fixed-strip-switcher",
        "shortcut-strip-switcher",
        ShortcutCategory::Navigation,
        &[key(&["Super"], "s")],
    ),
    fixed(
        "fixed-display-layout",
        "shortcut-display-layout",
        ShortcutCategory::System,
        &[key(&["Super"], "p")],
    ),
    fixed(
        "fixed-capture-screenshot",
        "shortcut-capture-screenshot",
        ShortcutCategory::Capture,
        &[key(&["Super", "Shift"], "s")],
    ),
    fixed(
        "fixed-picture-in-picture",
        "shortcut-picture-in-picture",
        ShortcutCategory::Capture,
        &[key(&["Super", "Shift"], "p")],
    ),
    fixed(
        "fixed-save-recent-replay",
        "shortcut-save-recent-replay",
        ShortcutCategory::Capture,
        &[key(&["Super", "Shift"], "r")],
    ),
    fixed(
        "fixed-workspace-history",
        "shortcut-workspace-history",
        ShortcutCategory::System,
        &[key(&["Super"], "h")],
    ),
    fixed(
        "fixed-explore-workspace-history",
        "shortcut-explore-workspace-history",
        ShortcutCategory::System,
        &[key(&["Super", "Shift"], "h")],
    ),
    fixed(
        "fixed-shortcut-guide",
        "shortcut-guide",
        ShortcutCategory::System,
        &[key(&[], "f1")],
    ),
    fixed(
        "fixed-stop-remote-input",
        "shortcut-stop-remote-input",
        ShortcutCategory::System,
        &[key(&["Super"], "escape")],
    ),
    fixed(
        "fixed-toggle-mouse-keys",
        "shortcut-toggle-mouse-keys",
        ShortcutCategory::Accessibility,
        &[key(&["Alt", "Shift"], "numlock")],
    ),
    // The chord every screen answers, before sign-in too
    // (`ui::screen_reader_chord`); the one Windows uses.
    fixed(
        "fixed-toggle-screen-reader",
        "shortcut-toggle-screen-reader",
        ShortcutCategory::Accessibility,
        &[key(&["Ctrl", "Super"], "enter")],
    ),
];

/// Chords the desktop keeps for itself only in some states, so they are
/// not routed ahead of every application, yet nothing may be bound to
/// them: Vim navigation (while Vim mode is on) and bare Escape, which
/// dismisses whatever the desktop has open.
pub static RESERVED_SHORTCUTS: &[FixedShortcut] = &[
    fixed(
        "reserved-vim-navigation",
        "shortcut-vim-navigation",
        ShortcutCategory::Navigation,
        &[
            key(&["Super", "Ctrl"], "h"),
            key(&["Super", "Ctrl"], "j"),
            key(&["Super", "Ctrl"], "k"),
            key(&["Super", "Ctrl"], "l"),
            key(&["Super", "Ctrl", "Shift"], "h"),
            key(&["Super", "Ctrl", "Shift"], "l"),
        ],
    ),
    fixed(
        "reserved-dismiss",
        "shortcut-dismiss",
        ShortcutCategory::System,
        &[key(&[], "escape")],
    ),
];

/// The fixed or reserved shortcut `id` names.
#[must_use]
pub fn fixed_shortcut(id: &str) -> Option<&'static FixedShortcut> {
    FIXED_SHORTCUTS
        .iter()
        .chain(RESERVED_SHORTCUTS)
        .find(|fixed| fixed.id == id)
}

/// The fixed or reserved desktop shortcut `chord` belongs to, when it
/// belongs to one: a chord no setting and no published action may take.
#[must_use]
pub fn desktop_owner(chord: &KeyChord) -> Option<&'static FixedShortcut> {
    FIXED_SHORTCUTS
        .iter()
        .chain(RESERVED_SHORTCUTS)
        .find(|fixed| fixed.owns(chord))
}

/// What a fixed shortcut does, for the ones a session dispatches by its own
/// command. A fixed chord the keys already answer where it is resolved — the
/// window walk, the workspace steps, the display sheet, Print — names no
/// action here: its chord is answered before this table is asked.
///
/// Each action is the verb a menu row, a palette row and an agent's
/// invocation name too, so a person's chord and an action row do the same
/// thing.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum FixedAction {
    CloseTab,
    NewWindow,
    DuplicateTab,
    OpenTerminal,
    StripSwitcher,
    CaptureScreenshot,
    PictureInPicture,
    SaveRecentReplay,
    WorkspaceHistory,
    ExploreWorkspaceHistory,
    ShortcutGuide,
    StopRemoteInput,
}

impl FixedAction {
    /// The action the catalogue's fixed shortcut `id` performs, when a
    /// session carries one.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "fixed-close-tab" => Self::CloseTab,
            "fixed-new-window" => Self::NewWindow,
            "fixed-duplicate-tab" => Self::DuplicateTab,
            "fixed-open-terminal" => Self::OpenTerminal,
            "fixed-strip-switcher" => Self::StripSwitcher,
            "fixed-capture-screenshot" => Self::CaptureScreenshot,
            "fixed-picture-in-picture" => Self::PictureInPicture,
            "fixed-save-recent-replay" => Self::SaveRecentReplay,
            "fixed-workspace-history" => Self::WorkspaceHistory,
            "fixed-explore-workspace-history" => Self::ExploreWorkspaceHistory,
            "fixed-shortcut-guide" => Self::ShortcutGuide,
            "fixed-stop-remote-input" => Self::StopRemoteInput,
            _ => return None,
        })
    }

    /// The catalogue id of this action.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::CloseTab => "fixed-close-tab",
            Self::NewWindow => "fixed-new-window",
            Self::DuplicateTab => "fixed-duplicate-tab",
            Self::OpenTerminal => "fixed-open-terminal",
            Self::StripSwitcher => "fixed-strip-switcher",
            Self::CaptureScreenshot => "fixed-capture-screenshot",
            Self::PictureInPicture => "fixed-picture-in-picture",
            Self::SaveRecentReplay => "fixed-save-recent-replay",
            Self::WorkspaceHistory => "fixed-workspace-history",
            Self::ExploreWorkspaceHistory => "fixed-explore-workspace-history",
            Self::ShortcutGuide => "fixed-shortcut-guide",
            Self::StopRemoteInput => "fixed-stop-remote-input",
        }
    }
}

/// The fixed action `chord` asks for, when a session dispatches one.
#[must_use]
pub fn fixed_action(chord: &KeyChord) -> Option<FixedAction> {
    FIXED_SHORTCUTS
        .iter()
        .find(|fixed| fixed.owns(chord))
        .and_then(|fixed| FixedAction::from_id(fixed.id))
}

impl ShortcutConfig {
    /// Whether the desktop answers `chord` before a stored binding of
    /// `action` to it: a chord fixed since the binding was saved
    /// (Ctrl+Super+Enter turns the screen reader on and off), other than
    /// the action's own default in `self`, the default table. Such a
    /// binding would never run.
    pub(crate) fn shadows(&self, action: ShortcutAction, chord: &KeyChord) -> bool {
        self.bindings.get(&action) != Some(chord) && desktop_owner(chord).is_some()
    }
}

/// One row of the catalogue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutEntry {
    /// Stable id: a [`ShortcutAction::id`] or a [`FixedShortcut::id`].
    pub id: &'static str,
    /// Fluent key of what it is called.
    pub label_key: String,
    /// The chords it answers to now; empty when unassigned.
    pub chords: Vec<KeyChord>,
    /// The chords it answers to out of the box.
    pub defaults: Vec<KeyChord>,
    pub rebindable: bool,
    pub category: ShortcutCategory,
    /// The rebindable action this row is, for a row that is one.
    pub action: Option<ShortcutAction>,
}

impl ShortcutEntry {
    /// Whether the current chords differ from the defaults.
    #[must_use]
    pub fn is_changed(&self) -> bool {
        self.chords != self.defaults
    }
}

/// Every desktop shortcut under `config`: the rebindable actions in
/// [`ShortcutAction::ALL`] order, then the fixed shortcuts.
#[must_use]
pub fn catalogue(config: &ShortcutConfig) -> Vec<ShortcutEntry> {
    let defaults = ShortcutConfig::default();
    ShortcutAction::ALL
        .into_iter()
        .map(|action| ShortcutEntry {
            id: action.id(),
            label_key: action.label_key(),
            chords: config.bindings.get(&action).cloned().into_iter().collect(),
            defaults: defaults
                .bindings
                .get(&action)
                .cloned()
                .into_iter()
                .collect(),
            rebindable: true,
            category: action.category(),
            action: Some(action),
        })
        .chain(FIXED_SHORTCUTS.iter().map(|fixed| ShortcutEntry {
            id: fixed.id,
            label_key: fixed.label_key.to_string(),
            chords: fixed.chords(),
            defaults: fixed.chords(),
            rebindable: false,
            category: fixed.category,
            action: None,
        }))
        .collect()
}

/// The catalogue row `id` names under `config`.
#[must_use]
pub fn entry(config: &ShortcutConfig, id: &str) -> Option<ShortcutEntry> {
    catalogue(config).into_iter().find(|entry| entry.id == id)
}

impl ShortcutConfig {
    /// Put `action` back on its default chord.
    ///
    /// # Errors
    ///
    /// The same refusals as [`Self::set`]: another action or a published
    /// action holds the default now, and keeps it. The table is then
    /// unchanged.
    pub fn reset(&mut self, action: ShortcutAction) -> Result<(), ShortcutConfigError> {
        let chord = Self::default()
            .bindings
            .remove(&action)
            .ok_or(ShortcutConfigError::InvalidChord)?;
        self.set(action, chord)
    }

    /// Put every desktop action back on its default chord. Chords bound to
    /// published actions stay as they are.
    ///
    /// # Errors
    ///
    /// [`ShortcutConfigError::TakenByAction`] when a published action now
    /// holds one of the defaults; the table is then unchanged.
    pub fn reset_all(&mut self) -> Result<(), ShortcutConfigError> {
        let defaults = Self::default();
        if defaults
            .bindings
            .values()
            .any(|chord| self.action_for(chord).is_some())
        {
            return Err(ShortcutConfigError::TakenByAction);
        }
        self.bindings = defaults.bindings;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(text: &str) -> KeyChord {
        KeyChord::parse(text).expect("a well-formed chord")
    }

    #[test]
    fn ids_are_unique_and_round_trip() {
        let mut seen = BTreeSet::new();
        for action in ShortcutAction::ALL {
            assert_eq!(ShortcutAction::from_id(action.id()), Some(action));
            assert!(seen.insert(action.id()), "{} listed twice", action.id());
        }
        for fixed in RESERVED_SHORTCUTS {
            assert!(fixed.id.starts_with("reserved-"), "{}", fixed.id);
            assert!(seen.insert(fixed.id), "{} listed twice", fixed.id);
        }
        for fixed in FIXED_SHORTCUTS {
            assert!(fixed.id.starts_with("fixed-"), "{}", fixed.id);
            assert!(seen.insert(fixed.id), "{} listed twice", fixed.id);
            assert!(!fixed.keys.is_empty(), "{} has no chord", fixed.id);
        }
    }

    #[test]
    fn every_action_with_a_default_is_listed() {
        let listed: BTreeSet<_> = ShortcutAction::ALL.into_iter().collect();
        let defaulted: BTreeSet<_> = ShortcutConfig::default().bindings.into_keys().collect();
        assert_eq!(listed, defaulted);
        assert_eq!(
            catalogue(&ShortcutConfig::default()).len(),
            listed.len() + FIXED_SHORTCUTS.len()
        );
    }

    /// Super+Up/Down maximize and restore, Ctrl+Super+Up/Down switch strips,
    /// Shift+Ctrl+Super+Up/Down carry the focused group there, Super+1…9 go
    /// to a strip and Ctrl+Super+1…9 to a window of it. None of these is a
    /// fixed or reserved desktop chord, and each is one catalogue row.
    #[test]
    fn maximize_and_strip_chords_are_free_and_listed_once() {
        let config = ShortcutConfig::default();
        let rows = catalogue(&config);
        let mut expected = vec![
            ("maximize-window", "Super+Up".to_string()),
            ("restore-window", "Super+Down".to_string()),
            ("previous-strip", "Super+Ctrl+Up".to_string()),
            ("next-strip", "Super+Ctrl+Down".to_string()),
            (
                "move-group-to-previous-strip",
                "Super+Ctrl+Shift+Up".to_string(),
            ),
            (
                "move-group-to-next-strip",
                "Super+Ctrl+Shift+Down".to_string(),
            ),
        ];
        let strips = [
            "focus-strip-1",
            "focus-strip-2",
            "focus-strip-3",
            "focus-strip-4",
            "focus-strip-5",
            "focus-strip-6",
            "focus-strip-7",
            "focus-strip-8",
            "focus-strip-9",
        ];
        let groups = [
            "focus-group-1",
            "focus-group-2",
            "focus-group-3",
            "focus-group-4",
            "focus-group-5",
            "focus-group-6",
            "focus-group-7",
            "focus-group-8",
            "focus-group-9",
        ];
        for (n, (strip, group)) in strips.into_iter().zip(groups).enumerate() {
            expected.push((strip, format!("Super+{}", n + 1)));
            expected.push((group, format!("Super+Ctrl+{}", n + 1)));
        }
        for (id, shown) in expected {
            let chord = chord(&shown);
            assert!(
                desktop_owner(&chord).is_none(),
                "{shown} is a fixed or reserved chord"
            );
            let holders: Vec<_> = rows
                .iter()
                .filter(|entry| entry.chords.contains(&chord))
                .map(|entry| entry.id)
                .collect();
            assert_eq!(holders, vec![id], "{shown} belongs to {id} alone");
        }
        let maximize = entry(&config, "maximize-window").expect("listed");
        assert_eq!(maximize.category, ShortcutCategory::Windows);
        assert_eq!(maximize.label_key, "shortcut-name-maximize-window");
        let strip = entry(&config, "focus-strip-1").expect("listed");
        assert_eq!(strip.category, ShortcutCategory::Navigation);
        assert_eq!(
            ShortcutAction::from_id("focus-strip-1").and_then(ShortcutAction::focus_strip_index),
            Some(0)
        );
    }

    #[test]
    fn a_rebind_onto_another_actions_chord_names_it_and_changes_nothing() {
        let mut config = ShortcutConfig::default();
        let before = config.clone();
        assert_eq!(
            config.set(ShortcutAction::QuickSettings, chord("Super+Space")),
            Err(ShortcutConfigError::Conflict(ShortcutAction::Search))
        );
        assert_eq!(config, before, "the chord was not taken from search");
    }

    #[test]
    fn a_rebind_onto_a_fixed_chord_names_the_fixed_shortcut() {
        let mut config = ShortcutConfig::default();
        let before = config.clone();
        assert_eq!(
            config.set(ShortcutAction::Search, chord("Alt+Tab")),
            Err(ShortcutConfigError::Fixed("fixed-switch-windows"))
        );
        assert_eq!(
            config.set(ShortcutAction::NextTab, chord("Super+W")),
            Err(ShortcutConfigError::Fixed("fixed-close-tab"))
        );
        assert_eq!(config, before);
    }

    #[test]
    fn the_screen_reader_chord_is_fixed_and_wins_over_a_stored_binding() {
        let chord = chord("Ctrl+Super+Enter");
        assert_eq!(
            desktop_owner(&chord).map(|fixed| fixed.id),
            Some("fixed-toggle-screen-reader")
        );
        let mut config = ShortcutConfig::default();
        assert_eq!(
            config.set(ShortcutAction::OpenFiles, chord.clone()),
            Err(ShortcutConfigError::Fixed("fixed-toggle-screen-reader"))
        );
        // One saved before the desktop fixed it never runs; the action
        // keeps its default instead of a dead chord.
        let mut stored = ShortcutConfig::default();
        stored.bindings.insert(ShortcutAction::OpenFiles, chord);
        assert_eq!(
            ShortcutConfig::normalise(stored)
                .bindings
                .get(&ShortcutAction::OpenFiles),
            ShortcutConfig::default()
                .bindings
                .get(&ShortcutAction::OpenFiles)
        );
    }

    #[test]
    fn an_action_may_keep_a_default_the_desktop_also_answers() {
        // Super+Page Up is both the fixed workspace change and the default
        // of Previous workspace: moving it away and back is not a conflict.
        let mut config = ShortcutConfig::default();
        config
            .set(ShortcutAction::PreviousWorkspace, chord("Super+Alt+Up"))
            .expect("a free chord");
        config
            .reset(ShortcutAction::PreviousWorkspace)
            .expect("its own default");
        assert_eq!(config, ShortcutConfig::default());
        assert_eq!(
            config.set(ShortcutAction::NextTab, chord("Super+PageUp")),
            Err(ShortcutConfigError::Conflict(
                ShortcutAction::PreviousWorkspace
            ))
        );
    }

    #[test]
    fn reserved_chords_are_refused_to_actions_and_published_actions() {
        let mut config = ShortcutConfig::default();
        assert_eq!(
            config.set(ShortcutAction::Search, chord("Super+Ctrl+H")),
            Err(ShortcutConfigError::Fixed("reserved-vim-navigation"))
        );
        assert_eq!(
            config.check_action_chord(&chord("Escape"), None),
            Err(ShortcutConfigError::Fixed("reserved-dismiss"))
        );
        assert!(
            catalogue(&config)
                .iter()
                .all(|entry| !entry.id.starts_with("reserved-")),
            "reserved chords are not listed as shortcuts"
        );
    }

    #[test]
    fn a_rebind_onto_a_published_actions_chord_is_refused() {
        let mut config = ShortcutConfig::default();
        config
            .set_action(
                chord("Super+Shift+Y"),
                crate::hardware::ActionChordTarget {
                    provider: "workspace:room".into(),
                    resource: "drafts".into(),
                    action: "publish".into(),
                },
                "Run Publish",
            )
            .expect("a free chord");
        let before = config.clone();
        assert_eq!(
            config.set(ShortcutAction::Search, chord("Super+Shift+Y")),
            Err(ShortcutConfigError::TakenByAction)
        );
        assert_eq!(config, before);
    }

    #[test]
    fn reset_refuses_while_another_action_holds_the_default() {
        let mut config = ShortcutConfig::default();
        config
            .set(ShortcutAction::Search, chord("Super+J"))
            .expect("a free chord");
        config
            .set(ShortcutAction::QuickSettings, chord("Super+Space"))
            .expect("search gave it up");
        assert_eq!(
            config.reset(ShortcutAction::Search),
            Err(ShortcutConfigError::Conflict(ShortcutAction::QuickSettings))
        );
        config
            .reset_all()
            .expect("every default is free of action chords");
        assert_eq!(config, ShortcutConfig::default());
    }

    #[test]
    fn reset_all_keeps_a_default_a_published_action_took() {
        let mut config = ShortcutConfig::default();
        config
            .set(ShortcutAction::Search, chord("Super+J"))
            .expect("a free chord");
        config
            .set_action(
                chord("Super+Space"),
                crate::hardware::ActionChordTarget {
                    provider: "workspace:room".into(),
                    resource: "drafts".into(),
                    action: "publish".into(),
                },
                "Run Publish",
            )
            .expect("search gave the chord up");
        let before = config.clone();
        assert_eq!(config.reset_all(), Err(ShortcutConfigError::TakenByAction));
        assert_eq!(config, before);
    }

    #[test]
    fn search_and_lock_are_one_row_each_whose_rebind_replaces_the_system_chord() {
        let mut config = ShortcutConfig::default();
        let rows = catalogue(&config);
        for (id, default) in [("search", "Super+Space"), ("lock", "Super+L")] {
            let matching: Vec<_> = rows
                .iter()
                .filter(|entry| entry.chords.contains(&chord(default)))
                .collect();
            assert_eq!(matching.len(), 1, "{default} is listed once");
            assert_eq!(matching[0].id, id);
            assert!(matching[0].rebindable);
        }
        config
            .set(ShortcutAction::Lock, chord("Ctrl+Alt+L"))
            .expect("a free chord");
        config
            .set(ShortcutAction::Search, chord("Super+K"))
            .expect("a free chord");
        assert!(desktop_owner(&chord("Super+L")).is_none());
        assert!(desktop_owner(&chord("Super+Space")).is_none());
        assert!(
            config
                .bindings
                .values()
                .all(|bound| *bound != chord("Super+L") && *bound != chord("Super+Space")),
            "the old chords are free once rebound"
        );
    }

    #[test]
    fn the_catalogue_reports_current_and_default_chords() {
        let mut config = ShortcutConfig::default();
        config
            .set(ShortcutAction::QuickSettings, chord("Super+J"))
            .expect("a free chord");
        let quick = entry(&config, "quick-settings").expect("listed");
        assert_eq!(quick.chords, vec![chord("Super+J")]);
        assert_eq!(quick.defaults, vec![chord("Super+Q")]);
        assert!(quick.rebindable && quick.is_changed());
        assert_eq!(quick.category, ShortcutCategory::System);
        assert_eq!(quick.label_key, "shortcut-name-quick-settings");
        let close = entry(&config, "fixed-close-tab").expect("listed");
        assert!(!close.rebindable);
        assert_eq!(close.chords, vec![chord("Alt+F4"), chord("Super+W")]);
    }

    #[test]
    fn every_dispatched_fixed_action_is_one_catalogue_entry_with_its_chords() {
        let actions = [
            FixedAction::CloseTab,
            FixedAction::NewWindow,
            FixedAction::DuplicateTab,
            FixedAction::OpenTerminal,
            FixedAction::StripSwitcher,
            FixedAction::CaptureScreenshot,
            FixedAction::PictureInPicture,
            FixedAction::SaveRecentReplay,
            FixedAction::WorkspaceHistory,
            FixedAction::ExploreWorkspaceHistory,
            FixedAction::ShortcutGuide,
            FixedAction::StopRemoteInput,
        ];
        for action in actions {
            let listed = fixed_shortcut(action.id()).expect("the action is a fixed shortcut");
            assert_eq!(
                FIXED_SHORTCUTS
                    .iter()
                    .filter(|fixed| FixedAction::from_id(fixed.id) == Some(action))
                    .count(),
                1,
                "{} names one action",
                action.id()
            );
            for chord in listed.chords() {
                assert_eq!(fixed_action(&chord), Some(action), "{chord:?}");
            }
        }
        // The chords the keys answer where they are resolved, and the
        // accessibility ones, name no action here.
        for (text, id) in [
            ("Alt+Tab", "fixed-switch-windows"),
            ("Super+PageUp", "fixed-change-workspace"),
            ("Super+P", "fixed-display-layout"),
            ("PrintScreen", "fixed-screenshot"),
            ("Ctrl+Super+Enter", "fixed-toggle-screen-reader"),
            ("Alt+Shift+NumLock", "fixed-toggle-mouse-keys"),
        ] {
            assert_eq!(
                desktop_owner(&chord(text)).map(|fixed| fixed.id),
                Some(id),
                "{text} is fixed"
            );
            assert_eq!(fixed_action(&chord(text)), None, "{text} needs no action");
        }
    }
}
