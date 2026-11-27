//! Another of the person's devices asking for its disk-unlock half.
//! Every device of the person holds the sibling halves of the others, sealed to the person's
//! root-derived secret-sync key ([`sealed`]). Early boot of an enrolled device asks this device
//! over [`tilo_unlock_protocol::UNLOCK_ALPN`]: it presents the boot key certificate the person's
//! root signed, answers this device's challenge, and waits. The person's yes is an asked record
//! in their own workspace ([`asked`]), answering the release it would allow, with the code the
//! boot screen shows among its facts; any of their devices shows it, and one of them answers it
//! with the record's `resolve`. An approve releases the half sealed to the boot's ephemeral key,
//! and the next boot needs another approval.
//! The answer is kept in the record of the person's devices
//! ([`tilo_workspace_service::workspaces::person_workspace::devices::UnlockAnswer`]), so a boot
//! whose stream went away before the answer reached it hears the same answer again, with a half
//! sealed to its own new key where the person allowed it, instead of asking them twice, and the
//! record's history is their list of what they answered.
//! A device's enrolment is its daemon's to publish, in its own entry of that record: the boot key
//! certified by the person's root, and the sibling half sealed to their secret-sync key.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use iroh::endpoint::{Connection, RecvStream, SendStream};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tilo_actions::ActionAddress;
use tilo_actions::InvocationId;
use tilo_actions::remote::asked::{
    AskedBy, AskedDispatch, AskedFacts, AskedRecords, Fact, Named, RecordRef,
};
use tilo_actions::remote::{
    ActionCallError, Caller, Completer, InvokeArgument, InvokeRequest, Pending, RemoteActionTarget,
    pending,
};
use tilo_device_identity::login::{PROVIDER, RELEASE_HALF, RESOURCE};
use tilo_remote_actions::asked::AskedStore;
use tilo_unlock_protocol::published::{
    AutoApproval, DecisionKey, DecisionOutcome, MAX_UNLOCK_DECISIONS, UnlockDecisionEntry,
    UnlockEnrolment, UnlockFabricState, decision_for,
};
use tilo_unlock_protocol::{
    APPROVAL_TIMEOUT_NS, BootId, CHALLENGE_LIFETIME_NS, Challenge, ClientFrame, PROTOCOL_VERSION,
    RefusalReason, Release, SiblingFrame, SlotId, UnlockHello, UnlockRequest, approval_code,
    attest, decode_frame, encode_frame, frame_len,
};
use tilo_user_identity::{
    CertifiedFleet, DeviceCapabilities, DeviceCert, DeviceCertification,
    device_cert_signing_digest, verify_device_cert,
};
use tilo_workspace_service::workspaces::person_workspace::devices::{
    DEVICES, Device, Devices, UnlockAnswer,
};
use tilo_workspace_service::workspaces::person_workspace::{self, devices};

use super::services::{holds_root_for_person, root_record_for_person};
use super::workspaces::ServiceWorkspaces;

/// The name this device raises the boot's question under, in the store of the calls that wait for
/// the person: an approve of one of its records comes back here.
const DISPATCH: &str = "device-unlock";

/// How long a boot may take to answer its challenge.
const ANSWER_TIMEOUT: Duration = Duration::from_nanos(CHALLENGE_LIFETIME_NS);
/// How long an approved boot may take to read the half once decided.
const WRITE_GRACE: Duration = Duration::from_secs(2);

/// Requests one device may start within [`RATE_WINDOW_NS`].
const MAX_REQUESTS_PER_WINDOW: usize = 5;
/// The rate-limit window.
const RATE_WINDOW_NS: u64 = 15 * 60 * 1_000_000_000;
/// Requests waiting for the person at once, across all devices.
const MAX_PENDING: usize = 16;
/// How long the person's answer stands for the boot that asked.
const DECISION_LIFETIME_NS: u64 = APPROVAL_TIMEOUT_NS;
/// Boots remembered as served, oldest dropped first.
const MAX_SERVED_BOOTS: usize = 1024;
/// Challenges outstanding at once, oldest dropped first.
const MAX_OPEN_CHALLENGES: usize = 256;

/// The sibling side of disk unlock, served by this device's daemon.
pub(crate) struct Unlock {
    workspaces: Arc<ServiceWorkspaces>,
    /// The person this device is one of, and this device's own key: the fleet key a boot is
    /// answered by.
    person: [u8; 32],
    local: [u8; 32],
    /// The one store of the calls that wait for the person, where the boot's question is kept,
    /// and the records written there as this device's.
    store: Arc<AskedStore>,
    records: Arc<dyn AskedRecords>,
    /// What delivers the person's answer to the exchange waiting for it.
    asks: Arc<Asks>,
    /// The challenges this device issued, the boots it served and the requests waiting for it.
    sibling: Mutex<UnlockSibling>,
}

