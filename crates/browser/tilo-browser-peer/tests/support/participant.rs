//! A native participant of the room's channels, as a user's laptop is: it
//! announces itself, creates the note's channel and publishes its genesis,
//! then admits every device of its user that asks for access to a channel.
//! Browsers write no commits and never write in the clear, so every test
//! that edits a document runs one.

#![allow(
    dead_code,
    reason = "each test binary uses a different subset of the participant"
)]

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use channel_crypto::{ChannelContentKeys, ChannelPeer, VaultKey};
use collab_types::{ChannelId, Ed25519PublicKey, MlsGovernanceFrame, document_channel_id};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use tilo_relay::{CoeditError, Inbound, RelayClient, RelayCoeditSession};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use tilo_relay::daemon::Relay;

use crate::support::{Device, ROOM, enrol, route, scratch_dir, workspace};

/// Publish `message` through the host the way a shell does.
pub fn publish(session: &RelayCoeditSession, message: &NeighbourMsg) {
    publish_result(session, message).unwrap();
}

fn publish_result(session: &RelayCoeditSession, message: &NeighbourMsg) -> Result<(), CoeditError> {
    let (channel, retention) = neighbour_wire::retention(message, false).unwrap();
    let body = postcard::to_allocvec(message).unwrap();
    match retention {
        neighbour_wire::Retention::Checkpoint => session.publish_checkpoint(channel, body),
        neighbour_wire::Retention::Delta => session.publish_retained_delta(channel, body),
    }
}

struct Channels {
    dir: tempfile::TempDir,
    key: SigningKey,
    peers: HashMap<ChannelId, ChannelPeer>,
}

impl Channels {
    /// The channel's peer, its group created on first use: this laptop is
    /// the first of the room's devices to open every channel.
    fn peer(&mut self, channel: ChannelId) -> &mut ChannelPeer {
        self.peers.entry(channel).or_insert_with(|| {
            let dir = self.dir.path().join(hex::encode(channel.0));
            std::fs::create_dir_all(&dir).unwrap();
            ChannelPeer::open(&dir, channel, Box::new(self.key.clone()), &vault())
                .unwrap()
                .unwrap_or_else(|| {
                    ChannelPeer::create_now(
                        Some(&dir),
                        channel,
                        Box::new(self.key.clone()),
                        &vault(),
                    )
                    .unwrap()
                })
        })
    }

    /// What `channel`'s peer queued to publish, taken.
    fn take(&mut self, channel: ChannelId) -> Vec<MlsGovernanceFrame> {
        let peer = self.peer(channel);
        let frames = peer.outbox().to_vec();
        peer.published(&vault()).unwrap();
        frames
    }
}

/// Hold one fixture admission until the test releases it.
struct AdmissionGate {
    entered: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
}

type AdmissionGates = Arc<Mutex<HashMap<(ChannelId, Ed25519PublicKey), Arc<AdmissionGate>>>>;

pub struct HeldAdmission(Arc<AdmissionGate>);

impl HeldAdmission {
    /// Prove the real member requested this channel before proceeding.
    pub async fn requested(&self) {
        tilo_test_env::within(
            "the held member to request its channel",
            self.0.entered.notified(),
        )
        .await;
    }
}

impl Drop for HeldAdmission {
    fn drop(&mut self) {
        self.0.release.add_permits(1);
    }
}

pub struct Participant {
    pub device: Device,
    channels: Arc<Mutex<Channels>>,
    admissions: AdmissionGates,
    task: tokio::task::JoinHandle<()>,
}

impl Participant {
    /// Take part in the room through `host` as a new primary device of
    /// `root`.
    pub async fn start(host: &Relay, root: &SigningKey) -> Self {
        let device = enrol(root, "laptop", DeviceCapabilities::primary());
        let channels = Arc::new(Mutex::new(Channels {
            dir: scratch_dir(),
            key: device.key.clone(),
            peers: HashMap::new(),
        }));
        let session = join(host, root, &device).await;
        publish(
            &session,
            &NeighbourMsg::IdentityHello {
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                cert: Box::new(device.cert.clone()),
                revocations: Vec::new(),
            },
        );
        let note = route();
        let genesis = channels.lock().unwrap().take(document_channel_id(
            &note.app_id,
            &note.cluster_id,
            &note.record,
        ));
        for frame in genesis {
            publish(
                &session,
                &NeighbourMsg::MlsGovernance {
                    room: ROOM.into(),
                    frame: Box::new(frame),
                    delivery: 1,
                },
            );
        }
        let own = device.key.verifying_key().to_bytes();
        let admissions = AdmissionGates::default();
        let task = tokio::spawn(admit(session, channels.clone(), own, admissions.clone()));
        Self {
            device,
            channels,
            admissions,
            task,
        }
    }

