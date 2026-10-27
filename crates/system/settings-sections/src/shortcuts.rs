//! The person's keyboard shortcuts as the `shortcuts` section of their
//! settings: the chord each desktop shortcut is on and the chords that run
//! published actions, in the shape the compositor's key routing reads
//! ([`ShortcutConfig`]). Every change is one of the section's verbs, so
//! Settings, the shortcut guide, the palette, an agent and a script rebind a
//! key the same way, each change checked against every other chord and kept
//! in the section's history.

use serde::{Deserialize, Serialize};
use settings_store::hardware::{
    ActionChord, KeyChord, ShortcutAction, ShortcutConfig, ShortcutConfigError,
};
use ui::tilo_resource::{self, Refusal};

/// What the section stores of the table, as the table writes it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Stored(pub serde_json::Value);

tilo_resource::json_model!(Stored);

/// The person's keyboard shortcuts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[model(defaults)]
#[serde(default)]
pub struct ShortcutPrefs {
    /// The chord each desktop shortcut is on, by the shortcut's id; changed
    /// by `rebind`, `reset` and `reset-all`, which keep every chord
    /// unambiguous.
    #[model(readonly)]
    pub bindings: Stored,
    /// Chords that run a published action, such as a saved shortcut's
    /// keyboard trigger.
    #[model(readonly)]
    pub actions: Stored,
}

impl Default for ShortcutPrefs {
    fn default() -> Self {
        Self::of(&ShortcutConfig::default())
    }
}

impl ShortcutPrefs {
    /// The section holding `config`.
    #[must_use]
    pub fn of(config: &ShortcutConfig) -> Self {
        let stored = serde_json::to_value(config).unwrap_or_default();
        Self {
            bindings: Stored(
                stored
                    .get("bindings")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({})),
            ),
            actions: Stored(
                stored
                    .get("actions")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!([])),
            ),
        }
    }

    /// The table the section holds, repaired as the compositor reads it.
    #[must_use]
    pub fn config(&self) -> ShortcutConfig {
        let mut section = serde_json::Map::new();
        section.insert("bindings".into(), self.bindings.0.clone());
        if self
            .actions
            .0
            .as_array()
            .is_some_and(|actions| !actions.is_empty())
        {
            section.insert("actions".into(), self.actions.0.clone());
        }
        ShortcutConfig::from_section(section)
    }

    /// Change the table with `change`, keeping it only when `change` takes.
    fn change(
        &mut self,
        change: impl FnOnce(&mut ShortcutConfig) -> Result<(), Refusal>,
    ) -> Result<(), Refusal> {
        let mut config = self.config();
        change(&mut config)?;
        *self = Self::of(&config);
        Ok(())
    }
}

/// The rebindable shortcut `id` names.
fn shortcut(id: &str) -> Result<ShortcutAction, Refusal> {
    ShortcutAction::from_id(id).ok_or_else(|| Refusal::new(format!("`{id}` is no shortcut")))
}

fn refused(error: impl std::fmt::Display) -> Refusal {
    Refusal::new(error.to_string())
}

/// Why `chord` cannot go where it was asked to in `config`, naming what
/// holds it.
fn taken(config: &ShortcutConfig, chord: &KeyChord, error: ShortcutConfigError) -> Refusal {
    let shown = chord.display();
    Refusal::new(match error {
        ShortcutConfigError::Conflict(other) => {
            format!("`{shown}` is taken by the `{}` shortcut", other.id())
        }
        ShortcutConfigError::Fixed(id) => {
            format!("`{shown}` is the desktop's fixed `{id}` shortcut")
        }
        ShortcutConfigError::TakenByAction => match config.action_for(chord) {
            Some(bound) => format!("`{shown}` runs {}", bound.label),
            None => format!("`{shown}` runs another action"),
        },
        other => format!("`{shown}`: {other}"),
    })
}

impl ui::storage::SettingsSchema for ShortcutPrefs {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("shortcuts");
    ui::settings_codec!();
}

