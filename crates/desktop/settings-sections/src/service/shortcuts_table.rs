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

/// One app command a window runs on keys: the keys its app gives it, and the keys it is on now.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
#[serde(default, rename_all = "kebab-case")]
pub struct AppCommand {
    /// The app, by its package id, as `rebind-command` and `reset-command` name it.
    pub app: String,
    /// The app's name, as people call it; named as a view reads it.
    #[serde(rename = "app_name")]
    pub app_name: String,
    /// The command, by the id of the action the app publishes.
    pub action: String,
    /// What the command is called.
    pub label: String,
    /// The keys its app gives it; empty when the app gives it none.
    pub usual: String,
    /// Its keys now, as a person writes them; empty for a command on no keys.
    pub chord: String,
    /// Whether the person moved it off the keys its app gives it.
    pub moved: bool,
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
    /// Every app command a window runs on keys, app by app, and every one the person moved.
    pub commands: Vec<AppCommand>,
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
        let commands = commands(&config, fleet_catalogue::apps());
        Self {
            bindings,
            actions,
            commands,
        }
    }
}

/// Every command of `apps` a window runs on keys, with the keys it is on as `config` moves it,
/// then each command `config` moved that no app of `apps` declares (an app being developed).
fn commands(
    config: &settings_store::hardware::ShortcutConfig,
    apps: &[fleet_catalogue::FleetApp],
) -> Vec<AppCommand> {
    let keys = |chord: Option<&settings_store::hardware::KeyChord>| {
        chord
            .map(settings_store::hardware::KeyChord::display)
            .unwrap_or_default()
    };
    let mut listed: Vec<AppCommand> = apps
        .iter()
        .flat_map(|app| {
            app.commands().map(move |command| {
                let moved = config
                    .commands_of(app.id)
                    .find(|moved| moved.action == command.action);
                AppCommand {
                    app: app.id.to_owned(),
                    app_name: app.name.to_owned(),
                    action: command.action.to_owned(),
                    label: command.label.to_owned(),
                    usual: command.key.to_owned(),
                    chord: moved.map_or_else(
                        || command.key.to_owned(),
                        |moved| keys(moved.chord.as_ref()),
                    ),
                    moved: moved.is_some(),
                }
            })
        })
        .collect();
    for moved in &config.commands {
        if !listed
            .iter()
            .any(|listed| listed.app == moved.app && listed.action == moved.action)
        {
            listed.push(AppCommand {
                app: moved.app.clone(),
                app_name: moved.app.clone(),
                action: moved.action.clone(),
                label: moved.action.clone(),
                usual: String::new(),
                chord: keys(moved.chord.as_ref()),
                moved: true,
            });
        }
    }
    listed
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

    #[test]
    fn every_app_command_on_keys_is_listed_and_a_moved_one_shows_where_it_went() {
        let (app, command) = fleet_catalogue::apps()
            .iter()
            .find_map(|app| app.commands().next().map(|command| (app, command)))
            .expect("an app of the fleet runs a command on keys");
        let listed = |table: &ShortcutsTable| {
            table
                .commands
                .iter()
                .filter(|listed| listed.app == app.id)
                .find(|listed| listed.action == command.action)
                .cloned()
                .expect("the command is listed")
        };
        let usual = listed(&ShortcutsTable::of(&serde_json::json!({})));
        assert_eq!(
            (usual.app_name.as_str(), usual.label.as_str()),
            (app.name, command.label),
            "the command is named by its app and its own label"
        );
        assert_eq!(usual.chord, command.key, "on the keys its app gives it");
        assert!(!usual.moved);

        let mut config = settings_store::hardware::ShortcutConfig::default();
        config
            .set_command(app.id, command.action, None)
            .expect("a command can be put on no keys");
        let stored = serde_json::to_value(config.stored()).expect("the table is stored as JSON");
        let moved = listed(&ShortcutsTable::of(&stored));
        assert!(moved.moved, "it reads as moved");
        assert_eq!(moved.chord, "", "on no keys");
        assert_eq!(moved.usual, command.key, "with the keys it is usually on");
    }
}
