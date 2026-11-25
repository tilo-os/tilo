//! Another of the person's devices asking for its disk-unlock half.
//! Every device of the person holds the sibling halves of the others, sealed to the person's
//! root-derived secret-sync key ([`crate::platform::fabric::unlock`]). Early boot of an enrolled device
//! reaches this one over [`tilo_unlock_protocol::UNLOCK_ALPN`]: it presents the boot key
//! certificate the person's root signed, answers this device's challenge, and waits. The person is
//! asked on the session's question sheet, with the approval code the boot screen shows; an approval
//! releases the half sealed to the boot's ephemeral key, and the next boot needs another approval.
//! The answer is an answer of the record of the person's devices
//! ([`tilo_workspace_service::workspaces::person_workspace::devices::UnlockAnswer`]), written there
//! through this computer's daemon by the device the person answered on, and the workspace carries
//! it to their other devices: a boot whose stream went away before the answer reached it hears the
//! same answer again, with a half sealed to its own new key where the person allowed it, instead
//! of asking them twice, and the record's history is their list of what they answered.
//! A device's enrolment is its daemon's to publish, in its own entry of that record ([`UNLOCK`]):
//! the boot key certified by the person's root, and the sibling half sealed to their secret-sync
//! key.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::platform::fabric::DeviceId;
use crate::platform::fabric::SharedLink;
use crate::platform::fabric::{
    Decided, DecisionOutcome, MAX_UNLOCK_DECISIONS, MergedUnlock, UnlockDecision,
    UnlockDecisionEntry, UnlockEnrolment, UnlockFabricState, UnlockSibling, UnlockView,
    decision_for, merged_unlock_state,
};
use iroh::endpoint::{Connection, RecvStream, SendStream};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tilo_service::{AskSender, Asked, Question};
use tilo_unlock_protocol::{
    APPROVAL_TIMEOUT_NS, CHALLENGE_LIFETIME_NS, ClientFrame, RefusalReason, SiblingFrame, SlotId, UnlockHello,
    UnlockRequest, decode_frame, encode_frame, frame_len,
};
use tilo_user_identity::{
    CertifiedFleet, DeviceCert, device_cert_signing_digest,
};
use tilo_workspace_service::workspaces::person_workspace::devices::{
    DEVICES, Device, Devices, UnlockAnswer,
};

use crate::PersonResources;

/// The fact under which a device keeps its enrolments and the auto-approvals the person set on it,
/// in its entry of the record of their devices: its [`UnlockFabricState`] without the answers,
/// which are the record's own ([`UnlockAnswer`]).
const UNLOCK: &str = "unlock";

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
    /// The device that asks, as the person's store names it.
    pub device: String,
    /// The slot it wants unlocked.
    pub slot: String,
}

/// The person's devices this session answers as, read from the record of their own workspace
/// through this computer's daemon, and whether that daemon keeps their root.
struct Ownership {
    fleet: CertifiedFleet,
    /// Whether this device holds the person's root, so it can release.
    key_holder: bool,
}

/// The sibling side of disk unlock, served by one session.
pub(crate) struct Unlock {
    link: SharedLink,
    /// The person's own workspace and this device's certificate there: where this device keeps its
    /// enrolments and the person's answers. A session without it answers boots but keeps nothing.
    record: Option<(PersonResources, DeviceCert)>,
    questions: Option<AskSender>,
    /// This device's fleet key: the sibling boots answer.
    local: [u8; 32],
    /// The challenges this device issued, the boots it served and the requests waiting for it. One
    /// session answers as one device.
    sibling: Mutex<UnlockSibling>,
}

