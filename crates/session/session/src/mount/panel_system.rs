//! The firmware updates `fwupd` offers, carried from the system service to
//! quick settings.
//!
//! The system service reads the machine and publishes it as the
//! `system-update` resource: what update waits for a restart, and how many
//! firmware updates the daemon offers. The session lends the panel that
//! count ([`land`]), so the panel never asks `fwupd` itself and shows what
//! the service read.

use tilo_system_service::SYSTEM_UPDATE;
use tilo_system_service::update::Update;

use crate::Shared;

/// Land the firmware the system service offers, if it publishes it and this
/// session has a device to show it on. Returns whether the person's panel
/// has that reading now.
pub(crate) fn land(shared: &Shared) -> bool {
    let Some(device) = &shared.device else {
        return false;
    };
    let Some(reading) = shared.services.borrow().read(SYSTEM_UPDATE) else {
        return false;
    };
    match serde_json::from_value::<Update>(reading) {
        Ok(update) => {
            device.firmware(update.firmware);
            true
        }
        Err(error) => {
            tracing::warn!(%error, "the system update reading did not parse");
            false
        }
    }
}
