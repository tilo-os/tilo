//! The person's devices that hold a session with this one now, as this device's daemon publishes
//! them for its person's shell.
//!
//! A device holds a link to this one when its session serves it: the link its records and calls
//! come over ([`ActionService::reached`]). What lasts about a device — what it is, its speaker
//! groups — is in the person's record of their devices; being there now is this, and it is kept
//! nowhere. The reading is the two together as far as the reader needs: the keys of the
//! certified devices that hold a link, and this device, whose session is what serves the shell.
//!
//! The reading moves when a link opens or ends, and when the record does, so a device the person
//! takes back leaves it at once.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{Value, json};
use tilo_peer_state::actions::ActionService;
use tilo_workspace_service::workspaces::FollowedRecord;
use tilo_workspace_service::workspaces::person_workspace::devices::{
    Devices, ONLINE, OnlineDevices,
};

use super::reading::{Changes, Read, Served};

/// The resource the person's devices online now are published under.
pub(crate) const RESOURCE: &str = ONLINE;

/// What holds the reading: the action service whose links it reads, the person and device this
/// daemon is, and the record that says which keys are the person's.
pub(crate) struct Online {
    actions: ActionService,
    person: [u8; 32],
    this: [u8; 32],
    devices: FollowedRecord,
    /// Moves with every change of the reading: what a follower reads again.
    moved: tokio::sync::watch::Sender<u64>,
}

impl Online {
    /// Publish the person's devices online now, read from `actions` and `devices`.
    #[must_use]
    pub(crate) fn new(
        actions: ActionService,
        this: [u8; 32],
        person: [u8; 32],
        devices: FollowedRecord,
    ) -> Arc<Self> {
        let (moved, _) = tokio::sync::watch::channel(0);
        let online = Arc::new(Self {
            actions,
            person,
            this,
            devices,
            moved,
        });
        let moved = online.moved.clone();
        let mut reached = online.actions.reached_changed();
        let mut record = online.devices.changes();
        tilo_rt::app().spawn(async move {
            loop {
                tokio::select! {
                    outcome = reached.changed() => {
                        if outcome.is_err() {
                            return;
                        }
                    }
                    outcome = record.changed() => {
                        if outcome.is_err() {
                            return;
                        }
                    }
                }
                moved.send_modify(|moved| *moved = moved.wrapping_add(1));
            }
        });
        online
    }

    /// The reading the person's shell follows over the shell link.
    pub(crate) fn published(self: &Arc<Self>) -> Served {
        let reading = Arc::clone(self);
        let changing = Arc::clone(self);
        let read: Arc<Read> =
            Arc::new(move |_caller: &tilo_app_host::services::Caller| reading.reading());
        let changes: Arc<Changes> = Arc::new(move || changing.moved.subscribe());
        Served::new(RESOURCE, read, changes)
    }

    /// The keys of the person's certified devices that are online now, this one among them.
    fn online(&self) -> BTreeSet<[u8; 32]> {
        let certified: BTreeSet<[u8; 32]> = self
            .devices
            .state::<Devices>()
            .map(|record| {
                record
                    .certified_at(&self.person, tilo_clock::unix_now_ns())
                    .into_iter()
                    .map(|cert| cert.device_public_key)
                    .collect()
            })
            .unwrap_or_default();
        let mut online: BTreeSet<[u8; 32]> = self
            .actions
            .reached()
            .into_iter()
            .filter(|device| certified.contains(device))
            .collect();
        online.insert(self.this);
        online
    }

    fn reading(&self) -> Value {
        let devices = OnlineDevices {
            devices: self.online().iter().map(hex::encode).collect(),
        };
        serde_json::to_value(devices).unwrap_or_else(|error| {
            tracing::warn!(%error, "the person's devices online now do not encode");
            json!({ "devices": [] })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_workspace_service::workspaces::person_workspace::devices::ONLINE;

    /// The resource this publishes is the one the workspace service names, so a session's follow
    /// and the daemon's offer cannot drift apart.
    #[test]
    fn the_reading_is_published_under_the_shared_name() {
        assert_eq!(RESOURCE, ONLINE);
    }
}
