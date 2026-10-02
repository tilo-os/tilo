//! A native device that stewards the room's channels, as a user's laptop
//! does: it announces itself and the room's genesis, then admits every
//! device of its user that asks for access to a channel. Browsers never
//! write in the clear, so every test that edits a document runs one.

#![allow(
    dead_code,
    reason = "each test binary uses a different subset of the steward"
)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use channel_crypto::{ChannelContentKeys, ChannelSteward, VaultKey};
use collab_types::{ChannelId, Ed25519PublicKey, MlsAccessRequestV1, document_channel_id};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use tilo_relay::{Inbound, RelayClient, RelayCoeditSession};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use tilo_relay::daemon::Relay;

use crate::support::{Device, ROOM, enrol, route, workspace};

/// Publish `message` through the host the way a shell does.
pub fn publish(session: &RelayCoeditSession, message: &NeighbourMsg) {
    let (channel, retention) = neighbour_wire::retention(message, false).unwrap();
    let body = postcard::to_allocvec(message).unwrap();
    match retention {
        neighbour_wire::Retention::Checkpoint => session.publish_checkpoint(channel, body),
        neighbour_wire::Retention::Delta => session.publish_retained_delta(channel, body),
    }
    .unwrap();
}

struct Channels {
    dir: tempfile::TempDir,
    key: SigningKey,
    stewards: HashMap<ChannelId, ChannelSteward>,
}

impl Channels {
    fn steward(&mut self, channel: ChannelId) -> &mut ChannelSteward {
        self.stewards.entry(channel).or_insert_with(|| {
            let dir = self.dir.path().join(hex::encode(channel.0));
            std::fs::create_dir_all(&dir).unwrap();
            ChannelSteward::open_or_create(&dir, channel, self.key.clone(), &vault()).unwrap()
        })
    }
}

pub struct Steward {
    pub device: Device,
    channels: Arc<Mutex<Channels>>,
    task: tokio::task::JoinHandle<()>,
}

impl Steward {
    /// Steward the room through `host` as a new primary device of `root`.
    pub async fn start(host: &Relay, root: &SigningKey) -> Self {
        let device = enrol(root, "steward", DeviceCapabilities::primary());
        let channels = Arc::new(Mutex::new(Channels {
            dir: tempfile::tempdir().unwrap(),
            key: device.key.clone(),
            stewards: HashMap::new(),
        }));
        let session = join(host, root, &device).await;
        publish(
            &session,
            &NeighbourMsg::IdentityHello {
                cert: Box::new(device.cert.clone()),
                revocations: Vec::new(),
            },
        );
        let note = route();
        let genesis = channels
            .lock()
            .unwrap()
            .steward(document_channel_id(
                &note.app_id,
                &note.cluster_id,
                &note.record,
            ))
            .journal()
            .frames()
            .first()
            .cloned()
            .unwrap();
        publish(
            &session,
            &NeighbourMsg::MlsGovernance {
                room: ROOM.into(),
                frame: Box::new(genesis),
                delivery: 1,
            },
        );
        let own = device.key.verifying_key().to_bytes();
        let task = tokio::spawn(admit(session, channels.clone(), own));
        Self {
            device,
            channels,
            task,
        }
    }

    /// Steward on through a restarted `host`, which kept the room.
    pub async fn reconnect(&mut self, host: &Relay, root: &SigningKey) {
        self.task.abort();
        let session = join(host, root, &self.device).await;
        let own = self.device.key.verifying_key().to_bytes();
        self.task = tokio::spawn(admit(session, self.channels.clone(), own));
    }

    /// The current content keys of `route`'s channel.
    pub fn content_keys(&self, route: &tilo_browser_peer::DocumentRoute) -> ChannelContentKeys {
        self.channels
            .lock()
            .unwrap()
            .steward(document_channel_id(
                &route.app_id,
                &route.cluster_id,
                &route.record,
            ))
            .content_keys()
            .unwrap()
    }
}

impl Drop for Steward {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn join(host: &Relay, root: &SigningKey, device: &Device) -> RelayCoeditSession {
    let endpoint = iroh::endpoint::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .bind()
        .await
        .unwrap();
    let client = RelayClient::connect(
        endpoint,
        host.node_addr(),
        root.verifying_key().to_bytes(),
        device.cert.clone(),
        device.key.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    RelayCoeditSession::join(client, workspace(), device.key.verifying_key().to_bytes()).unwrap()
}

/// Admit every device that asks (the host admits only the user's own), and
/// apply their acknowledgements.
async fn admit(session: RelayCoeditSession, channels: Arc<Mutex<Channels>>, own: [u8; 32]) {
    let mut requests: HashMap<ChannelId, BTreeMap<Ed25519PublicKey, MlsAccessRequestV1>> =
        HashMap::new();
    let mut delivery = 1_u64;
    while let Some(inbound) = session.recv().await {
        let Inbound::Delta { data, .. } = inbound else {
            continue;
        };
        let mut channels = channels.lock().unwrap();
        match postcard::from_bytes::<NeighbourMsg>(&data) {
            Ok(NeighbourMsg::MlsAccessRequest { request, .. }) => {
                if request.verify().is_err() {
                    continue;
                }
                let pending = requests.entry(request.channel).or_default();
                pending.insert(request.device, *request.clone());
                let allowed: BTreeSet<Ed25519PublicKey> = pending
                    .keys()
                    .copied()
                    .chain([Ed25519PublicKey::from_bytes(own)])
                    .collect();
                let steward = channels.steward(request.channel);
                steward.reconcile(&allowed, pending, &vault()).unwrap();
                for outgoing in steward.outbox() {
                    for frame in outgoing.frames {
                        delivery = delivery.checked_add(1).unwrap();
                        publish(
                            &session,
                            &NeighbourMsg::MlsGovernance {
                                room: ROOM.into(),
                                frame: Box::new(frame),
                                delivery,
                            },
                        );
                    }
                }
            }
            Ok(NeighbourMsg::MlsGovernanceAck { ack, .. }) if ack.verify().is_ok() => {
                let _ = channels.steward(ack.channel).acknowledge(&ack);
            }
            _ => {}
        }
    }
}

fn vault() -> VaultKey {
    VaultKey::new([7; 32])
}
