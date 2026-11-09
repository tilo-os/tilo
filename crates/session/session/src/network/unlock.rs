//! Another of the person's devices asking for its disk-unlock half.
//! Every device of the person holds the sibling halves of the others, sealed to the person's
//! root-derived secret-sync key ([`tilo_device_fabric::unlock`]). Early boot of an enrolled device
//! reaches this one over [`tilo_unlock_protocol::UNLOCK_ALPN`]: it presents the boot key
//! certificate the person's root signed, answers this device's challenge, and waits. The person is
//! asked on the session's question sheet, with the approval code the boot screen shows; an approval
//! releases the half sealed to the boot's ephemeral key, and the next boot needs another approval.
//! The answer is a fact this device keeps ([`UnlockDecisionRecord`]) and their other devices carry:
//! a boot whose stream went away before the answer reached it hears the same answer again, with a
//! half sealed to its own new key where the person allowed it, instead of asking them twice.
//! `tilo-disk-encrypt enrol --sibling` reaches [`Unlock::enrol`] through the session's enrolment
//! socket ([`tilo_unlock_protocol::ENROL_SOCKET`]): the boot key is certified by the person's root,
//! the sibling half is sealed to the secret-sync key, and the enrolment is published in this
//! device's fabric record for the person's other devices to carry.

use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::endpoint::{Connection, RecvStream, SendStream};
use serde::{Deserialize, Serialize};
use tilo_device_fabric::DeviceId;
use tilo_device_fabric::link::SharedLink;
use tilo_device_fabric::unlock::{
    Decided, DecisionOutcome, MAX_UNLOCK_DECISIONS, MergedUnlock, UnlockDecision,
    UnlockDecisionRecord, UnlockEnrolment, UnlockFabricState, UnlockSibling, UnlockView,
    decision_for, merged_unlock_state,
};
use tilo_identity_host::device_fleet::DeviceFleetStore;
use tilo_identity_host::registry::IdentityRegistryStore;
use tilo_service::{Asked, Question, Questions};
use tilo_unlock_protocol::{
    APPROVAL_TIMEOUT_NS, BootKeyCert, CHALLENGE_LIFETIME_NS, ClientFrame, ENROL_SOCKET, EnrolReply,
    EnrolRequest, EnrolResponse, PROTOCOL_VERSION, RefusalReason, SiblingContact, SiblingFrame,
    SlotId, UnlockHello, UnlockRequest, decode_frame, encode_frame, frame_len, seal_sibling_half,
};
use tilo_user_identity::{CertifiedFleet, DeviceStanding};

/// The kind the session's sheet reads a disk-unlock request as.
pub(crate) const KIND: &str = "network.device-unlock";

/// How long a boot may take to answer its challenge.
const ANSWER_TIMEOUT: Duration = Duration::from_nanos(CHALLENGE_LIFETIME_NS);
/// How long an approved boot may take to read the half once decided.
const WRITE_GRACE: Duration = Duration::from_secs(2);

/// A request waiting for the owner, as the sheet shows it.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct UnlockQuestion {
    /// The code the boot screen shows; the person compares them.
    pub approval_code: String,
    /// The device that asks, as its record names it.
    pub device: String,
    /// The slot it wants unlocked.
    pub slot: String,
}

/// The person's identity this session answers as, and what it holds.
struct Custody {
    identity_id: String,
    fleet: CertifiedFleet,
    /// Whether this device holds the person's root, so it can release.
    custodian: bool,
}

/// The sibling side of disk unlock, served by one session.
pub(crate) struct Unlock {
    link: SharedLink,
    questions: Option<Questions>,
    /// This device's fleet key: the sibling boots answer.
    local: [u8; 32],
    /// The challenges this device issued, the boots it served and the requests waiting for it. One
    /// session answers as one device.
    sibling: Mutex<UnlockSibling>,
    /// Serializes what this device publishes about disk unlock: two boots answered at once must not
    /// overwrite each other's record.
    published: Mutex<()>,
}

impl Unlock {
    /// The sibling side for the device whose fleet key is `local` , asking `questions` where it
    /// needs the person's consent.
    pub(crate) fn new(link: SharedLink, local: [u8; 32], questions: Option<Questions>) -> Self {
        Self {
            link,
            questions,
            local,
            sibling: Mutex::new(UnlockSibling::new(local)),
            published: Mutex::new(()),
        }
    }

