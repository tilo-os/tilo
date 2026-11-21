//! The person's own store, open to their other devices while this session runs.
//! The store holds what this session's apps record in the person's own scope — `(app, personal)`
//! records, routed on the cluster their identity root derives — so a record written here reaches
//! their other devices and one written there reaches this session's apps. The broker keeps each
//! write in it as well as in the desktop (`ProcessServices::personal`), and this session answers the
//! store's link on the endpoint it already runs, beside the workspaces' traffic: its protocols are
//! fixed when it binds, so the link is handed over with the other protocols the endpoint serves
//! ([`crate::sync::Client::start`]).
//! Nothing here joins anything and no workspace is involved: the link proves which of the person's
//! devices is on the other end, and the store admits only certificates their identity root signed
//! ([`tilo_peer_state`]).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use tilo_broker_core::collab_core::BrokerChannels;
use tilo_network_actor::DeviceIdentity;
use tilo_peer_state::{PeerPersonal, PersonalSync};
use tilo_workspace_service::apps::collab_broker::FanoutState;
use tilo_workspace_service::apps::personal::{self, Links, Person};

use crate::options::Options;
use crate::sync::SiblingDials;

/// This session's links to the person's own store: the store itself, opened on the identity this
/// device acts as, and where it is kept.
pub(crate) struct Personal {
    sync: PersonalSync,
    root: [u8; 32],
    state: PathBuf,
    pub(crate) enrollment_links: crate::enrollment::LocalLinks,
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
    pub(crate) fn open(options: &Options, identity: &DeviceIdentity) -> Result<Self, String> {
        let Some(state) = options.state.as_deref() else {
            return Err("this session keeps no state to keep the person's records in".into());
        };
        let root = identity.device_cert.user_public_key;
        let store = personal::open(state, root, identity.device_cert.clone(), SystemTime::now())?;
        Ok(Self {
            sync: PersonalSync::over(PeerPersonal::new(store), Arc::clone(&identity.signer)),
            root,
            state: state.to_owned(),
            enrollment_links: crate::enrollment::LocalLinks::default(),
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
    /// keep the store beside this session's state as it changes, and `wake` the session's loop
    /// when it does: what a sibling wrote of the session's own resources (the person's list of
    /// workspaces, the shell's own resource) is taken in by the next pass.
    /// `dials` opens the other connection to the same devices: the session between two of the person's
    /// sessions is where they tell each other what they hold — their workspaces and their hosts — so
    /// one adopts what the other keeps. Without it a device keeps only what it was there to make.
    /// The session must answer [`Self::protocol`] on `endpoint` for as long as the links: it is what
    /// a sibling dials.
    pub(crate) fn serve(
        &self,
        runtime: &tokio::runtime::Handle,
        endpoint: iroh::Endpoint,
        channels: Arc<Mutex<BrokerChannels>>,
        fanout: Arc<FanoutState>,
        dials: Option<SiblingDials>,
        wake: tilo_service::Wake,
    ) -> Links {
        // An identity is filed under its root's hex, in the profile and in the fleet of the
        // person's devices alike.
        let person = Person::of(hex::encode(self.root), self.root);
        // The devices the store holds once their fleet is in, before anything is spawned: what the
        // sibling dials start from, so its own admission cannot come after the first pass
        // read an empty store.
        let held = personal::admit_fleet(&self.sync, &person);
        let links = Links::serve(
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
        runtime.spawn(wake_on_changes(self.sync.clone(), wake));
        links
    }
}

/// Dial each of the person's devices the store holds as a sibling, from the ones the fleet named at
/// start and again whenever the store changes: what it holds is the fleet's, and the peer network keeps
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

/// Wake the session's loop whenever the person's store changes, until it is gone.
async fn wake_on_changes(sync: PersonalSync, wake: tilo_service::Wake) {
    let mut changes = sync.changes();
    while changes.changed().await.is_ok() {
        wake();
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

/// Two of the person's devices in one process: their own stores, and the link between them as the
/// session keeps it, carried by hand.
#[cfg(test)]
pub(crate) mod linked {
    use std::sync::Arc;

    use collab_types::signer::DeviceSigner;
    use tilo_peer_state::{PeerPersonal, PersonalStore, PersonalSync};
    use tilo_user_identity::DeviceCert;

    /// The person's own store on the device `cert` names, which `signer` signs for, as the session
    /// opens it.
    pub(crate) fn store_of(cert: &DeviceCert, signer: Arc<dyn DeviceSigner>) -> PersonalSync {
        let store = PersonalStore::open(cert.user_public_key, cert.clone(), tilo_clock::wall())
            .expect("a store for one of the person's devices");
        PersonalSync::over(PeerPersonal::new(store), signer)
    }

    /// `to` takes what `from` hands it on a link between the two: who `from` is, then every
    /// resource of the person's it holds, each admitted as the link admits it.
    pub(crate) fn hand(from: &PersonalSync, to: &PersonalSync) {
        let device = |store: &PersonalSync| store.read(|store| store.device().device_public_key);
        let (from_device, to_device) = (device(from), device(to));
        from.take(to_device, to.read(PersonalStore::hello))
            .expect("the person's other device says who it is");
        for frame in from
            .catch_up(to_device)
            .expect("what the store hands the person's other device")
        {
            to.take(from_device, frame)
                .expect("the person's own device's store is taken");
        }
    }
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
    /// store holds, at the server the person's settings name, and this device is not dialled at all:
    /// that session is where the two sessions tell each other the workspaces they hold, so one adopts
    /// what the other keeps.
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
            Some(relay_url()),
        );
        for device in held_devices(&sync) {
            dials.dial(device);
        }

        let mut dialled = Vec::new();
        while let Ok(command) = commands.try_recv() {
            match command {
                crate::sync::HostCmd::Network(Cmd::ConnectDevice(addr)) => {
                    dialled.push(*addr.id.as_bytes());
                    assert_eq!(addr.relay_urls().collect::<Vec<_>>(), vec![&relay_url()]);
                }
                other => panic!("a sibling dial sent something else: {other:?}"),
            }
        }
        assert_eq!(dialled, vec![elsewhere.device_public_key]);
    }

    /// The person's own relay, as their setting names it.
    fn relay_url() -> iroh::RelayUrl {
        "https://relay.example".parse().expect("a relay URL")
    }
}
