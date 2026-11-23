//! The `firmware` resource: what [`tilo_system_resources::firmware`] says it is.
//!
//! fwupd answers the inventory and history; installing every offered update runs its transaction
//! to the end, and the reading lands with what it installed. fwupd stays the verifier and the
//! policy authority, and nothing here passes force or downgrade flags.

use tilo_actions::ResourceSpec;
use tilo_resource::Effect as _;
use tilo_service::offer;
use tilo_system_resources::firmware::{
    Firmware, FirmwareAction, FirmwareHistoryEntry, FirmwareUpdate, FIRMWARE,
};

use crate::i18n::name;

/// The `firmware` resource, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.system.firmware", label = "Firmware")]
pub struct FirmwareReading;

/// The `firmware` resource reading `firmware`.
#[must_use]
pub fn resource(_firmware: &Firmware) -> ResourceSpec {
    offer::<FirmwareAction>(tilo_resource::typed_resource::<FirmwareReading>(
        FIRMWARE,
        name("so.tilo.system.firmware", "Firmware"),
    ))
}

/// The firmware inventory and history fwupd answers with now.
#[must_use]
pub fn reading() -> Firmware {
    let updates = os_actions::get_firmware_updates();
    let history = os_actions::get_firmware_history();
    let problem = match (&updates, &history) {
        (Err(error), _) | (_, Err(error)) => Some(error.clone()),
        (Ok(_), Ok(_)) => None,
    };
    Firmware {
        available: updates.is_ok() || history.is_ok(),
        updates: updates
            .unwrap_or_default()
            .into_iter()
            .map(update_reading)
            .collect(),
        history: history
            .unwrap_or_default()
            .into_iter()
            .map(history_reading)
            .collect(),
        problem,
        ..Firmware::default()
    }
}

/// Read fwupd's metadata again, then what it offers.
///
/// # Errors
/// fwupd did not refresh its metadata.
pub fn refresh() -> Result<(), String> {
    os_actions::refresh_firmware_metadata()
}

/// Install every firmware update offered now, and report what landed.
///
/// # Errors
/// There is nothing to install, the machine is not ready, or the transaction failed.
pub fn install() -> Result<(), String> {
    let updates = os_actions::get_firmware_updates()?;
    let events = os_actions::start_firmware_install(&updates)?;
    let mut outcome = None;
    for event in events {
        match event {
            os_actions::FirmwareEvent::Started { .. }
            | os_actions::FirmwareEvent::Progress { .. } => {}
            os_actions::FirmwareEvent::Completed(installed) => outcome = Some(installed),
            os_actions::FirmwareEvent::Failed(error) => return Err(error),
        }
    }
    if outcome.is_some() {
        Ok(())
    } else {
        Err("the firmware install ended without saying what it installed".to_owned())
    }
}

fn update_reading(update: os_actions::FirmwareUpdate) -> FirmwareUpdate {
    FirmwareUpdate {
        device_id: update.device_id,
        device: update.device,
        current_version: update.current_version,
        new_version: update.new_version,
        description: update.description,
        urgency: update.urgency,
        problems: update.problems,
    }
}

fn history_reading(entry: os_actions::FirmwareHistoryEntry) -> FirmwareHistoryEntry {
    FirmwareHistoryEntry {
        device: entry.device,
        from_version: entry.from_version,
        to_version: entry.to_version,
        state: entry.state,
        error: entry.error,
    }
}