    /// Answer one early-boot connection on [`tilo_unlock_protocol::UNLOCK_ALPN`].
    pub(crate) async fn serve(self: Arc<Self>, connection: Connection) {
        let Ok((mut send, mut recv)) = connection.accept_bi().await else {
            return;
        };
        let Some(ClientFrame::Hello(hello)) = read_client(&mut recv).await else {
            connection.close(0u32.into(), b"not a boot");
            return;
        };
        let Some(custody) = read_custody() else {
            tell(send, SiblingFrame::Refused(RefusalReason::NotACustodian)).await;
            return;
        };
        let published = self.published(&custody);
        let challenge = self.challenge(&hello, &custody, &published);
        let SiblingFrame::Challenge(issued) = &challenge else {
            tell(send, challenge.clone()).await;
            return;
        };
        let nonce = issued.nonce;
        if !write_sibling(&mut send, &challenge).await {
            return;
        }
        let Some(ClientFrame::Request(request)) = read_client(&mut recv).await else {
            return;
        };
        let now = tilo_clock::unix_now_ns();
        // The owner's answer to this boot, when their devices kept one, is the whole exchange: an
        // approval needs the half, a denial needs nothing loaded.
        let allowed_before = boot_cert_digest(&hello.boot_cert)
            .and_then(|digest| {
                decision_for(
                    &published.decisions,
                    &digest,
                    request.boot_id,
                    request.slot,
                    now,
                )
            })
            .is_some_and(|record| record.outcome == DecisionOutcome::Approve);
        let frame = {
            let secret_sync = if allowed_before {
                self.secret_sync(&custody).await
            } else {
                None
            };
            let view = view_of(&custody, &published, secret_sync.as_ref());
            self.lock().request(&request, &hello, &nonce, &view, now)
        };
        let SiblingFrame::Pending { approval_code } = &frame else {
            if write_sibling(&mut send, &frame).await {
                if let Err(error) = send.finish() {
                    tracing::warn!(%error, "a boot's pending answer did not reach it");
                }
                let _ = tokio::time::timeout(WRITE_GRACE, send.stopped()).await;
            }
            return;
        };
        let pending = approval_code.clone();
        let digest = request
            .digest(&hello.boot_cert, &self.local)
            .unwrap_or_default();
        if !write_sibling(&mut send, &frame).await {
            // The boot is gone before it could read even that: nobody will wait for a decision on
            // its behalf.
            self.lock().withdraw(&digest);
            return;
        }
        // The person is asked here, and the sheet shows nothing more of it once they answered or
        // the boot went away.
        let asked = self.ask(&hello.boot_cert, &pending, &request);
        let Some(answer) = wait_for_person(asked, &send).await else {
            self.lock().withdraw(&digest);
            return;
        };
        let approved = answer.is_some();
        // Released only with the person's consent, and only for a boot still waiting: a decision on
        // a request that went away decides nothing.
        if let Some(decided) = self.decide(&custody, &published, &digest, approved).await {
            if let Some(record) = decided.record {
                self.keep(record);
            }
            if !write_sibling(&mut send, &decided.frame).await {
                tracing::warn!("a boot was not told the decision on its unlock request");
            }
        }
        if let Err(error) = send.finish() {
            tracing::warn!(%error, "an unlock decision did not reach the boot that asked");
        }
        let _ = tokio::time::timeout(WRITE_GRACE, send.stopped()).await;
    }

    /// Challenge the boot, or refuse its hello.
    fn challenge(
        &self,
        hello: &UnlockHello,
        custody: &Custody,
        published: &MergedUnlock,
    ) -> SiblingFrame {
        let view = view_of(custody, published, None);
        self.lock().hello(
            hello,
            &view,
            tilo_device_fabric::random::nonzero::<32>(),
            tilo_clock::unix_now_ns(),
        )
    }

    /// The challenges this device issued and the requests waiting for it.
    fn lock(&self) -> std::sync::MutexGuard<'_, UnlockSibling> {
        self.sibling
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The person's secret-sync key, held only where this device may release a half: loaded then,
    /// never before.
    async fn secret_sync(&self, custody: &Custody) -> Option<tilo_seal::SecretKey> {
        if !custody.custodian {
            return None;
        }
        tokio::task::spawn_blocking(secret_sync_key)
            .await
            .ok()
            .flatten()
    }

