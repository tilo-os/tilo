//! This device's keyboard shortcuts on the app door.
//! The catalogue is the shortcut guide's own ([`tilo_sheets::keyboard_shortcuts`]): apps asking for
//! the keymap read the same names and the same live chords the guide and Settings show, and
//! changing a shortcut stays the person's `shortcuts` settings section's action. The broker loads
//! the keymap and decides who may read it; this only answers from the table it hands over.

use std::sync::Arc;

use tilo_actions::ActionAddress;
use tilo_actions::door::ActionDescriptor;
use tilo_actions::outcome::{ActionFailure, ActionOutput};
use tilo_actions::remote::CatalogueRow;
use tilo_broker_core::services::KeyboardShortcutServices;
use tilo_sheets::keyboard_shortcuts as catalogue;

/// The device's shortcut table as the broker answers an app's call for it.
pub(crate) struct Door;

impl KeyboardShortcutServices for Door {
    fn app_id(&self) -> &str {
        catalogue::APP_ID
    }

    fn is_resource(&self, resource: &str) -> bool {
        catalogue::is_resource(resource)
    }

    fn catalogue_rows(
        &self,
        provider: &str,
        _: &settings_store::hardware::ShortcutConfig,
    ) -> Vec<CatalogueRow> {
        catalogue::catalogue_rows(provider)
    }

    fn descriptor(
        &self,
        _: &settings_store::hardware::ShortcutConfig,
        address: &ActionAddress,
    ) -> Option<ActionDescriptor> {
        catalogue::descriptor_at(address)
    }

    fn apply(
        &self,
        keymap: &settings_store::hardware::ShortcutConfig,
        address: &ActionAddress,
        argument: Option<&str>,
    ) -> Result<ActionOutput, Box<ActionFailure>> {
        catalogue::apply(keymap, address, argument)
    }
}

/// The seam this session hands the broker: the shortcut catalogue, read from the keymap the broker
/// loads.
pub(crate) fn servable() -> Arc<dyn KeyboardShortcutServices> {
    Arc::new(Door)
}

#[cfg(test)]
mod tests {
    use super::*;
    use settings_store::hardware::ShortcutConfig;

    /// An app asking for one shortcut reads the catalogue the guide shows: the row names its live
    /// chords, canonically and as the layout spells them, and refuses everything that is not the
    /// one action published.
    #[test]
    fn an_app_reads_the_shortcut_catalogue_through_the_door() {
        let door = servable();
        let keymap = ShortcutConfig::default();
        assert_eq!(door.app_id(), "so.tilo.settings");
        assert!(door.is_resource(catalogue::RESOURCE));

        let rows = door.catalogue_rows("desk", &keymap);
        assert_eq!(rows.len(), 1, "one resource carries the catalogue");
        let row = &rows[0];
        assert_eq!(row.provider, "desk");
        assert_eq!(row.resource.id, catalogue::RESOURCE);
        assert!(!row.has_children);
        assert_eq!(
            row.action.as_ref().map(|action| &action.address),
            Some(&ActionAddress::new(
                catalogue::RESOURCE,
                catalogue::LIST_ACTION
            ))
        );

        let listed = door
            .apply(
                &keymap,
                &ActionAddress::new(catalogue::RESOURCE, catalogue::LIST_ACTION),
                None,
            )
            .expect("listing the catalogue is answered")
            .value
            .expect("the list is the answer");
        let shortcuts = listed["shortcuts"]
            .as_array()
            .expect("the catalogue is a list");
        assert!(
            shortcuts.len() > settings_store::hardware::ShortcutAction::ALL.len(),
            "every rebindable action and every fixed shortcut is listed"
        );
        let terminal = shortcuts
            .iter()
            .find(|row| row["id"] == settings_store::hardware::ShortcutAction::OpenTerminal.id())
            .expect("the terminal shortcut is listed");
        assert_eq!(terminal["rebindable"], true);
        assert_eq!(terminal["defaults"], serde_json::json!(["Ctrl+Alt+T"]));

        assert!(
            door.apply(
                &keymap,
                &ActionAddress::new(catalogue::RESOURCE, "reset-all"),
                None
            )
            .is_err(),
            "changing a shortcut is the shortcuts settings section's action"
        );
        assert_eq!(
            door.descriptor(
                &keymap,
                &ActionAddress::new(catalogue::RESOURCE, "reset-all")
            ),
            None
        );
    }

    /// The catalogue's contract is a read of this device's own settings: asking for it grants
    /// nothing and writes nothing.
    #[test]
    fn the_catalogue_is_a_read_of_this_devices_settings() {
        let descriptor = catalogue::descriptor();
        assert_eq!(descriptor.contract.kind, tilo_actions::ActionKind::Read);
        assert_eq!(descriptor.address.resource, catalogue::RESOURCE);
        assert_eq!(descriptor.address.action, catalogue::LIST_ACTION);
        assert_eq!(
            catalogue::catalogue_rows("desk")[0].app_id,
            "so.tilo.settings",
            "the rows are checked against Settings, which administers them"
        );
    }
}
