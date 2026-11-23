//! The desktop's keyboard shortcuts as one typed catalogue.
//!
//! Every surface that lists or changes a desktop shortcut reads this module:
//! the shell publishes it on the action API as the `keyboard-shortcuts`
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ShortcutCategory {
    System,
    Navigation,
    Windows,
    Apps,
    Capture,
    Accessibility,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShortcutHome {
    Shell,
    Session,
}

impl ShortcutCategory {
    pub const ALL: [Self; 6] = [
        Self::System,
        Self::Navigation,
        Self::Windows,
        Self::Apps,
        Self::Capture,
        Self::Accessibility,
    ];

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

    #[must_use]
    pub fn label_key(self) -> String {
        format!("shortcut-category-{}", self.id())
    }
}

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

#[must_use]
pub fn fixed_shortcut(id: &str) -> Option<&'static FixedShortcut> {
    FIXED_SHORTCUTS
        .iter()
        .chain(RESERVED_SHORTCUTS)
        .find(|fixed| fixed.id == id)
}

#[must_use]
pub fn desktop_owner(chord: &KeyChord) -> Option<&'static FixedShortcut> {
    FIXED_SHORTCUTS
        .iter()
        .chain(RESERVED_SHORTCUTS)
        .find(|fixed| fixed.owns(chord))
}

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortcutEntry {
    pub id: &'static str,
    pub label_key: String,
    pub chords: Vec<KeyChord>,
    pub defaults: Vec<KeyChord>,
    pub rebindable: bool,
    pub category: ShortcutCategory,
    pub action: Option<ShortcutAction>,
}

impl ShortcutEntry {
    #[must_use]
    pub fn is_changed(&self) -> bool {
        self.chords != self.defaults
    }
}

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

#[must_use]
pub fn entry(config: &ShortcutConfig, id: &str) -> Option<ShortcutEntry> {
    catalogue(config).into_iter().find(|entry| entry.id == id)
}

impl ShortcutConfig {
    pub fn reset(&mut self, action: ShortcutAction) -> Result<(), ShortcutConfigError> {
        let chord = Self::default()
            .bindings
            .remove(&action)
            .ok_or(ShortcutConfigError::InvalidChord)?;
        self.set(action, chord)
    }

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