    /// Record the person's decision on the request `digest` , for the boot still waiting.
    async fn decide(
        &self,
        custody: &Custody,
        published: &MergedUnlock,
        digest: &[u8; 32],
        approved: bool,
    ) -> Option<Decided> {
        let secret_sync = if approved {
            self.secret_sync(custody).await
        } else {
            None
        };
        let decision = if approved {
            UnlockDecision::Approve {
                approver: self.local,
            }
        } else {
            UnlockDecision::Deny {
                approver: self.local,
            }
        };
        let view = view_of(custody, published, secret_sync.as_ref());
        self.lock()
            .decide(digest, decision, &view, tilo_clock::unix_now_ns())
    }

    /// Put the boot's request to the person, with the code its screen shows,
    /// and let dropping the question go with it.
    fn ask(
        &self,
        cert: &BootKeyCert,
        approval_code: &str,
        request: &UnlockRequest,
    ) -> Option<Asked> {
        let questions = self.questions.as_ref()?;
        let handle = format!("device-unlock:{}", hex::encode(request.boot_id.0));
        let device = self.label_of(&cert.device_public_key);
        Some(
            questions.ask(Question {
                handle,
                kind: KIND.to_owned(),
                app_id: None,
                label: device.clone(),
                details: serde_json::to_value(UnlockQuestion {
                    approval_code: approval_code.to_owned(),
                    device,
                    slot: slot_name(&cert.slot),
                })
                .unwrap_or_default(),
            }),
        )
    }

    /// The name this device's record gives `device` .
    fn label_of(&self, device: &[u8; 32]) -> String {
        let key = hex::encode(device);
        self.link
            .device_fabric()
            .snapshots()
            .values()
            .find(|snapshot| snapshot.owner.as_str() == key)
            .map_or_else(|| key, |snapshot| snapshot.presence.label.clone())
    }

    /// The enrolments, auto-approvals and answers the person's fabric holds now, under the fleet
    /// this device trusts.
    fn published(&self, custody: &Custody) -> MergedUnlock {
        let fabric = self.link.device_fabric();
        merged_unlock_state(
            fabric.snapshots().values().map(|snapshot| &snapshot.unlock),
            &custody.fleet,
            tilo_clock::unix_now_ns(),
        )
    }

    /// Serve `tilo-disk-encrypt enrol --sibling` : certify the boot key, seal its sibling half, and
    /// publish the enrolment to the person's other devices.
    pub(crate) fn enrol(&self, request: &EnrolRequest) -> EnrolReply {
        match self.try_enrol(request) {
            Ok(response) => EnrolReply::Enrolled(Box::new(response)),
            Err(reason) => EnrolReply::Refused(reason),
        }
    }

    fn try_enrol(&self, request: &EnrolRequest) -> Result<EnrolResponse, String> {
        request
            .verify()
            .map_err(|error| format!("enrolment request: {error}"))?;
        let custody = read_custody()
            .ok_or_else(|| "this session holds no identity for the person's devices".to_string())?;
        if !custody.custodian {
            return Err("this device does not hold the person's root".into());
        }
        let store = DeviceFleetStore::from_env().map_err(|error| error.to_string())?;
        let mut cert = BootKeyCert {
            version: PROTOCOL_VERSION,
            user_public_key: *custody.fleet.user_public_key(),
            device_public_key: self.local,
            boot_public_key: request.boot_public_key,
            slot: request.slot,
            attestation_key: request.attestation_key.clone(),
            issued_at_ns: tilo_clock::unix_now_ns(),
            signature: [0; 64],
        };
        store
            .certify_boot_key(&custody.identity_id, &mut cert)
            .map_err(|error| error.to_string())?;
        let secret_sync = store
            .secret_sync_key(&custody.identity_id)
            .and_then(|key| key.public_key().map_err(std::io::Error::other))
            .map_err(|error| error.to_string())?;
        let sealed_half = seal_sibling_half(&secret_sync, &cert, &request.sibling_half)
            .map_err(|error| error.to_string())?;
        let now = tilo_clock::unix_now_ns();
        let siblings = self
            .link
            .device_fabric()
            .snapshots()
            .iter()
            .filter(|(owner, _)| owner.as_str() != hex::encode(self.local))
            .filter_map(|(owner, snapshot)| {
                let key = crate::network::key_of(owner)?;
                (custody.fleet.standing(&key, now) == DeviceStanding::Certified).then(|| {
                    SiblingContact {
                        device_public_key: key,
                        label: snapshot.presence.label.clone(),
                        endpoint: String::new(),
                    }
                })
            })
            .collect();
        let device = DeviceId::new(hex::encode(self.local)).map_err(|error| error.to_string())?;
        let enrolment = UnlockEnrolment {
            cert: cert.clone(),
            sealed_half,
            stamp: tilo_network_actor::fabric::FabricStamp { counter: 0, device },
        };
        self.publish(enrolment);
        tracing::info!(
            device = %hex::encode(self.local),
            slot = %slot_name(&cert.slot),
            "device unlock: enrolled",
        );
        Ok(EnrolResponse { cert, siblings })
    }