impl Unlock {
    /// The sibling side for the device whose fleet key is `local` , asking `questions` where it
    /// needs the person's consent.
    pub(crate) fn new(
        link: SharedLink,
        record: Option<(PersonResources, DeviceCert)>,
        local: [u8; 32],
        questions: Option<AskSender>,
    ) -> Self {
        Self {
            link,
            record,
            questions,
            local,
            sibling: Mutex::new(UnlockSibling::new(local)),
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
        let Some(ownership) = read_ownership() else {
            tell(send, SiblingFrame::Refused(RefusalReason::NotAKeyHolder)).await;
            return;
        };
        let published = self.published(&ownership);
        let challenge = self.challenge(&hello, &ownership, &published);
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
        let boot_digest = boot_cert_digest(&hello.boot_cert);
        let request_digest = request
            .digest(&hello.boot_cert, &self.local)
            .unwrap_or_default();
        let allowed_before = boot_digest
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
            let release = match &boot_digest {
                Some(digest) if allowed_before => {
                    self.release(
                        &ownership,
                        &published,
                        digest,
                        &request_digest,
                        &request.ephemeral_public,
                    )
                    .await
                }
                _ => None,
            };
            let view = view_of(&ownership, &published, release.as_ref());
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
        let digest = request_digest;
        if !write_sibling(&mut send, &frame).await {
            // The boot is gone before it could read even that: nobody will wait for a decision on
            // its behalf.
            self.lock().withdraw(&digest);
            return;
        }
        // The person is asked here, and the sheet shows nothing more of it once they answered or
        // the boot went away.
        let asking = boot_digest
            .and_then(|digest| published.enrolments.get(&digest))
            .map(UnlockEnrolment::device);
        let Some(asking) = asking else {
            self.lock().withdraw(&digest);
            return;
        };
        let asked = self.ask(asking, &pending, &request);
        let Some(answer) = wait_for_person(asked, &send).await else {
            self.lock().withdraw(&digest);
            return;
        };
        let approved = answer.is_some();
        // Released only with the person's consent, and only for a boot still waiting: a decision on
        // a request that went away decides nothing.
        if let Some(decided) = self.decide(&ownership, &published, &digest, approved).await {
            if let Some(record) = decided.record {
                self.keep(&record);
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
        ownership: &Ownership,
        published: &MergedUnlock,
    ) -> SiblingFrame {
        let view = view_of(ownership, published, None);
        self.lock().hello(
            hello,
            &view,
            crate::platform::fabric::nonzero::<32>(),
            tilo_clock::unix_now_ns(),
        )
    }

    /// The challenges this device issued and the requests waiting for it.
    fn lock(&self) -> std::sync::MutexGuard<'_, UnlockSibling> {
        self.sibling
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The boot's half, opened by the daemon that keeps the person's root and sealed to the
    /// boot's ephemeral key: nothing here holds the secret-sync key, and the half never appears
    /// in this process.
    async fn release(
        &self,
        ownership: &Ownership,
        published: &MergedUnlock,
        boot_cert_digest: &[u8; 32],
        request_digest: &[u8; 32],
        ephemeral_public: &[u8; 32],
    ) -> Option<tilo_unlock_protocol::Release> {
        if !ownership.key_holder {
            return None;
        }
        let enrolment = published.enrolments.get(boot_cert_digest)?;
        let args = serde_json::json!({
            "cert": hex::encode(postcard::to_allocvec(&enrolment.cert).ok()?),
            "sealed_half": hex::encode(postcard::to_allocvec(&enrolment.sealed_half).ok()?),
            "request_digest": hex::encode(request_digest),
            "ephemeral": hex::encode(ephemeral_public),
        });
        tokio::task::spawn_blocking(move || {
            tilo_api::daemon::this_computer()
                .ok()?
                .act(tilo_device_identity::login::RELEASE_HALF, args)
                .ok()?
                .get("release")?
                .as_str()
                .and_then(|release| {
                    let bytes = hex::decode(release)
                        .inspect_err(
                            |error| tracing::warn!(%error, "the daemon's release is not hex"),
                        )
                        .ok()?;
                    postcard::from_bytes::<tilo_unlock_protocol::Release>(&bytes)
                        .inspect_err(
                            |error| tracing::warn!(%error, "the daemon's release does not read"),
                        )
                        .ok()
                })
        })
        .await
        .ok()
        .flatten()
    }

    /// Record the person's decision on the request `digest` , for the boot still waiting.
    async fn decide(
        &self,
        ownership: &Ownership,
        published: &MergedUnlock,
        digest: &[u8; 32],
        approved: bool,
    ) -> Option<Decided> {
        let pending = self
            .lock()
            .pending()
            .find(|pending| pending.request_digest == *digest)
            .cloned();
        let release = match pending {
            Some(pending) if approved => {
                self.release(
                    ownership,
                    published,
                    &pending.enrolment,
                    &pending.request_digest,
                    &pending.ephemeral_public,
                )
                .await
            }
            _ => None,
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
        let view = view_of(ownership, published, release.as_ref());
        self.lock()
            .decide(digest, decision, &view, tilo_clock::unix_now_ns())
    }

    /// Put the boot's request to the person, with the code its screen shows,
    /// and let dropping the question go with it.
    fn ask(&self, device: [u8; 32], approval_code: &str, request: &UnlockRequest) -> Option<Asked> {
        let questions = self.questions.as_ref()?;
        let handle = format!("device-unlock:{}", hex::encode(request.boot_id.0));
        let device = self.label_of(&device);
        Some(
            questions.ask(Question {
                handle,
                kind: KIND.to_owned(),
                app_id: None,
                label: device.clone(),
                details: serde_json::to_value(UnlockQuestion {
                    approval_code: approval_code.to_owned(),
                    device,
                    slot: slot_name(&request.slot),
                })
                .unwrap_or_default(),
            }),
        )
    }

    /// The name the person's store holds for `device` .
    fn label_of(&self, device: &[u8; 32]) -> String {
        let key = hex::encode(device);
        self.link
            .devices()
            .devices()
            .iter()
            .find(|(id, _)| id.as_str() == key)
            .map_or_else(|| key, |(_, device)| device.about.label.clone())
    }

    /// The enrolments, auto-approvals and answers the person's store holds now, under the fleet
    /// this device trusts.
    fn published(&self, ownership: &Ownership) -> MergedUnlock {
        let fabric = self.link.devices();
        merged_unlock_state(
            fabric.devices().values().map(|device| &device.unlock),
            &ownership.fleet,
            tilo_clock::unix_now_ns(),
        )
    }

    /// Keep the person's answer in the record of their devices, so their other devices answer the
    /// same boot the same way while it holds.
    fn keep(&self, record: &UnlockDecisionEntry) {
        let Some((resources, _)) = &self.record else {
            tracing::warn!("the person's answer to a boot is not kept: no record of their devices");
            return;
        };
        let answer = answer_of(record);
        resources.apply(DEVICES, move || Devices::answering(&answer));
    }

}

/// What the sibling state machine reads: the fleet this device trusts and the person's fabric as it
/// holds it.
fn view_of<'a>(
    ownership: &'a Ownership,
    published: &'a MergedUnlock,
    release: Option<&'a tilo_unlock_protocol::Release>,
) -> UnlockView<'a> {
    UnlockView {
        fleet: &ownership.fleet,
        enrolments: &published.enrolments,
        auto_approvals: &published.auto_approvals,
        decisions: &published.decisions,
        key_holder: ownership.key_holder,
        release,
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
        Err(_) => tilo_failure::ignored!("no answer within the window is a refusal", Some(None)),
    }
}

/// The enrolment a boot's certificate is known by.
fn boot_cert_digest(cert: &DeviceCert) -> Option<[u8; 32]> {
    device_cert_signing_digest(cert).ok()
}

/// The slot as the boot screen names it, and as the person's own record of their answers keeps it.
fn slot_name(slot: &SlotId) -> String {
    format!("{}:{}", hex::encode(slot.volume_uuid), slot.keyslot)
}

/// What `device`, whose entry of the person's devices `record` is `kept`, keeps about disk unlock:
/// its enrolments and auto-approvals, and the answers the person gave on it that the record holds,
/// the newest where it holds more than one device publishes. Nothing where that does not hold for
/// the device.
pub(crate) fn kept_unlock(record: &Devices, kept: &Device, device: &DeviceId) -> UnlockFabricState {
    let mut state = kept
        .facts
        .get(UNLOCK)
        .and_then(|value| crate::platform::fabric::decoded::<UnlockFabricState>(value))
        .unwrap_or_default();
    let mut answers = record
        .unlocks
        .values()
        .filter(|answer| answer.device == device.as_str())
        .filter_map(decision_of)
        .collect::<Vec<_>>();
    answers.sort_by_key(|answer| answer.decided_at_ns);
    let over = answers.len().saturating_sub(MAX_UNLOCK_DECISIONS);
    answers.drain(..over);
    state.decisions = answers;
    let owner = hex::decode(device.as_str())
        .ok()
        .and_then(|key| <[u8; 32]>::try_from(key).ok());
    if owner.is_none_or(|owner| state.validate(&owner).is_err()) {
        return UnlockFabricState::default();
    }
    state
}

/// `answer` as the record keeps it, by plain keys.
fn answer_of(answer: &UnlockDecisionEntry) -> UnlockAnswer {
    UnlockAnswer {
        device: hex::encode(answer.decided_by),
        enrolment: hex::encode(answer.boot_cert_digest),
        boot: hex::encode(answer.boot_id.0),
        slot: slot_name(&answer.slot),
        allowed: answer.outcome == DecisionOutcome::Approve,
        at_ns: i64::try_from(answer.decided_at_ns).unwrap_or(i64::MAX),
        until_ns: i64::try_from(answer.valid_until_ns).unwrap_or(i64::MAX),
    }
}

/// The answer the record keeps as `answer`; `None` for one no device of this build wrote.
fn decision_of(answer: &UnlockAnswer) -> Option<UnlockDecisionEntry> {
    let (volume, keyslot) = answer.slot.split_once(':')?;
    Some(UnlockDecisionEntry {
        boot_cert_digest: bytes(&answer.enrolment)?,
        boot_id: tilo_unlock_protocol::BootId(bytes(&answer.boot)?),
        slot: SlotId {
            volume_uuid: bytes(volume)?,
            keyslot: keyslot.parse().ok()?,
        },
        outcome: if answer.allowed {
            DecisionOutcome::Approve
        } else {
            DecisionOutcome::Deny
        },
        decided_by: bytes(&answer.device)?,
        decided_at_ns: u64::try_from(answer.at_ns).ok()?,
        valid_until_ns: u64::try_from(answer.until_ns).ok()?,
    })
}

/// The `N` bytes `text` holds in hex.
fn bytes<const N: usize>(text: &str) -> Option<[u8; N]> {
    let mut out = [0; N];
    hex::decode_to_slice(text, &mut out).ok()?;
    Some(out)
}

/// The person's devices, read from the record of their own workspace through this computer's
/// daemon, and whether that daemon keeps their root.
fn read_ownership() -> Option<Ownership> {
    use tilo_server_protocol::devices::{DEVICES, kept, record_value};
    let who = tilo_api::daemon::who::who().ok()?;
    let person = who.person?;
    let place = tilo_api::daemon::this_computer().ok()?;
    let records = record_value(&place.read_record(DEVICES.0, DEVICES.1).ok()?);
    let mut fleet = CertifiedFleet::new(person);
    if let Some(devices) = records
        .get("devices")
        .and_then(serde_json::Value::as_object)
    {
        for key in devices.keys() {
            let Some((cert, revocation)) = kept(&records, key, &person) else {
                continue;
            };
            let _ = fleet.certify(cert);
            if let Some(revocation) = revocation {
                let _ = fleet.revoke(revocation);
            }
        }
    }
    Some(Ownership {
        fleet,
        key_holder: who.holds_root,
    })
}

async fn read_client(recv: &mut RecvStream) -> Option<ClientFrame> {
    read_frame(recv).await
}

/// Read one length-prefixed frame of `T`, within a boot's answering time.
async fn read_frame<T: DeserializeOwned>(recv: &mut RecvStream) -> Option<T> {
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
    // A frame that does not decode is not one this protocol knows: the caller
    // closes the connection.
    tilo_failure::ignored!(
        "a frame this protocol does not know is not a request",
        decode_frame(&body).ok()
    )
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
