//! The person's own store, open to their other devices while this session runs.
//! The store holds what this session's apps record in the person's own scope — `(app, personal)`
//! records, routed on the cluster their identity root derives — so a record written here reaches
//! their other devices and one written there reaches this session's apps. The broker keeps each
//! write in it as well as in the desktop (`HostServices::personal`), and this session answers the
//! store's link on the endpoint it already runs, beside the workspaces' traffic: its protocols are
//! fixed when it binds, so the link is handed over with the other protocols the endpoint serves
//! ([`crate::sync::Client::start`]).
//! Nothing here joins anything and no workspace is involved: the link proves which of the person's
//! devices is on the other end, and the store admits only certificates their identity root signed
//! ([`tilo_peer_state`]).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use tilo_action_broker::collab_broker::FanoutState;
use tilo_action_broker::personal::{self, Door, Person};
use tilo_broker_core::collab_core::BrokerChannels;
use tilo_network_actor::HostIdentity;
use tilo_peer_state::{PeerPersonal, PersonalSync};

use crate::options::Options;
use crate::sync::SiblingDials;

/// This session's door to the person's own store: the store itself, opened on the identity this
/// device acts as, and where it is kept.
pub(crate) struct Personal {
    sync: PersonalSync,
    root: [u8; 32],
    state: PathBuf,
}

impl Personal {
    /// The person's own store on this device, opened where this session keeps
    /// its state: one an earlier session wrote is read back.
    ///
    /// # Errors
    ///
    /// This session keeps no state, or the store kept there is not this
    /// device's to open: it cannot be read, or the device's certificate is
    /// not one the person's root signed that still holds.
    pub(crate) fn open(options: &Options, identity: &HostIdentity) -> Result<Self, String> {
        let Some(state) = options.state.as_deref() else {
            return Err("this session keeps no state to keep the person's records in".into());
        };
        let root = identity.device_cert.user_public_key;
        let store = personal::open(state, root, identity.device_cert.clone(), SystemTime::now())?;
        Ok(Self {
            sync: PersonalSync::over(PeerPersonal::new(store), Arc::clone(&identity.signer)),
            root,
            state: state.to_owned(),
        })
    }

    /// What answers the store's link on this session's endpoint: the session serves it beside its
    /// own traffic, from the pass the endpoint binds.
    pub(crate) fn protocol(&self) -> (Vec<u8>, Box<dyn iroh::protocol::DynProtocolHandler>) {
        (
            tilo_peer_state::PERSONAL_ALPN.to_vec(),
            Box::<dyn iroh::protocol::DynProtocolHandler>::from(self.sync.clone()),
        )
    }

    /// The store the broker keeps this session's own records in as well.
    pub(crate) fn sync(&self) -> &PersonalSync {
        &self.sync
    }

    /// Take the store to the person's other devices from `endpoint` : dial the ones this device has
    /// no link to, hand what they write to this session's apps through `channels` and `fanout` ,
    /// and keep the store beside this session's state as it changes.
    /// `dials` opens the other lane to the same devices: the session between two of the person's
    /// desks is where they tell each other what they hold — their workspaces and their hosts — so
    /// one adopts what the other keeps. Without it a device keeps only what it was there to make.
    /// The session must answer [`Self::protocol`] on `endpoint` for as long as the door: it is what
    /// a sibling dials.
    pub(crate) fn serve(
        &self,
        runtime: &tokio::runtime::Handle,
        endpoint: iroh::Endpoint,
        channels: Arc<Mutex<BrokerChannels>>,
        fanout: Arc<FanoutState>,
        dials: Option<SiblingDials>,
    ) -> Door {
        // An identity is filed under its root's hex, in the profile and in the fleet of the
        // person's devices alike.
        let person = Person::of(hex::encode(self.root), self.root);
        // The devices the store holds once their fleet is in, before anything is spawned: what the
        // sibling dials start from, so the door's own admission cannot come after the first pass
        // read an empty store.
        let held = personal::admit_fleet(&self.sync, &person);
        let door = Door::serve(
            runtime,
            endpoint,
            self.sync.clone(),
            &person,
            &self.state,
            channels,
            fanout,
        );
        if let Some(dials) = dials {
            runtime.spawn(keep_siblings(self.sync.clone(), dials, held));
        }
        door
    }
}

/// Dial each of the person's devices the store holds as a sibling, from the ones the fleet named at
/// start and again whenever the store changes: what it holds is the fleet's, and the plane keeps
/// dialling the ones that are down.
async fn keep_siblings(sync: PersonalSync, dials: SiblingDials, mut held: Vec<[u8; 32]>) {
    let mut changes = sync.changes();
    loop {
        for device in std::mem::take(&mut held) {
            dials.dial(device);
        }
        if changes.changed().await.is_err() {
            return;
        }
        held = held_devices(&sync);
    }
}

/// The devices the person's store holds: this one, and every one of theirs whose certificate the
/// fleet admitted.
fn held_devices(sync: &PersonalSync) -> Vec<[u8; 32]> {
    sync.read(|store| {
        store
            .devices()
            .into_iter()
            .map(|device| device.device_public_key)
            .collect()
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "test fixtures: a failed step is the assertion"
    )]

    use std::time::{Duration, SystemTime};

    use ed25519_dalek::SigningKey;
    use tilo_peer_state::PersonalStore;
    use tilo_user_identity::{DeviceCapabilities, DeviceCert};

    use super::*;
    use crate::sync::Cmd;

    /// One of the person's devices' certificates, signed by their root.
    fn certificate(root: &SigningKey, n: u8, now: SystemTime) -> DeviceCert {
        let key = SigningKey::from_bytes(&[n; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: format!("device-{n}"),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: now
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |since| {
                    u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
                }),
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    /// The person's other devices are dialled as this session's siblings from the devices their
    /// store holds, and this device is not dialled at all: that session is where the two desks tell
    /// each other the workspaces they hold, so one adopts what the other keeps.
    #[test]
    fn the_persons_other_devices_are_dialled_as_siblings() {
        let root = SigningKey::from_bytes(&[7; 32]);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let here = certificate(&root, 1, now);
        let elsewhere = certificate(&root, 2, now);
        let root_key = root.verifying_key().to_bytes();
        let mut store =
            PersonalStore::open(root_key, here.clone(), now).expect("a store for this device");
        store
            .admit_device(&elsewhere, now)
            .expect("their other device is one of the person's");
        let sync = PersonalSync::over(PeerPersonal::new(store), SigningKey::from_bytes(&[1; 32]));

        let (outgoing, mut commands) = tokio::sync::mpsc::unbounded_channel();
        let dials = SiblingDials::new(
            here.device_public_key,
            crate::sync::HostOutbox::of(outgoing),
        );
        for device in held_devices(&sync) {
            dials.dial(device);
        }

        let mut dialled = Vec::new();
        while let Ok(command) = commands.try_recv() {
            match command {
                crate::sync::HostCmd::Plane(Cmd::ConnectDevice(addr)) => {
                    dialled.push(*addr.id.as_bytes());
                }
                other => panic!("a sibling dial sent something else: {other:?}"),
            }
        }
        assert_eq!(dialled, vec![elsewhere.device_public_key]);
    }
}