    tilo_test_env::scenario! {
        fn a_rebind_onto_a_taken_fixed_or_reserved_chord_is_refused;

        phase a_rebind_onto_another_actions_chord_names_it_and_changes_nothing;
        {
            let mut config = ShortcutConfig::default();
            let before = config.clone();
            assert_eq!(
                config.set(ShortcutAction::QuickSettings, chord("Super+Space")),
                Err(ShortcutConfigError::Conflict(ShortcutAction::Search))
            );
            assert_eq!(config, before, "the chord was not taken from search");
        }

        phase a_rebind_onto_a_fixed_chord_names_the_fixed_shortcut;
        {
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

        phase the_screen_reader_chord_is_fixed_and_wins_over_a_stored_binding;
        {
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

        phase an_action_may_keep_a_default_the_desktop_also_answers;
        {
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

        phase reserved_chords_are_refused_to_actions_and_published_actions;
        {
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

        phase a_rebind_onto_a_published_actions_chord_is_refused;
        {
            let mut config = ShortcutConfig::default();
            config
                .set_action(
                    chord("Super+Shift+Y"),
                    crate::hardware::ActionChordTarget {
                        provider: "workspace:w1".into(),
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
    }

    tilo_test_env::scenario! {
        fn a_reset_never_takes_a_chord_another_binding_holds;

        phase reset_refuses_while_another_action_holds_the_default;
        {
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

        phase reset_all_keeps_a_default_a_published_action_took;
        {
            let mut config = ShortcutConfig::default();
            config
                .set(ShortcutAction::Search, chord("Super+J"))
                .expect("a free chord");
            config
                .set_action(
                    chord("Super+Space"),
                    crate::hardware::ActionChordTarget {
                        provider: "workspace:w1".into(),
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
    }

    tilo_test_env::scenario! {
        fn the_shortcut_catalogue_lists_every_action_once_with_its_chords;

        phase ids_are_unique_and_round_trip;
        {
            let mut seen = BTreeSet::new();
            for action in ShortcutAction::ALL {
                assert_eq!(ShortcutAction::from_id(action.id()), Some(action));
                assert!(seen.insert(action.id()), "{} listed twice", action.id());
            }
            for fixed in RESERVED_SHORTCUTS {
                assert!(seen.insert(fixed.id), "{} listed twice", fixed.id);
            }
            for fixed in FIXED_SHORTCUTS {
                assert!(seen.insert(fixed.id), "{} listed twice", fixed.id);
                assert!(!fixed.keys.is_empty(), "{} has no chord", fixed.id);
            }
        }

        phase every_action_with_a_default_is_listed;
        {
            let listed: BTreeSet<_> = ShortcutAction::ALL.into_iter().collect();
            let defaulted: BTreeSet<_> = ShortcutConfig::default().bindings.into_keys().collect();
            assert_eq!(listed, defaulted);
            assert_eq!(
                catalogue(&ShortcutConfig::default()).len(),
                listed.len() + FIXED_SHORTCUTS.len()
            );
        }

        phase every_rebindable_default_belongs_to_one_row;
        // Super+Up/Down maximize and restore, Ctrl+Super+Up/Down switch strips,
        // Shift+Ctrl+Super+Up/Down carry the focused group there. None of these is a
        // fixed or reserved desktop chord, and each is one catalogue row.
        {
            let config = ShortcutConfig::default();
            let rows = catalogue(&config);
            let mut seen = BTreeSet::new();
            for row in rows.iter().filter(|row| row.rebindable) {
                for c in &row.chords {
                    assert!(seen.insert(c.clone()), "{} is listed twice", c.display());
                }
            }
            for (id, shown) in [
                ("maximize-window", "Super+Up"),
                ("restore-window", "Super+Down"),
                ("previous-strip", "Super+Ctrl+Up"),
                ("next-strip", "Super+Ctrl+Down"),
                ("move-group-to-previous-strip", "Super+Ctrl+Shift+Up"),
                ("move-group-to-next-strip", "Super+Ctrl+Shift+Down"),
            ] {
                assert!(
                    desktop_owner(&chord(shown)).is_none(),
                    "{shown} is a fixed or reserved chord"
                );
                assert!(
                    entry(&config, id)
                        .expect("listed")
                        .chords
                        .contains(&chord(shown)),
                    "{shown} belongs to {id}"
                );
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

        phase search_and_lock_are_one_row_each_whose_rebind_replaces_the_system_chord;
        {
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
        }

        phase every_dispatched_fixed_action_is_one_catalogue_entry_with_its_chords;
        {
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
            for (text, id) in [
                ("Alt+Tab", "fixed-switch-windows"),
                ("Ctrl+Super+Enter", "fixed-toggle-screen-reader"),
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

    tilo_test_env::scenario! {
        fn a_saved_rebind_keeps_only_what_the_person_changed_and_repairs_damage;

        phase app_chords_and_fixed_desktop_chords_stay_out_of_the_desktop_defaults;
        {
            let cfg = ShortcutConfig::default();
            let unique: BTreeSet<_> = cfg.bindings.values().collect();
            assert_eq!(unique.len(), cfg.bindings.len(), "no two defaults share a chord");
            for app_chord in ["Ctrl+Tab", "Ctrl+Shift+Tab", "Ctrl+Shift+T"] {
                let chord = KeyChord::parse(app_chord).unwrap();
                assert!(
                    cfg.bindings.values().all(|bound| *bound != chord),
                    "{app_chord} belongs to the focused app, not to a desktop default"
                );
            }
            assert!(cfg.validate().is_ok(), "no other default claims the chords");
        }

        phase a_chord_is_parsed_normalised_and_refused_when_printable_alone;
        {
            assert_eq!(KeyChord::parse("meta + K").unwrap().display(), "Super+k");
            assert!(KeyChord::parse("k").is_err());
        }

        phase an_edit_onto_a_taken_or_reserved_chord_changes_nothing;
        {
            let mut cfg = ShortcutConfig::default();
            let old = cfg.bindings[&ShortcutAction::Lock].clone();
            assert_eq!(
                cfg.set(
                    ShortcutAction::Lock,
                    KeyChord::parse("Super+Space").unwrap()
                ),
                Err(ShortcutConfigError::Conflict(ShortcutAction::Search))
            );
            assert_eq!(cfg.bindings[&ShortcutAction::Lock], old);
            for key in ["f1", "printscreen"] {
                assert_eq!(
                    cfg.set(
                        ShortcutAction::Search,
                        KeyChord {
                            modifiers: BTreeSet::from(["Ctrl".to_string()]),
                            key: key.into(),
                        },
                    ),
                    Err(ShortcutConfigError::ReservedSystemKey),
                    "{key} must remain compositor-owned"
                );
            }
            cfg.set(ShortcutAction::Search, KeyChord::parse("Ctrl+J").unwrap())
                .unwrap();
            assert_eq!(
                cfg.set(ShortcutAction::NextTab, KeyChord::parse("Ctrl+J").unwrap()),
                Err(ShortcutConfigError::Conflict(ShortcutAction::Search))
            );
        }

        phase a_corrupt_table_is_repaired_on_load;
        {
            let mut corrupt = ShortcutConfig::default();
            corrupt.bindings.insert(
                ShortcutAction::Search,
                KeyChord {
                    modifiers: BTreeSet::from(["Ctrl".to_string()]),
                    key: "f1".into(),
                },
            );
            corrupt
                .bindings
                .insert(ShortcutAction::Lock, KeyChord::parse("Alt+L").unwrap());
            corrupt.bindings.insert(
                ShortcutAction::PreviousTab,
                KeyChord::parse("Ctrl+J").unwrap(),
            );
            corrupt
                .bindings
                .insert(ShortcutAction::NextTab, KeyChord::parse("Ctrl+J").unwrap());

            let repaired = ShortcutConfig::normalise(corrupt);
            assert_eq!(
                repaired.bindings[&ShortcutAction::Search].display(),
                "Super+Space",
                "reserved persisted keys revert to their action default"
            );
            assert_eq!(
                repaired.bindings[&ShortcutAction::Lock].display(),
                "Alt+l",
                "the lock chord is rebindable like every other action"
            );
            assert_eq!(
                repaired.bindings[&ShortcutAction::PreviousTab].display(),
                "Ctrl+j",
                "the first action in stable action order owns a duplicate"
            );
            assert_eq!(
                repaired.bindings[&ShortcutAction::NextTab].display(),
                "Super+Ctrl+Tab",
                "the duplicate action deterministically falls back to default"
            );
            assert!(repaired.validate().is_ok());
        }

        phase a_save_keeps_only_the_chord_a_person_changed;
        {
            let dir = crate::test_dir();
            let store = crate::profile(dir.path());

            let mut saved = ShortcutConfig::default();
            saved
                .set(ShortcutAction::NextTab, KeyChord::parse("Ctrl+J").unwrap())
                .unwrap();
            saved.save_in(&*store).unwrap();
            assert_eq!(
                crate::SettingsSource::load(&*store)["shortcuts"]["bindings"],
                serde_json::json!({"next_tab": {"modifiers": ["Ctrl"], "key": "j"}}),
                "the file keeps only the chord the person changed"
            );
            let reloaded = ShortcutConfig::load_in(&*store);
            assert_eq!(reloaded, saved, "the defaults plus that one chord");
            assert!(reloaded.validate().is_ok());
        }

        phase a_chord_runs_any_published_action_but_never_takes_a_desktop_chord;
        {
            let dir = crate::test_dir();
            let store = crate::profile(dir.path());
            let target = |resource: &str| crate::hardware::ActionChordTarget {
                provider: "workspace:w1".into(),
                resource: resource.into(),
                action: "run".into(),
            };
            let mut cfg = ShortcutConfig::default();
            let chord = KeyChord::parse("Super+Shift+N").unwrap();
            cfg.set_action(chord.clone(), target("shortcuts.shortcut:a"), "Run A")
                .unwrap();
            assert_eq!(
                cfg.action_for(&chord).map(|bound| bound.label.as_str()),
                Some("Run A")
            );
            assert_eq!(
                cfg.set_action(chord.clone(), target("shortcuts.shortcut:b"), "Run B"),
                Err(ShortcutConfigError::TakenByAction),
                "one chord runs one action"
            );
            assert_eq!(
                cfg.set_action(
                    KeyChord::parse("Alt+Tab").unwrap(),
                    target("shortcuts.shortcut:b"),
                    "Run B"
                ),
                Err(ShortcutConfigError::Fixed("fixed-switch-windows"))
            );
            assert_eq!(
                cfg.set_action(
                    KeyChord::parse("Super+Space").unwrap(),
                    target("shortcuts.shortcut:b"),
                    "Run B"
                ),
                Err(ShortcutConfigError::Conflict(ShortcutAction::Search))
            );
            assert_eq!(
                cfg.set(ShortcutAction::Search, chord.clone()),
                Err(ShortcutConfigError::TakenByAction),
                "a desktop action cannot take an action's chord either"
            );
            cfg.save_in(&*store).unwrap();
            let reloaded = ShortcutConfig::load_in(&*store);
            assert_eq!(reloaded, cfg);
            assert!(reloaded.validate().is_ok());
            assert!(cfg.remove_action(&target("shortcuts.shortcut:a")));
            assert!(cfg.action_for(&chord).is_none());
        }
    }
}
