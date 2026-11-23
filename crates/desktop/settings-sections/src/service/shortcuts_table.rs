//! The person's keyboard shortcuts as a reading a view shows: every desktop shortcut with the
//! chord it is on now, and every chord that runs an action.
//!
//! The section (`settings.shortcuts`) keeps its table as the compositor reads it, opaque to a
//! view; this is the same stored table read as records, so a pane lists the chords and the
//! section's own `rebind`, `reset` and `reset-all` change them. It is a reading beside the
//! section, never a second copy: every change goes through the section.

use serde::{Deserialize, Serialize};

/// The resource the reading is published as.
pub const RESOURCE: &str = "settings/shortcuts-table";
/// Its kind, as the reading's type is named and a view reads `settings.shortcuts_table`.
pub const KIND: &str = "settings.shortcuts-table";

/// One desktop shortcut and the chord it is on.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct ShortcutChord {
    /// The shortcut's id, as `rebind` and `reset` name it.
    pub id: String,
    /// Its keys, as a person writes them (`Ctrl+Shift+K`).
    pub chord: String,
}

/// One keyboard trigger that runs an action, and its chord.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct ActionShortcut {
    /// What the trigger runs, as the person who made it named it.
    pub label: String,
    /// Its keys, as a person writes them.
    pub chord: String,
}

/// The shortcuts table as a view reads it.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct ShortcutsTable {
    /// Every desktop shortcut, in the order the table keeps them.
    pub bindings: Vec<ShortcutChord>,
    /// Every chord that runs an action.
    pub actions: Vec<ActionShortcut>,
}

impl ShortcutsTable {
    /// The table `stored`, the shortcuts section as the settings document holds it.
    #[must_use]
    pub fn of(stored: &serde_json::Value) -> Self {
        let section = stored.as_object().cloned().unwrap_or_default();
        let config = settings_store::hardware::ShortcutConfig::from_section(section);
        let bindings = config
            .bindings
            .iter()
            .map(|(action, chord)| ShortcutChord {
                id: action.id().to_owned(),
                chord: chord.display(),
            })
            .collect();
        let actions = config
            .actions
            .iter()
            .map(|action| ActionShortcut {
                label: action.label.clone(),
                chord: action.chord.display(),
            })
            .collect();
        Self { bindings, actions }
    }
}

/// The shortcuts table as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "settings.shortcuts-table", label = "Keyboard shortcuts")]
pub struct ShortcutsTableReading;

/// The reading, as the settings service publishes it beside the section. It offers no verb of
/// its own: a change is the section's.
#[must_use]
pub fn surface() -> tilo_actions::surface::Surface {
    tilo_resource::surface::reading_surface::<ShortcutsTable, tilo_resource::NoEffect>(
        KIND,
        "Keyboard shortcuts",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_table_reads_every_shortcut_with_its_chord() {
        let table = ShortcutsTable::of(&serde_json::json!({}));
        assert_eq!(
            table.bindings.len(),
            settings_store::hardware::ShortcutAction::ALL.len(),
            "every shortcut the section knows is listed"
        );
        let search = table
            .bindings
            .iter()
            .find(|binding| binding.id == "search")
            .expect("the search shortcut is listed");
        assert_eq!(search.chord, "Super+Space");
        assert!(
            table.actions.is_empty(),
            "nothing runs on a chord of its own"
        );
    }

    #[test]
    fn a_changed_chord_reads_as_the_section_keeps_it() {
        let stored = serde_json::json!({
            "bindings": {"search": {"modifiers": ["Ctrl"], "key": "k"}},
        });
        let table = ShortcutsTable::of(&stored);
        let search = table
            .bindings
            .iter()
            .find(|binding| binding.id == "search")
            .expect("the search shortcut is listed");
        assert_eq!(search.chord, "Ctrl+k");
    }
}
