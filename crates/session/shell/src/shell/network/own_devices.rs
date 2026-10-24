//! This person's own devices as the network meets them: a device that
//! proves a certificate the person's root signed is kept as theirs, and a
//! revocation the root signed drops one (see `docs/device-mesh.md`).

use super::{ShellNetwork, personal_device_peer_path, personal_device_peers};

/// Where `person`'s own devices are recorded: the enrolment file and the
/// person's fleet in `fleet`.
pub(super) fn own_device_sources(
    person: Option<[u8; 32]>,
    fleet: Option<&crate::device_fleet::DeviceFleetStore>,
) -> personal_device_peers::Sources {
    personal_device_peers::Sources {
        peers: personal_device_peer_path(),
        fleet: person.zip(fleet.cloned()),
    }
}

impl ShellNetwork {
    /// Where this session finds the person's own devices.
    pub(super) fn own_device_sources(&self) -> personal_device_peers::Sources {
        own_device_sources(
            self.personal_continuity_user,
            self.own_device_fleet.as_ref(),
        )
    }

    /// The person's other devices, by endpoint key, as the worker last read
    /// them.
    pub(crate) fn personal_device_keys(&self) -> Vec<[u8; 32]> {
        let local = *self.local_peer_id.as_bytes();
        self.personal_device_peers.lock().map_or_else(
            |_| Vec::new(),
            |mut peers| {
                peers
                    .devices_at(|| self.own_device_sources())
                    .iter()
                    .copied()
                    .filter(|device| *device != local)
                    .collect()
            },
        )
    }

    /// The shell's peer endpoint, which the person's devices reach it on.
    pub(crate) fn peer_endpoint(&self) -> Option<iroh::Endpoint> {
        self.private_chat_endpoint.clone()
    }

    /// Apply the revocations of this person's devices a peer carried: the
    /// revoked devices stop counting as theirs at once, and stay refused
    /// after a restart through the fleet.
    pub(super) fn learn_own_revocations(&self, revocations: &tilo_user_identity::RevocationList) {
        let Some(person) = self.personal_continuity_user else {
            return;
        };
        let Ok(mut peers) = self.personal_device_peers.lock() else {
            return;
        };
        let learned: Vec<tilo_user_identity::RevocationEntry> = revocations
            .iter()
            .filter(|entry| {
                entry.user_public_key == person
                    && !peers.fleet_revoked(&entry.revoked_device_public_key)
            })
            .cloned()
            .collect();
        if learned.is_empty() {
            return;
        }
        peers.revoke(learned.iter().map(|entry| entry.revoked_device_public_key));
        drop(peers);
        self.write_own_fleet(move |store, identity| {
            for entry in learned {
                if let Err(error) = store.remember_revocation(identity, entry) {
                    tracing::warn!(%error, "could not keep a revocation of this person's device");
                }
            }
        });
    }

    /// Count a device of this person that proved its certificate in an
    /// identity hello as theirs: trusted and dialed like an enrolled one,
    /// whichever of their devices enrolled it, and kept in the fleet so the
    /// next session does too.
    pub(super) fn keep_own_device(&self, cert: &tilo_user_identity::DeviceCert) {
        if self.personal_continuity_user != Some(cert.user_public_key)
            || !cert.capabilities.is_personal_device()
            || cert.device_public_key == *self.local_peer_id.as_bytes()
        {
            return;
        }
        let Ok(mut peers) = self.personal_device_peers.lock() else {
            return;
        };
        if peers.keeps(cert) || peers.is_revoked(&cert.device_public_key) {
            return;
        }
        peers.remember(cert.device_public_key);
        drop(peers);
        let cert = cert.clone();
        self.write_own_fleet(move |store, identity| {
            match store.remember_certificate(identity, cert) {
                Ok(_) => tracing::info!("kept another device of this person"),
                Err(error) => {
                    tracing::warn!(%error, "could not keep another device of this person");
                }
            }
        });
    }

    /// Change this person's fleet off the frame thread, after what is queued.
    fn write_own_fleet(
        &self,
        write: impl FnOnce(&crate::device_fleet::DeviceFleetStore, &str) + Send + 'static,
    ) {
        let (Some(person), Some(store)) =
            (self.personal_continuity_user, self.own_device_fleet.clone())
        else {
            return;
        };
        let submitted = crate::shell::durable_writer::global()
            .submit_ordered(move || write(&store, &hex::encode(person)));
        if let Err(error) = submitted {
            tracing::warn!(%error, "could not queue a change to this person's fleet");
        }
    }

    /// Read the person's devices again now, as the worker would after a
    /// change: for tests that rewrite them mid-session.
    #[cfg(test)]
    pub(super) fn recheck_personal_device_peers(&self) {
        if let Ok(peers) = self.personal_device_peers.lock() {
            peers.reread();
        }
    }

    /// Change the registered home-device ids, and drop the recipient set
    /// derived from them.
    pub(super) fn update_registered_home_device_ids(
        &self,
        update: impl FnOnce(&mut std::collections::HashSet<[u8; 32]>),
    ) {
        if let Ok(mut devices) = self.registered_home_device_ids.lock() {
            update(&mut devices);
        }
        if let Ok(mut peers) = self.personal_device_peers.lock() {
            peers.invalidate_recipients();
        }
    }
}