/// The boot questions the person is asked, by the invocation their record names: the answer an
/// approve or a decline delivers completes the one the exchange waits on.
#[derive(Default)]
struct Asks {
    waiting: Mutex<HashMap<InvocationId, Completer<bool>>>,
}

impl Asks {
    /// Answer the question `invocation` asks with the person's `allowed`, where it still waits
    /// here: a question whose boot went away waits no longer.
    fn answered(&self, invocation: InvocationId, allowed: bool) {
        let waiting = self
            .waiting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&invocation);
        if let Some(waiting) = waiting {
            waiting.complete(allowed);
        }
    }
}

/// What the person's answer reaches: the question the exchange waits on, and nothing of the
/// boot's own kept with it.
impl AskedDispatch for Asks {
    fn context(&self, _: InvocationId) -> Value {
        Value::Null
    }

    fn readmit(
        &self,
        _: &Caller,
        request: InvokeRequest,
        _: &Value,
    ) -> Result<(), ActionCallError> {
        self.answered(request.invocation, true);
        Ok(())
    }

    fn declined(&self, _: &Caller, invocation: InvocationId) -> Result<(), ActionCallError> {
        self.answered(invocation, false);
        Ok(())
    }

    fn withdrawn(&self, _: &Caller, invocation: InvocationId, _: &str) {
        self.answered(invocation, false);
    }
}

impl std::fmt::Debug for Unlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Unlock")
            .field("device", &hex::encode(self.local))
            .finish_non_exhaustive()
    }
}

/// The sibling side of `workspaces`' device, keeping the boot questions in `store`: what the
/// device's endpoint answers early boots with, once the calls that wait for its person are kept.
pub(crate) fn serving(workspaces: &Arc<ServiceWorkspaces>, store: Arc<AskedStore>) -> Arc<Unlock> {
    let unlock = Unlock::new(Arc::clone(workspaces), &workspaces.device_cert(), store);
    unlock.withdraw_ended();
    unlock
}

impl Unlock {
    /// The sibling side of the device whose certificate is `cert`, keeping its questions in
    /// `store` and the answers in the record of its person's devices.
    pub(crate) fn new(
        workspaces: Arc<ServiceWorkspaces>,
        cert: &DeviceCert,
        store: Arc<AskedStore>,
    ) -> Arc<Self> {
        let (person, local) = (cert.user_public_key, cert.device_public_key);
        let asks = Arc::new(Asks::default());
        let holding: Weak<Asks> = Arc::downgrade(&asks);
        let dispatch: Weak<dyn AskedDispatch> = holding;
        // Each question is told to the person as their own log's, with Approve and Decline
        // answering its record, as every asked call is. The task that tells them runs on this
        // device's runtime, which the daemon start may not be in yet.
        let _entered = tilo_rt::app().handle().enter();
        let records = tilo_person_services::PersonServices::of(workspaces.copies())
            .told(store.for_dispatch(DISPATCH, dispatch));
        Arc::new(Self {
            workspaces,
            person,
            local,
            records,
            store,
            asks,
            sibling: Mutex::new(UnlockSibling::new(local)),
        })
    }