#[tilo_resource::resource(kind = "settings.shortcuts", label = "Keyboard shortcuts")]
impl ShortcutPrefs {
    /// Move a desktop shortcut to another chord. A chord another shortcut,
    /// a fixed desktop key or a published action holds is refused, naming
    /// it, and nothing changes.
    #[action(id = "rebind", keywords = "hotkey, keybinding, keyboard shortcut")]
    pub fn rebind(
        &mut self,
        #[param("The shortcut, by its id, as the keyboard shortcuts list names it")]
        shortcut: String,
        #[param("The chord, such as `Super+K` or `Ctrl+Alt+T`")] chord: String,
    ) -> Result<(), Refusal> {
        let action = self::shortcut(&shortcut)?;
        let chord = KeyChord::parse(&chord)
            .map_err(|reason| Refusal::new(format!("`{chord}`: {reason}")))?;
        self.change(|config| {
            config
                .set(action, chord.clone())
                .map_err(|error| taken(config, &chord, error))
        })
    }

    /// Put desktop shortcuts back on their default chords, together: two
    /// shortcuts that swapped chords swap back.
    #[action(id = "reset")]
    pub fn reset(
        &mut self,
        #[param("The shortcuts, by their ids")] shortcuts: Vec<String>,
    ) -> Result<(), Refusal> {
        let actions = shortcuts
            .iter()
            .map(|id| self::shortcut(id))
            .collect::<Result<Vec<_>, _>>()?;
        let defaults = ShortcutConfig::default();
        self.change(|config| {
            for action in &actions {
                config.bindings.remove(action);
            }
            for action in actions {
                let chord = defaults
                    .bindings
                    .get(&action)
                    .cloned()
                    .ok_or_else(|| Refusal::new(format!("`{}` has no default", action.id())))?;
                config
                    .set(action, chord.clone())
                    .map_err(|error| taken(config, &chord, error))?;
            }
            Ok(())
        })
    }

    /// Put every desktop shortcut back on its default chord. Chords that run
    /// published actions stay.
    #[action(id = "reset-all")]
    pub fn reset_all(&mut self) -> Result<(), Refusal> {
        self.change(|config| config.reset_all().map_err(refused))
    }

    /// Set the chords that run published actions (the keyboard triggers of
    /// the person's saved shortcuts) to exactly these; one a desktop
    /// shortcut has taken meanwhile is left out rather than taken back.
    #[action(id = "set-action-chords", housekeeping)]
    pub fn set_action_chords(
        &mut self,
        #[param("Each chord with the action it runs and its label")] chords: serde_json::Value,
    ) -> Result<(), Refusal> {
        let chords: Vec<ActionChord> = serde_json::from_value(chords).map_err(refused)?;
        self.change(|config| {
            config.actions.clear();
            for bound in chords {
                let _ = config.set_action(bound.chord, bound.target, bound.label);
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_resource::Replica;

    /// A rebind lands in the table the compositor reads, a clash changes
    /// nothing, and a reset puts the default back.
    #[test]
    fn a_shortcut_moves_only_to_a_free_chord() {
        let mut shortcuts = Replica::<ShortcutPrefs>::new();
        shortcuts
            .apply(ShortcutPrefs::REBIND.with(("quick-settings".to_owned(), "Super+J".to_owned())))
            .unwrap();
        let config = shortcuts.state().unwrap().config();
        assert_eq!(
            config.bindings[&ShortcutAction::QuickSettings].display(),
            "Super+j"
        );
        let before = shortcuts.state().unwrap();
        let refused = shortcuts
            .apply(
                ShortcutPrefs::REBIND.with(("quick-settings".to_owned(), "Super+Space".to_owned())),
            )
            .unwrap_err();
        assert!(format!("{refused:?}").contains("search"), "{refused:?}");
        assert_eq!(shortcuts.state().unwrap(), before);
        shortcuts
            .apply(ShortcutPrefs::RESET.with(vec!["quick-settings".to_owned()]))
            .unwrap();
        assert_eq!(
            shortcuts.state().unwrap().config(),
            ShortcutConfig::default()
        );
    }

    /// The section reads and writes the shape the compositor reads.
    #[test]
    fn the_section_is_the_table_the_compositor_reads() {
        let mut config = ShortcutConfig::default();
        config
            .set(
                ShortcutAction::OpenTerminal,
                KeyChord::parse("Super+Return").unwrap(),
            )
            .unwrap();
        let stored = serde_json::to_value(ShortcutPrefs::of(&config)).unwrap();
        let read = ShortcutConfig::from_section(stored.as_object().unwrap().clone());
        assert_eq!(read, config);
    }
}
