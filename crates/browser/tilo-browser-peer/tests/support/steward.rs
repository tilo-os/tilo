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
    // When each frame was last published: a new frame goes out at once, one
    // a member still has not acknowledged again only after RESEND.
    let mut sent_at: HashMap<[u8; 32], std::time::Instant> = HashMap::new();
    const RESEND: std::time::Duration = std::time::Duration::from_secs(2);
    while let Some(first) = session.recv().await {
        // Everything already waiting is taken together: a channel many
        // members ask for at once (a record just made) is reconciled once
        // for all of them rather than once per request.
        let mut batch = vec![first];
        while let Ok(Some(next)) =
            tokio::time::timeout(std::time::Duration::ZERO, session.recv()).await
        {
            batch.push(next);
        }
        let mut asked: Vec<ChannelId> = Vec::new();
        for inbound in batch {
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
            let mut channels = channels.lock().unwrap();
            match message {
                Ok(NeighbourMsg::MlsAccessRequest { request, .. }) => {
                    if request.verify().is_err() {
                        continue;
                    }
                    requests
                        .entry(request.channel)
                        .or_default()
                        .insert(request.device, *request.clone());
                    if !asked.contains(&request.channel) {
                        asked.push(request.channel);
                    }
                }
                Ok(NeighbourMsg::MlsGovernanceAck { ack, .. }) if ack.verify().is_ok() => {
                    let _ = channels.steward(ack.channel).acknowledge(&ack);
                }
                _ => {}
            }
        }
        let mut channels = channels.lock().unwrap();
        // `TILO_STEWARD_LOG`: each reconcile's size and duration on stderr.
        let log = std::env::var_os("TILO_STEWARD_LOG").is_some();
        for channel in asked {
            let started = std::time::Instant::now();
            let pending = requests.entry(channel).or_default();
            let already = channels.steward(channel).members();
            let allowed: BTreeSet<Ed25519PublicKey> = pending
                .keys()
                .copied()
                .chain(already)
                .chain([Ed25519PublicKey::from_bytes(own)])
                .collect();
            let steward = channels.steward(channel);
            let outcome = steward.reconcile(&allowed, pending, &vault()).unwrap();
            if log {
                eprintln!(
                    "steward: {} channel {} pending {} admitted {} removed {} in {:?}",
                    tilo_clock::unix_now_ms(),
                    hex::encode(channel.0).chars().take(8).collect::<String>(),
                    pending.len(),
                    outcome.admitted.len(),
                    outcome.removed.len(),
                    started.elapsed()
                );
            }
            // Publishing reaches the whole room, so a frame several members
            // still lack is sent once, as the relay's own stewardship does
            // (`tilo_relay::rooms::stewardship`): once per lagging member,
            // a burst of admissions grows with the square of the room.
            let mut sent = std::collections::BTreeSet::new();
            for outgoing in steward.outbox() {
                for frame in outgoing.frames {
                    let digest = frame.digest().unwrap();
                    if !sent.insert(digest)
                        || sent_at.get(&digest).is_some_and(|at| at.elapsed() < RESEND)
                    {
                        continue;
                    }
                    sent_at.insert(digest, std::time::Instant::now());
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
    }
}

fn vault() -> VaultKey {
    VaultKey::new([7; 32])
}