    /// Add `enrolment` to this device's own fabric record, keeping what else it published there.
    fn publish(&self, enrolment: UnlockEnrolment) {
        self.update_state(|state| {
            state
                .enrolments
                .retain(|current| current.cert.slot != enrolment.cert.slot);
            state.enrolments.push(enrolment);
        });
    }

    /// Keep the owner's answer in this device's own fabric record, so their other devices answer
    /// the same boot the same way. Answers that no longer hold give way to the newest ones.
    fn keep(&self, record: UnlockDecisionRecord) {
        let now = tilo_clock::unix_now_ns();
        self.update_state(|state| {
            state
                .decisions
                .retain(|current| current.valid_until_ns > now && current.key() != record.key());
            state.decisions.push(record);
            state.decisions.sort_by_key(|kept| kept.decided_at_ns);
            let over = state.decisions.len().saturating_sub(MAX_UNLOCK_DECISIONS);
            state.decisions.drain(..over);
        });
    }

    /// Change what this device publishes about disk unlock: its own record in the person's fabric
    /// is the only one it may write.
    fn update_state(&self, change: impl FnOnce(&mut UnlockFabricState)) {
        let _serialized = self
            .published
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let fabric = self.link.device_fabric();
        let key = hex::encode(self.local);
        let mut state = fabric
            .snapshots()
            .iter()
            .find(|(owner, _)| owner.as_str() == key)
            .map(|(_, snapshot)| snapshot.unlock.clone())
            .unwrap_or_default();
        change(&mut state);
        self.link.share_unlock(state);
    }

    /// Serve the session's enrolment socket, one request per connection.
    pub(crate) fn spawn_enrol_socket(self: &Arc<Self>) {
        let runtime = tilo_paths::xdg::runtime_dir_or_default();
        let path = runtime.join(ENROL_SOCKET);
        let service = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("device-unlock-enrol".into())
            .spawn(move || {
                let listener = match crate::agent_door::bind_private(&path) {
                    Ok(listener) => listener,
                    Err(error) => {
                        // ignored-failure: a session without the enrolment socket enrolls nothing
                        tracing::debug!(%error, "disk-unlock enrolment socket unavailable");
                        return;
                    }
                };
                for stream in listener.incoming().flatten() {
                    if let Err(error) = stream.set_read_timeout(Some(Duration::from_secs(10))) {
                        // Without it a boot that says nothing holds this connection open for as
                        // long as it likes.
                        tracing::warn!(%error, "an enrolment connection has no read timeout");
                    }
                    let _ = service.answer_enrolment(stream);
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "could not start the disk-unlock enrolment socket");
        }
    }

    fn answer_enrolment(&self, mut stream: std::os::unix::net::UnixStream) -> Option<()> {
        let mut prefix = [0_u8; 4];
        stream.read_exact(&mut prefix).ok()?;
        let len = frame_len(prefix).ok()?;
        let mut body = zeroize::Zeroizing::new(vec![0_u8; len]);
        stream.read_exact(&mut body).ok()?;
        let reply = decode_frame::<EnrolRequest>(&body).map_or_else(
            |_| EnrolReply::Refused("malformed enrolment request".into()),
            |request| self.enrol(&request),
        );
        let frame = encode_frame(&reply).ok()?;
        stream.write_all(&frame).ok()
    }
}

/// What the sibling state machine reads: the fleet this device trusts and the person's fabric as it
/// holds it.
fn view_of<'a>(
    custody: &'a Custody,
    published: &'a MergedUnlock,
    secret_sync: Option<&'a tilo_seal::SecretKey>,
) -> UnlockView<'a> {
    UnlockView {
        fleet: &custody.fleet,
        enrolments: &published.enrolments,
        auto_approvals: &published.auto_approvals,
        decisions: &published.decisions,
        custodian: custody.custodian,
        secret_sync,
    }
}