    /// Take back the questions a boot asked before this daemon started: no connection waits for
    /// one any more, so nobody can be told its answer.
    pub(crate) fn withdraw_ended(&self) {
        for (at, _, _) in self.store.asked_by(DISPATCH) {
            self.store.withdraw(&at, &crate::i18n::t("unlock-gone"));
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
        let Some(ownership) = self.ownership().await else {
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
        // The person's answer to this boot, when their devices kept one, is the whole exchange:
        // an approval needs the half, a denial needs nothing loaded.
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
                    self.sealed(
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
                finish(&mut send).await;
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
        let asking = boot_digest
            .and_then(|digest| published.enrolments.get(&digest))
            .map(UnlockEnrolment::device);
        let Some(asking) = asking else {
            self.lock().withdraw(&digest);
            return;
        };
        // The person is asked here, and the question is gone from their list once they answered
        // or the boot went away.
        let answer = match self.ask(asking, &pending, &request).await {
            Some((at, answer)) => wait_for_person(&self.store, &at, answer, &send).await,
            // Nothing here can be asked: the boot is refused as if nobody answered.
            None => Some(false),
        };
        let Some(allowed) = answer else {
            self.lock().withdraw(&digest);
            return;
        };
        // Released only with the person's consent, and only for a boot still waiting: a decision
        // on a request that went away decides nothing.
        if let Some(decided) = self.decide(&ownership, &published, &digest, allowed).await {
            if let Some(record) = decided.record {
                self.keep(&record).await;
            }
            if !write_sibling(&mut send, &decided.frame).await {
                tracing::warn!("a boot was not told the decision on its unlock request");
            }
        }
        finish(&mut send).await;
    }

    /// Challenge the boot, or refuse its hello.
    fn challenge(
        &self,
        hello: &UnlockHello,
        ownership: &Ownership,
        published: &MergedUnlock,
    ) -> SiblingFrame {
        let view = view_of(ownership, published, None);
        let Some(nonce) = fresh_nonce() else {
            return refuse(RefusalReason::NotAKeyHolder);
        };
        self.lock()
            .hello(hello, &view, nonce, tilo_clock::unix_now_ns())
    }

    /// The challenges this device issued and the requests waiting for it.
    fn lock(&self) -> std::sync::MutexGuard<'_, UnlockSibling> {
        self.sibling
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// What this device answers as: the person's devices, read from the record of their own
    /// workspace here, with the fleet this device trusts, and whether it keeps their root.
    async fn ownership(&self) -> Option<Ownership> {
        if !person_workspace::holds_person_workspace_of(self.workspaces.copies(), &self.person)
            .await
        {
            return None;
        }
        let record = devices::of(self.workspaces.copies(), &self.person).await;
        let mut fleet = CertifiedFleet::new(self.person);
        for listed in record.listed(&self.person) {
            let _ = fleet.certify(listed.cert.clone());
            if let Some(revocation) = listed.device.revocation(&listed.cert) {
                let _ = fleet.revoke(revocation);
            }
        }
        Some(Ownership {
            fleet,
            key_holder: holds_root_for_person(self.person),
            record,
        })
    }

    /// The enrolments, auto-approvals and answers the record of the person's devices holds now,
    /// under the fleet this device trusts.
    fn published(&self, ownership: &Ownership) -> MergedUnlock {
        let now = tilo_clock::unix_now_ns();
        let states: Vec<UnlockFabricState> = ownership
            .record
            .listed(&self.person)
            .into_iter()
            .map(|listed| {
                let key = listed.cert.device_public_key;
                kept_unlock(&ownership.record, &listed.device, &key)
            })
            .collect();
        merged_unlock_state(states.iter(), &ownership.fleet, now)
    }

    /// The boot's half, opened by the root this device keeps and sealed to the boot's ephemeral
    /// key: nothing here holds the secret-sync key, and the half never leaves this process.
    async fn sealed(
        &self,
        ownership: &Ownership,
        published: &MergedUnlock,
        boot_cert_digest: &[u8; 32],
        request_digest: &[u8; 32],
        ephemeral_public: &[u8; 32],
    ) -> Option<Release> {
        if !ownership.key_holder {
            return None;
        }
        let enrolment = published.enrolments.get(boot_cert_digest)?;
        let cert = postcard::to_allocvec(&enrolment.cert).ok()?;
        let sealed_half = postcard::to_allocvec(&enrolment.sealed_half).ok()?;
        let args = json!({
            "cert": hex::encode(cert),
            "sealed_half": hex::encode(sealed_half),
            "request_digest": hex::encode(request_digest),
            "ephemeral": hex::encode(ephemeral_public),
        });
        let person = self.person;
        tokio::task::spawn_blocking(move || {
            let record = root_record_for_person(person).ok()?;
            let rooted = super::signing::Rooted::at(&record).ok()?;
            let answer = super::signing::release_half(&args, &rooted)
                .inspect_err(|error| tracing::warn!(%error, "the half was not released"))
                .ok()?;
            let text = answer.get("release")?.as_str()?;
            let bytes = hex::decode(text)
                .inspect_err(|error| tracing::warn!(%error, "the release is not hex"))
                .ok()?;
            postcard::from_bytes::<Release>(&bytes)
                .inspect_err(|error| tracing::warn!(%error, "the release does not read"))
                .ok()
        })
        .await
        .ok()
        .flatten()
    }

    /// Record the person's decision on the request `digest`, for the boot still waiting.
    async fn decide(
        &self,
        ownership: &Ownership,
        published: &MergedUnlock,
        digest: &[u8; 32],
        allowed: bool,
    ) -> Option<Decided> {
        let pending = self
            .lock()
            .pending()
            .find(|pending| pending.request_digest == *digest)
            .cloned();
        let release = match pending {
            Some(pending) if allowed => {
                self.sealed(
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
        let decision = if allowed {
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

    /// Put the boot's request to the person as an asked record, with the code its screen shows,
    /// and answer the question with its record's `resolve`.
    async fn ask(
        &self,
        device: [u8; 32],
        approval_code: &str,
        request: &UnlockRequest,
    ) -> Option<(RecordRef, Pending<bool>)> {
        let computer = self.name_of(device).await;
        let facts = asked_facts(&computer, device, approval_code, &request.slot);
        let workspace = tilo_server_protocol::servers::person_workspace_id(&self.person);
        let caller = Caller::authenticated(self.person, self.local);
        let invocation = InvocationId::new();
        let asked = InvokeRequest {
            asks: false,
            on_behalf_of: None,
            invocation,
            target: RemoteActionTarget {
                provider: PROVIDER.to_owned(),
                address: ActionAddress::new(RESOURCE, RELEASE_HALF),
            },
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let (completer, answer) = pending();
        self.asks
            .waiting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(invocation, completer);
        match self
            .records
            .write_asked(&workspace, &caller, &asked, &facts)
        {
            Ok(at) => Some((at, answer)),
            Err(why) => {
                self.asks
                    .waiting
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&invocation);
                tracing::warn!(%why, "a boot's question was not kept for the person to answer");
                None
            }
        }
    }

    /// The name the person's devices record holds for `device`.
    async fn name_of(&self, device: [u8; 32]) -> String {
        devices::of(self.workspaces.copies(), &self.person)
            .await
            .listed(&self.person)
            .into_iter()
            .find(|listed| listed.cert.device_public_key == device)
            .map_or_else(
                || hex::encode(device),
                |listed| listed.device.called(&listed.cert),
            )
    }

    /// Keep the person's answer in the record of their devices, so their other devices answer the
    /// same boot the same way while it holds.
    async fn keep(&self, record: &UnlockDecisionEntry) {
        let answer = answer_of(record);
        let workspace = tilo_server_protocol::servers::person_workspace_id(&self.person);
        if let Err(error) = person_workspace::apply_on(
            self.workspaces.copies(),
            &workspace,
            DEVICES,
            Devices::answering(&answer),
        )
        .await
        {
            tracing::warn!(%error, "the person's answer to a boot is not kept");
        }
    }
}

/// The protocol an early boot's connection is answered on.
#[derive(Clone, Debug)]
pub(crate) struct Protocol(Arc<Unlock>);

impl Protocol {
    /// The handler answering early boots as `unlock`.
    pub(crate) fn new(unlock: Arc<Unlock>) -> Self {
        Self(unlock)
    }
}

impl iroh::protocol::ProtocolHandler for Protocol {
    async fn accept(&self, connection: Connection) -> Result<(), iroh::protocol::AcceptError> {
        let unlocking = Arc::clone(&self.0);
        tokio::spawn(unlocking.serve(connection));
        Ok(())
    }
}

/// What the person is asked for a boot's request: which of their devices it unlocks, for which
/// disk, and the code its boot screen shows, to compare with the one here.
fn asked_facts(computer: &str, device: [u8; 32], approval_code: &str, slot: &SlotId) -> AskedFacts {
    let slot = slot_name(slot);
    AskedFacts {
        by: AskedBy::Device(Named {
            id: hex::encode(device),
            name: computer.to_owned(),
        }),
        words: crate::i18n::t_args("unlock-asked", &[("device", computer.into())]),
        facts: vec![
            Fact::new(
                "",
                crate::i18n::t_args("unlock-detail", &[("slot", slot.as_str().into())]),
            ),
            Fact::new(crate::i18n::t("unlock-code"), approval_code.to_owned()),
        ],
        ..AskedFacts::default()
    }
}

/// The person's devices this device answers as, read from the record of their own workspace here,
/// and whether it keeps their root.
struct Ownership {
    fleet: CertifiedFleet,
    /// Whether this device holds the person's root, so it can release.
    key_holder: bool,
    record: Devices,
}

/// What the sibling state machine reads: the fleet this device trusts and the person's devices as
/// the record holds them.
fn view_of<'a>(
    ownership: &'a Ownership,
    published: &'a MergedUnlock,
    release: Option<&'a Release>,
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

/// Wait for the person's answer to `at`, whose boot is still waiting on `send`: their yes or no,
/// or `None` when the boot stopped waiting before one came.
async fn wait_for_person(
    store: &AskedStore,
    at: &RecordRef,
    answer: Pending<bool>,
    send: &SendStream,
) -> Option<bool> {
    let wait = tokio::time::timeout(Duration::from_nanos(APPROVAL_TIMEOUT_NS), async {
        tokio::select! {
            allowed = answer => Some(allowed),
            // The boot stopped waiting: its question is taken back rather than answered for nobody.
            _ = send.stopped() => None,
        }
    })
    .await;
    match wait {
        // Their yes or no.
        Ok(Some(Some(allowed))) => Some(allowed),
        // The question ended without them answering it, or the boot stopped waiting: either
        // way nobody waits for an answer any more.
        Ok(Some(None) | None) => {
            store.withdraw(at, &crate::i18n::t("unlock-gone"));
            None
        }
        Err(_) => {
            // Nobody answered in time, which refuses this boot, and nobody waits for the answer
            // any more.
            store.withdraw(at, &crate::i18n::t("unlock-unanswered"));
            Some(false)
        }
    }
}

/// Fresh bytes from the host's generator, never all zero: the challenge format reserves zero for
/// "none".
fn fresh_nonce() -> Option<[u8; 32]> {
    loop {
        let mut nonce = [0_u8; 32];
        if getrandom::fill(&mut nonce).is_err() {
            tracing::warn!("the host's random generator failed: a boot is not challenged");
            return None;
        }
        if nonce.iter().any(|byte| *byte != 0) {
            return Some(nonce);
        }
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

/// Finish the stream once the boot has read what was written, giving it time to.
async fn finish(send: &mut SendStream) {
    if let Err(error) = send.finish() {
        tracing::warn!(%error, "an unlock decision did not reach the boot that asked");
    }
    let _ = tokio::time::timeout(WRITE_GRACE, send.stopped()).await;
}

/// The enrolment a boot's certificate is known by.
fn boot_cert_digest(cert: &DeviceCert) -> Option<[u8; 32]> {
    device_cert_signing_digest(cert).ok()
}

/// The slot as the boot screen names it, and as the person's own record of their answers keeps it.
fn slot_name(slot: &SlotId) -> String {
    format!("{}:{}", hex::encode(slot.volume_uuid), slot.keyslot)
}

/// What the device `key`, whose entry of the person's devices `kept` is, keeps about disk unlock:
/// its enrolments and auto-approvals, and the newest answers the person gave on it that the record
/// keeps. Nothing where that is not true of the device.
fn kept_unlock(record: &Devices, kept: &Device, key: &[u8; 32]) -> UnlockFabricState {
    let device = hex::encode(key);
    let mut state = kept
        .facts
        .get(super::boot_unlock::UNLOCK_FACT)
        .and_then(|value| decoded::<UnlockFabricState>(value))
        .unwrap_or_default();
    let mut answers = record
        .unlocks
        .values()
        .filter(|answer| answer.device == device)
        .filter_map(decision_of)
        .collect::<Vec<_>>();
    answers.sort_by_key(|answer| answer.decided_at_ns);
    let over = answers.len().saturating_sub(MAX_UNLOCK_DECISIONS);
    answers.drain(..over);
    state.decisions = answers;
    if state.validate(key).is_err() {
        return UnlockFabricState::default();
    }
    state
}

/// The value `hex` holds, postcard in hex, when it reads as a `T`.
fn decoded<T: DeserializeOwned>(hex: &str) -> Option<T> {
    tilo_failure::ignored!(
        "a value no build of this person's wrote is not one of their devices' to read",
        postcard::from_bytes(&hex::decode(hex).ok()?).ok()
    )
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
        boot_id: BootId(bytes(&answer.boot)?),
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

/// What the person's devices hold about disk unlock: the enrolments and auto-approvals their
/// record carries, and the answers to boots that are still in force.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct MergedUnlock {
    enrolments: BTreeMap<[u8; 32], UnlockEnrolment>,
    auto_approvals: BTreeMap<[u8; 32], AutoApproval>,
    decisions: BTreeMap<DecisionKey, UnlockDecisionEntry>,
}

/// The enrolments, auto-approvals and answers across the person's devices, keeping only those of
/// the fleet's root and of devices it has not revoked. The newest auto-approval per device wins,
/// and the newest answer per boot, an approval over a denial made at the same moment.
fn merged_unlock_state<'a>(
    snapshots: impl IntoIterator<Item = &'a UnlockFabricState>,
    fleet: &CertifiedFleet,
    now_ns: u64,
) -> MergedUnlock {
    let mut merged = MergedUnlock::default();
    let certification = |device: &[u8; 32]| fleet.certification(device, now_ns);
    for state in snapshots {
        for enrolment in &state.enrolments {
            if enrolment.cert.user_public_key == *fleet.user_public_key()
                && certification(&enrolment.device()) == DeviceCertification::Certified
                && let Some(id) = enrolment.id()
            {
                merged.enrolments.insert(id, enrolment.clone());
            }
        }
        for approval in &state.auto_approvals {
            if approval.user_public_key != *fleet.user_public_key()
                || certification(&approval.device_public_key) != DeviceCertification::Certified
            {
                continue;
            }
            let newer = merged
                .auto_approvals
                .get(&approval.device_public_key)
                .is_none_or(|current| approval.issued_at_ns > current.issued_at_ns);
            if newer {
                merged
                    .auto_approvals
                    .insert(approval.device_public_key, approval.clone());
            }
        }
        for decision in &state.decisions {
            if certification(&decision.decided_by) != DeviceCertification::Certified
                || decision.valid_until_ns <= now_ns
            {
                continue;
            }
            let newer = merged.decisions.get(&decision.key()).is_none_or(|current| {
                (
                    decision.decided_at_ns,
                    decision.outcome == DecisionOutcome::Approve,
                ) >= (
                    current.decided_at_ns,
                    current.outcome == DecisionOutcome::Approve,
                )
            });
            if newer {
                merged.decisions.insert(decision.key(), decision.clone());
            }
        }
    }
    merged
}

/// What a sibling knows when it answers: the person's fleet, the enrolments, auto-approvals and
/// answers the record carries, and the half when this device holds the root.
struct UnlockView<'a> {
    fleet: &'a CertifiedFleet,
    enrolments: &'a BTreeMap<[u8; 32], UnlockEnrolment>,
    auto_approvals: &'a BTreeMap<[u8; 32], AutoApproval>,
    /// The person's answers still in force, from every device of theirs.
    decisions: &'a BTreeMap<DecisionKey, UnlockDecisionEntry>,
    /// Whether this device holds the person's root, so it can release.
    key_holder: bool,
    /// The boot's half, opened and re-sealed to its ephemeral key by the root kept here: what a
    /// release hands over, and nothing this device can open for another.
    release: Option<&'a Release>,
}

/// A request waiting for the person's decision on `device.unlock`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingUnlock {
    request_digest: [u8; 32],
    approval_code: String,
    enrolment: [u8; 32],
    device_public_key: [u8; 32],
    slot: SlotId,
    boot_id: BootId,
    ephemeral_public: [u8; 32],
    expires_at_ns: u64,
}

/// The person's answer to a pending `device.unlock`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnlockDecision {
    Approve { approver: [u8; 32] },
    Deny { approver: [u8; 32] },
}

/// What the person's answer means for the boot waiting on it: the frame to send it, and the
/// answer to keep for the boot's next attempt — none where the answer releases nothing (the half
/// could not be opened, the device was removed, or nobody decided in time).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Decided {
    frame: SiblingFrame,
    record: Option<UnlockDecisionEntry>,
}

#[derive(Clone, Debug)]
struct OpenChallenge {
    enrolment: [u8; 32],
    challenge: Challenge,
}

/// The sibling's state: outstanding challenges, served boots, rate limits and requests waiting
/// for the person.
#[derive(Debug)]
struct UnlockSibling {
    sibling_public_key: [u8; 32],
    challenges: BTreeMap<[u8; 32], OpenChallenge>,
    challenge_order: VecDeque<[u8; 32]>,
    served: BTreeSet<([u8; 32], BootId)>,
    served_order: VecDeque<([u8; 32], BootId)>,
    attempts: BTreeMap<[u8; 32], VecDeque<u64>>,
    pending: BTreeMap<[u8; 32], PendingUnlock>,
}

impl UnlockSibling {
    fn new(sibling_public_key: [u8; 32]) -> Self {
        Self {
            sibling_public_key,
            challenges: BTreeMap::new(),
            challenge_order: VecDeque::new(),
            served: BTreeSet::new(),
            served_order: VecDeque::new(),
            attempts: BTreeMap::new(),
            pending: BTreeMap::new(),
        }
    }

    /// Answer a hello with a fresh challenge, or refuse. `nonce` must come from the OS CSPRNG.
    fn hello(
        &mut self,
        hello: &UnlockHello,
        view: &UnlockView<'_>,
        nonce: [u8; 32],
        now_ns: u64,
    ) -> SiblingFrame {
        match self.admit(hello, view, now_ns) {
            Ok(enrolment) => {
                let challenge = Challenge {
                    version: PROTOCOL_VERSION,
                    sibling_public_key: self.sibling_public_key,
                    nonce,
                    expires_at_ns: now_ns.saturating_add(CHALLENGE_LIFETIME_NS),
                };
                if self.challenge_order.len() >= MAX_OPEN_CHALLENGES
                    && let Some(oldest) = self.challenge_order.pop_front()
                {
                    self.challenges.remove(&oldest);
                }
                self.challenge_order.push_back(nonce);
                self.challenges.insert(
                    nonce,
                    OpenChallenge {
                        enrolment,
                        challenge: challenge.clone(),
                    },
                );
                SiblingFrame::Challenge(challenge)
            }
            Err(reason) => refuse(reason),
        }
    }

    fn admit(
        &mut self,
        hello: &UnlockHello,
        view: &UnlockView<'_>,
        now_ns: u64,
    ) -> Result<[u8; 32], RefusalReason> {
        if hello.version != PROTOCOL_VERSION {
            return Err(RefusalReason::UnsupportedVersion);
        }
        let cert = &hello.boot_cert;
        if cert.user_public_key != *view.fleet.user_public_key()
            || cert.capabilities != DeviceCapabilities::nothing()
            || verify_device_cert(cert).is_err()
        {
            return Err(RefusalReason::BadCertificate);
        }
        let id = device_cert_signing_digest(cert).map_err(|_| RefusalReason::BadCertificate)?;
        let enrolment = view.enrolments.get(&id).ok_or(RefusalReason::NotEnrolled)?;
        let device = enrolment.device();
        match view.fleet.certification(&device, now_ns) {
            DeviceCertification::Certified => {}
            DeviceCertification::Revoked(_) => return Err(RefusalReason::Revoked),
            DeviceCertification::Uncertified => return Err(RefusalReason::BadCertificate),
        }
        if enrolment.cert != *cert {
            return Err(RefusalReason::NotEnrolled);
        }
        self.charge(device, now_ns)?;
        if !view.key_holder {
            return Err(RefusalReason::NotAKeyHolder);
        }
        Ok(id)
    }

    /// Count one attempt against the device's rate limit.
    fn charge(&mut self, device: [u8; 32], now_ns: u64) -> Result<(), RefusalReason> {
        let attempts = self.attempts.entry(device).or_default();
        while attempts
            .front()
            .is_some_and(|at| now_ns.saturating_sub(*at) >= RATE_WINDOW_NS)
        {
            attempts.pop_front();
        }
        if attempts.len() >= MAX_REQUESTS_PER_WINDOW {
            return Err(RefusalReason::RateLimited);
        }
        attempts.push_back(now_ns);
        Ok(())
    }

    /// Handle the signed answer to `challenge_nonce` (the challenge issued on this stream).
    /// Returns `Pending` (the person is asked through their asked record), `Released` (they
    /// allowed this boot, or auto-approved it), `Denied` (their answer to this boot is in force),
    /// or `Refused`.
    fn request(
        &mut self,
        request: &UnlockRequest,
        hello: &UnlockHello,
        challenge_nonce: &[u8; 32],
        view: &UnlockView<'_>,
        now_ns: u64,
    ) -> SiblingFrame {
        let cert = &hello.boot_cert;
        let (pending, enrolment) =
            match self.check_request(request, cert, challenge_nonce, view, now_ns) {
                Ok(checked) => checked,
                Err(reason) => return refuse(reason),
            };
        let device = pending.device_public_key;
        // The person already answered this boot: it hears that answer again, with a half sealed
        // to this request's own key where they allowed it, instead of asking them a second time.
        if let Some(frame) = self.answered(&pending, view, now_ns) {
            return frame;
        }
        if self
            .served
            .contains(&(pending.device_public_key, pending.boot_id))
        {
            return refuse(RefusalReason::BootAlreadyServed);
        }
        self.forget_unanswered(now_ns);
        if self.pending.len() >= MAX_PENDING {
            return refuse(RefusalReason::RateLimited);
        }
        if auto_approved(request, &enrolment, self.sibling_public_key, view).is_some() {
            let frame = release(&pending, view);
            if matches!(frame, SiblingFrame::Released(_)) {
                self.mark_served(device, request.boot_id);
            }
            return frame;
        }
        let code = pending.approval_code.clone();
        self.pending.insert(pending.request_digest, pending);
        SiblingFrame::Pending {
            approval_code: code,
        }
    }

    /// The shape of a request that passed every check, before the person is asked anything.
    fn check_request(
        &mut self,
        request: &UnlockRequest,
        cert: &DeviceCert,
        challenge_nonce: &[u8; 32],
        view: &UnlockView<'_>,
        now_ns: u64,
    ) -> Result<(PendingUnlock, UnlockEnrolment), RefusalReason> {
        if request.nonce != *challenge_nonce {
            return Err(RefusalReason::UnknownChallenge);
        }
        // Single use: whatever happens next, this challenge is spent.
        let open = self
            .challenges
            .remove(challenge_nonce)
            .ok_or(RefusalReason::UnknownChallenge)?;
        self.challenge_order
            .retain(|nonce| nonce != challenge_nonce);
        // The fleet or the enrolments may have changed since the hello.
        let enrolment = view
            .enrolments
            .get(&open.enrolment)
            .filter(|enrolment| enrolment.cert == *cert)
            .ok_or(RefusalReason::NotEnrolled)?
            .clone();
        let device = enrolment.device();
        if let DeviceCertification::Revoked(_) = view.fleet.certification(&device, now_ns) {
            return Err(RefusalReason::Revoked);
        }
        let digest = request.verify(cert, enrolment.slot, &open.challenge, now_ns)?;
        Ok((
            PendingUnlock {
                request_digest: digest,
                approval_code: approval_code(&digest),
                enrolment: open.enrolment,
                device_public_key: device,
                slot: request.slot,
                boot_id: request.boot_id,
                ephemeral_public: request.ephemeral_public,
                expires_at_ns: now_ns.saturating_add(APPROVAL_TIMEOUT_NS),
            },
            enrolment,
        ))
    }

    /// The person's answer to this boot, while it holds: a denial as it stands, an approval as a
    /// half sealed to this request's own key. A boot that reconnects after its stream went away
    /// is not put to the person again.
    fn answered(
        &mut self,
        pending: &PendingUnlock,
        view: &UnlockView<'_>,
        now_ns: u64,
    ) -> Option<SiblingFrame> {
        let record = decision_for(
            view.decisions,
            &pending.enrolment,
            pending.boot_id,
            pending.slot,
            now_ns,
        )?;
        match record.outcome {
            DecisionOutcome::Deny => Some(SiblingFrame::Denied),
            DecisionOutcome::Approve => {
                let frame = release(pending, view);
                if matches!(frame, SiblingFrame::Released(_)) {
                    self.mark_served(pending.device_public_key, pending.boot_id);
                }
                Some(frame)
            }
        }
    }

    /// Let go of the questions nobody can answer any more: a request whose answer window passed
    /// while its handler was gone is no longer pending, so it holds no place another boot would be
    /// refused for.
    fn forget_unanswered(&mut self, now_ns: u64) {
        self.pending
            .retain(|_, pending| pending.expires_at_ns > now_ns);
    }

    /// The boot waiting on `request_digest` went away: nobody can receive a decision any more, and
    /// the person is asked no further.
    fn withdraw(&mut self, request_digest: &[u8; 32]) {
        self.pending.remove(request_digest);
    }

    /// Requests waiting for the person, by request digest.
    fn pending(&self) -> impl Iterator<Item = &PendingUnlock> {
        self.pending.values()
    }

    /// Apply the person's decision. Returns the frame for the waiting boot and the answer to
    /// keep, or `None` when no such request is pending (already decided, withdrawn, or never
    /// made).
    fn decide(
        &mut self,
        request_digest: &[u8; 32],
        decision: UnlockDecision,
        view: &UnlockView<'_>,
        now_ns: u64,
    ) -> Option<Decided> {
        let pending = self.pending.remove(request_digest)?;
        if pending.expires_at_ns <= now_ns {
            return Some(Decided {
                frame: SiblingFrame::Denied,
                record: None,
            });
        }
        let approver = match decision {
            UnlockDecision::Deny { approver } => {
                return Some(Decided {
                    frame: SiblingFrame::Denied,
                    record: Some(decision_record(
                        &pending,
                        DecisionOutcome::Deny,
                        approver,
                        now_ns,
                    )),
                });
            }
            UnlockDecision::Approve { approver } => approver,
        };
        let still_enrolled = !matches!(
            view.fleet.certification(&pending.device_public_key, now_ns),
            DeviceCertification::Revoked(_)
        ) && view.enrolments.contains_key(&pending.enrolment);
        let frame = if still_enrolled {
            release(&pending, view)
        } else {
            SiblingFrame::Refused(RefusalReason::Revoked)
        };
        let record = match &frame {
            SiblingFrame::Released(_) => {
                self.mark_served(pending.device_public_key, pending.boot_id);
                Some(decision_record(
                    &pending,
                    DecisionOutcome::Approve,
                    approver,
                    now_ns,
                ))
            }
            _ => None,
        };
        Some(Decided { frame, record })
    }

    fn mark_served(&mut self, device: [u8; 32], boot_id: BootId) {
        if self.served_order.len() >= MAX_SERVED_BOOTS
            && let Some(oldest) = self.served_order.pop_front()
        {
            self.served.remove(&oldest);
        }
        self.served_order.push_back((device, boot_id));
        self.served.insert((device, boot_id));
    }
}

/// The answer to a request nobody will be asked about.
fn refuse(reason: RefusalReason) -> SiblingFrame {
    SiblingFrame::Refused(reason)
}

/// The accepted boot chain when the person auto-approves this device and the request's TPM quote
/// proves that chain for this exact request.
fn auto_approved(
    request: &UnlockRequest,
    enrolment: &UnlockEnrolment,
    sibling_public_key: [u8; 32],
    view: &UnlockView<'_>,
) -> Option<[u8; 32]> {
    let device = enrolment.device();
    let policy = view.auto_approvals.get(&device)?;
    if policy.user_public_key != *view.fleet.user_public_key()
        || policy.device_public_key != device
        || policy.validate().is_err()
    {
        return None;
    }
    let key = enrolment.attestation_key.as_deref()?;
    let quote = request.attestation.as_ref()?;
    let challenge = Challenge {
        version: PROTOCOL_VERSION,
        sibling_public_key,
        nonce: request.nonce,
        expires_at_ns: 0,
    };
    let nonce = attest::qualifying_data(
        &enrolment.cert,
        &challenge,
        &request.boot_id,
        &request.ephemeral_public,
    )
    .ok()?;
    let chain = attest::verify_quote(key, quote, &nonce).ok()?;
    policy
        .accepted_boot_chains
        .contains(&chain)
        .then_some(chain)
}

/// The answer to keep for `pending`, in force for [`DECISION_LIFETIME_NS`].
fn decision_record(
    pending: &PendingUnlock,
    outcome: DecisionOutcome,
    decided_by: [u8; 32],
    now_ns: u64,
) -> UnlockDecisionEntry {
    UnlockDecisionEntry {
        boot_cert_digest: pending.enrolment,
        boot_id: pending.boot_id,
        slot: pending.slot,
        outcome,
        decided_by,
        decided_at_ns: now_ns,
        valid_until_ns: now_ns.saturating_add(DECISION_LIFETIME_NS),
    }
}

/// The half the root kept here sealed to this boot's ephemeral key: a release this device holds
/// and no key of another boot opens.
fn release(_pending: &PendingUnlock, view: &UnlockView<'_>) -> SiblingFrame {
    view.release.map_or(
        SiblingFrame::Refused(RefusalReason::NotAKeyHolder),
        |release| SiblingFrame::Released(release.clone()),
    )
}