    /// Take part on through a restarted `host`, which kept the room.
    pub async fn reconnect(&mut self, host: &Relay, root: &SigningKey) {
        self.task.abort();
        let _ = (&mut self.task).await;
        let session = join(host, root, &self.device).await;
        let own = self.device.key.verifying_key().to_bytes();
        self.task = tokio::spawn(admit(
            session,
            self.channels.clone(),
            own,
            self.admissions.clone(),
        ));
    }

    /// Pause one device's signed request, leaving other admissions unchanged.
    pub fn hold_admission(
        &self,
        route: &tilo_browser_peer::DocumentRoute,
        device: [u8; 32],
    ) -> HeldAdmission {
        let gate = Arc::new(AdmissionGate {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
        });
        let channel = document_channel_id(&route.app_id, &route.cluster_id, &route.record);
        assert!(
            self.admissions
                .lock()
                .unwrap()
                .insert(
                    (channel, Ed25519PublicKey::from_bytes(device)),
                    gate.clone()
                )
                .is_none()
        );
        HeldAdmission(gate)
    }

    pub fn content_keys(&self, route: &tilo_browser_peer::DocumentRoute) -> ChannelContentKeys {
        self.channels
            .lock()
            .unwrap()
            .peer(document_channel_id(
                &route.app_id,
                &route.cluster_id,
                &route.record,
            ))
            .content_keys()
            .unwrap()
    }
}

impl Drop for Participant {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn join(host: &Relay, root: &SigningKey, device: &Device) -> RelayCoeditSession {
    crate::support::place(host, &device.cert).await;
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

/// Admit every device that asks (the host admits only the user's own).
///
/// Members already in a channel stay there: the devices the membership
/// backs are this laptop, every device that asked and every member.
async fn admit(
    session: RelayCoeditSession,
    channels: Arc<Mutex<Channels>>,
    own: [u8; 32],
    admissions: AdmissionGates,
) {
    let mut asked: HashMap<ChannelId, BTreeSet<Ed25519PublicKey>> = HashMap::new();
    let mut delivery = 1_u64;
    while let Some(inbound) = session.recv().await {
        let Inbound::Delta { data, .. } = inbound else {
            continue;
        };
        let message = postcard::from_bytes::<NeighbourMsg>(&data);
        let Ok(NeighbourMsg::MlsRoutedAccessRequest { request, .. }) = message else {
            continue;
        };
        if request.verify().is_err() {
            continue;
        }
        let gate = admissions
            .lock()
            .unwrap()
            .remove(&(request.channel, request.device));
        if let Some(gate) = gate {
            gate.entered.notify_one();
            let permit = gate.release.acquire().await.unwrap();
            drop(permit);
        }
        // A host may disconnect while a request is in flight. Never publish
        // through that session while holding the persistent channel state:
        // a failed send must not poison the mutex used after reconnect.
        let outgoing = {
            let mut channels = channels.lock().unwrap();
            let channel = request.channel;
            let asking = asked.entry(channel).or_default();
            asking.insert(request.device);
            let peer = channels.peer(channel);
            peer.learn_request(*request, &vault()).unwrap();
            let allowed: BTreeSet<Ed25519PublicKey> = asking
                .iter()
                .copied()
                .chain(peer.members())
                .chain([Ed25519PublicKey::from_bytes(own)])
                .collect();
            peer.reconcile_now(&allowed, &vault()).unwrap();
            channels.take(channel)
        };
        for frame in outgoing {
            delivery = delivery.checked_add(1).unwrap();
            let message = NeighbourMsg::MlsGovernance {
                room: ROOM.into(),
                frame: Box::new(frame),
                delivery,
            };
            let sent = publish_result(&session, &message);
            // The successor session publishes again what this one could not.
            if matches!(&sent, Err(CoeditError::Disconnected)) {
                return;
            }
            sent.unwrap();
        }
    }
}

fn vault() -> VaultKey {
    VaultKey::new([7; 32])
}
