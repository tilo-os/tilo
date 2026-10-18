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
use tilo_relay::{CoeditError, Inbound, RelayClient, RelayCoeditSession};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use tilo_relay::daemon::Relay;

use crate::support::{Device, ROOM, enrol, route, workspace};

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
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.0.entered.notified(),
        )
        .await
        .expect("the held member never requested its channel");
    }
}

impl Drop for HeldAdmission {
    fn drop(&mut self) {
        self.0.release.add_permits(1);
    }
}

pub struct Steward {
    pub device: Device,
    channels: Arc<Mutex<Channels>>,
    admissions: AdmissionGates,
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
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
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
        let admissions = AdmissionGates::default();
        let task = tokio::spawn(admit(session, channels.clone(), own, admissions.clone()));
        Self {
            device,
            channels,
            admissions,
            task,
        }
    }

    /// Steward on through a restarted `host`, which kept the room.
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
///
/// Members already in a channel stay there. This loop's request map starts
/// empty on every reconnect; reconciling against only the requests seen
/// since then would remove everyone who has not asked again yet, and a
/// peer that resumes cannot open edits sealed while it was out.
async fn admit(
    session: RelayCoeditSession,
    channels: Arc<Mutex<Channels>>,
    own: [u8; 32],
    admissions: AdmissionGates,
) {
    let mut requests: HashMap<ChannelId, BTreeMap<Ed25519PublicKey, MlsAccessRequestV1>> =
        HashMap::new();
    let mut delivery = 1_u64;
    while let Some(inbound) = session.recv().await {
        let Inbound::Delta { data, .. } = inbound else {
            continue;
        };
        let message = postcard::from_bytes::<NeighbourMsg>(&data);
        if let Ok(NeighbourMsg::MlsAccessRequest { request, .. }) = &message
            && request.verify().is_ok()
        {
            let gate = admissions
                .lock()
                .unwrap()
                .remove(&(request.channel, request.device));
            if let Some(gate) = gate {
                gate.entered.notify_one();
                let permit = gate.release.acquire().await.unwrap();
                drop(permit);
            }
        }
        // A host may disconnect while a request is in flight. Never publish
        // through that session while holding the steward's persistent channel
        // state: a failed send must not poison the mutex used after reconnect.
        let outgoing = {
            let mut channels = channels.lock().unwrap();
            match message {
                Ok(NeighbourMsg::MlsAccessRequest { request, .. }) => {
                    if request.verify().is_err() {
                        continue;
                    }
                    let pending = requests.entry(request.channel).or_default();
                    pending.insert(request.device, *request.clone());
                    let already = channels.steward(request.channel).members();
                    let allowed: BTreeSet<Ed25519PublicKey> = pending
                        .keys()
                        .copied()
                        .chain(already)
                        .chain([Ed25519PublicKey::from_bytes(own)])
                        .collect();
                    let steward = channels.steward(request.channel);
                    steward.reconcile(&allowed, pending, &vault()).unwrap();
                    steward
                        .outbox()
                        .into_iter()
                        .flat_map(|outgoing| outgoing.frames)
                        .flat_map(|frame| frame.into_deliveries().unwrap())
                        .collect::<Vec<_>>()
                }
                Ok(NeighbourMsg::MlsGovernanceAck { ack, .. }) if ack.verify().is_ok() => {
                    let _ = channels.steward(ack.channel).acknowledge(&ack);
                    Vec::new()
                }
                _ => Vec::new(),
            }
        };
        for frame in outgoing {
            delivery = delivery.checked_add(1).unwrap();
            let message = NeighbourMsg::MlsGovernance {
                room: ROOM.into(),
                frame: Box::new(frame),
                delivery,
            };
            let sent = publish_result(&session, &message);
            // The successor session replays the outbox when its member asks
            // for keys. The old host can no longer deliver them.
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