/// Tell the boot why it is refused, and finish the stream.
async fn tell(mut send: SendStream, frame: SiblingFrame) {
    if !write_sibling(&mut send, &frame).await {
        tracing::warn!("a boot was not told why it is refused");
    }
    if let Err(error) = send.finish() {
        tracing::warn!(%error, "a refusal did not reach the boot");
    }
}

/// The person's answer to the question `asked` : their decision, or `None` when the boot stopped
/// waiting before one came.
async fn wait_for_person(
    asked: Option<Asked>,
    send: &SendStream,
) -> Option<Option<tilo_service::Answer>> {
    let Some(asked) = asked else {
        // No sheet to ask on: the boot is refused as if nobody answered.
        return Some(None);
    };
    let wait = tokio::time::timeout(Duration::from_nanos(APPROVAL_TIMEOUT_NS), async {
        tokio::select! {
            answer = asked => Some(answer),
            // The boot stopped waiting: its question is taken back rather than answered for nobody.
            _ = send.stopped() => None,
        }
    })
    .await;
    match wait {
        Ok(answer) => answer,
        // Nobody answered in time, which refuses this boot.
        // ignored-failure: no answer within the window is a refusal
        Err(_) => Some(None),
    }
}

/// The enrolment a boot certificate is known by.
fn boot_cert_digest(cert: &BootKeyCert) -> Option<[u8; 32]> {
    cert.signing_digest().ok()
}

/// The slot as the boot screen names it, and as the person's own record of their answers keeps it.
pub(crate) fn slot_name(slot: &SlotId) -> String {
    format!("{}:{}", hex::encode(slot.volume_uuid), slot.keyslot)
}

/// This device's identity, the fleet it is certified by, and whether it holds the person's root.
fn read_custody() -> Option<Custody> {
    let registry = IdentityRegistryStore::from_env()
        .ok()?
        .load_reconciled_cached()
        .ok()?;
    let profile = registry.certified_identity()?;
    let store = DeviceFleetStore::from_env().ok()?;
    let fleet = store
        .certified_fleet(&profile.id, profile.user_public_key)
        .ok()?;
    let custodian = store.secret_sync_key(&profile.id).is_ok();
    Some(Custody {
        identity_id: profile.id,
        fleet,
        custodian,
    })
}

/// The secret-sync key, loaded only where a half is released.
fn secret_sync_key() -> Option<tilo_seal::SecretKey> {
    let registry = IdentityRegistryStore::from_env()
        .ok()?
        .load_reconciled_cached()
        .ok()?;
    let profile = registry.certified_identity()?;
    DeviceFleetStore::from_env()
        .ok()?
        .secret_sync_key(&profile.id)
        .ok()
}

async fn read_client(recv: &mut RecvStream) -> Option<ClientFrame> {
    let mut prefix = [0_u8; 4];
    tokio::time::timeout(ANSWER_TIMEOUT, recv.read_exact(&mut prefix))
        .await
        .ok()?
        .ok()?;
    let mut body = vec![0_u8; frame_len(prefix).ok()?];
    tokio::time::timeout(ANSWER_TIMEOUT, recv.read_exact(&mut body))
        .await
        .ok()?
        .ok()?;
    // A frame that does not decode is not an unlock request: the caller closes
    // the connection as not a boot.
    // ignored-failure: a frame this protocol does not know is not a boot
    decode_frame(&body).ok()
}

async fn write_sibling(send: &mut SendStream, frame: &SiblingFrame) -> bool {
    match encode_frame(frame) {
        Ok(bytes) => send.write_all(&bytes).await.is_ok(),
        Err(error) => {
            tracing::warn!(%error, "an unlock answer could not be encoded");
            false
        }
    }
}
