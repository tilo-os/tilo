//! The person's own store, open to their other devices while this session
//! runs.
//!
//! The store holds what this session's apps record in the person's own scope
//! — `(app, personal)` records, routed on the cluster their identity root
//! derives — so a record written here reaches their other devices and one
//! written there reaches this session's apps. The broker keeps each write in
//! it as well as in the desktop (`HostServices::personal`), and this session
//! answers the store's link on the endpoint it already runs, beside the
//! workspaces' traffic: its protocols are fixed when it binds, so the link is
//! handed over with the other protocols the endpoint serves
//! ([`crate::sync::Client::start`]).
//!
//! Nothing here joins anything and no workspace is involved: the link proves
//! which of the person's devices is on the other end, and the store admits
//! only certificates their identity root signed ([`tilo_peer_state`]).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use tilo_action_broker::collab_broker::FanoutState;
use tilo_action_broker::collab_core::BrokerChannels;
use tilo_action_broker::personal::{self, Door, Person};
use tilo_network_actor::HostIdentity;
use tilo_peer_state::{PeerPersonal, PersonalSync};

use crate::options::Options;

/// This session's door to the person's own store: the store itself, opened on
/// the identity this device acts as, and where it is kept.
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
            sync: PersonalSync::over(
                PeerPersonal::new(store),
                identity.device_signing_key.clone(),
            ),
            root,
            state: state.to_owned(),
        })
    }

    /// What answers the store's link on this session's endpoint: the session
    /// serves it beside its own traffic, from the pass the endpoint binds.
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

    /// Take the store to the person's other devices from `endpoint`: dial the
    /// ones this device has no link to, hand what they write to this
    /// session's apps through `channels` and `fanout`, and keep the store
    /// beside this session's state as it changes.
    ///
    /// The session must answer [`Self::protocol`] on `endpoint` for as long
    /// as the door: it is what a sibling dials.
    pub(crate) fn serve(
        &self,
        runtime: &tokio::runtime::Handle,
        endpoint: iroh::Endpoint,
        channels: Arc<Mutex<BrokerChannels>>,
        fanout: Arc<FanoutState>,
    ) -> Door {
        Door::serve(
            runtime,
            endpoint,
            self.sync.clone(),
            // An identity is filed under its root's hex, in the profile and in
            // the fleet of the person's devices alike.
            &Person::of(hex::encode(self.root), self.root),
            &self.state,
            channels,
            fanout,
        )
    }
}
