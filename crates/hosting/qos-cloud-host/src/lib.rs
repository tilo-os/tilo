//! Persistent certified application state, independent of compositor initialization.
use anyhow::{Context, Result, anyhow, bail};
use automerge::transaction::Transactable as _;
use ed25519_dalek::SigningKey;
use qos_peer_state::{
    NeighbourMsg, SeedDocument, SeedState, StatePeer, TransportOrigin,
    transport::{Retention, retention},
};
use qos_user_identity::{
    DeviceCert, RevocationList, WorkspaceDirectory,
    cloud_enrollment::{Challenge, EnrollmentRequest, verify_request},
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};
pub mod action_server;
pub mod actions;
mod agent_documents;
pub mod agent_owner;
mod agent_runtime;
pub use agent_runtime::CloudAgentCommands;
mod agent_runtime_adapter;
mod agent_runtime_config;
pub mod agent_runtime_owner;
mod agent_runtime_reads;
mod agent_sender;
pub(crate) mod agent_workers;
pub mod broker;
pub mod collaboration;
mod delivery;
pub mod gateway;
mod https_owner;
pub mod mesh;
pub mod model;
pub mod operator;
pub mod runtime;
mod storage;
pub use actions::{ActionReceipt, ActionRequest, NativeOperation};
#[cfg(test)]
mod agent_owner_tests;
#[cfg(all(test, unix))]
mod durability_tests;
#[cfg(test)]
mod mesh_tests;
#[cfg(test)]
mod tests;
const STATE_VERSION: u32 = 4;
const AGENT_STATE_VERSION: u32 = 5;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum PublicationKind {
    Checkpoint,
    Delta,
    Presence,
}
#[derive(Clone, Serialize, Deserialize)]
struct StoredPublication {
    message: NeighbourMsg,
    kind: PublicationKind,
}
impl StoredPublication {
    fn new(message: NeighbourMsg, shell_checkpoint: bool) -> Result<Self> {
        let kind = match retention(&message, shell_checkpoint) {
            Some((_, Retention::Checkpoint)) => PublicationKind::Checkpoint,
            Some((_, Retention::Delta)) => PublicationKind::Delta,
            None => PublicationKind::Presence,
        };
        Ok(Self { message, kind })
    }
    fn baseline(message: NeighbourMsg) -> Result<Self> {
        Self::new(message, true)
    }
    fn mutation(message: NeighbourMsg) -> Result<Self> {
        if retention(&message, false).is_none() {
            bail!("live mutation has no retained channel")
        }
        Ok(Self {
            message,
            kind: PublicationKind::Delta,
        })
    }
    fn validate(&self) -> Result<()> {
        let actual = match self.kind {
            PublicationKind::Checkpoint => retention(&self.message, true)
                .map(|(_, kind)| kind == Retention::Checkpoint)
                .unwrap_or(false),
            // A covered application snapshot is also sent as a live retained
            // delta so already-current peers install the exact checkpoint bound
            // by the new room receipt. The same bytes remain a checkpoint in
            // baseline batches for cold recovery.
            PublicationKind::Delta => retention(&self.message, false).is_some(),
            PublicationKind::Presence => retention(&self.message, true).is_none(),
        };
        if !actual {
            bail!("invalid durable publication kind")
        }
        Ok(())
    }
}
fn baseline_publications(messages: Vec<NeighbourMsg>) -> Result<Vec<StoredPublication>> {
    messages
        .into_iter()
        .map(StoredPublication::baseline)
        .collect()
}
fn mutation_publications(messages: Vec<NeighbourMsg>) -> Result<Vec<StoredPublication>> {
    messages
        .into_iter()
        .map(StoredPublication::mutation)
        .collect()
}
fn live_projection_from_bootstrap(
    before: &StatePeer,
    prepared: &StatePeer,
    bootstrap: &[NeighbourMsg],
) -> Result<Vec<StoredPublication>> {
    let Some(NeighbourMsg::SyncResp { .. }) = bootstrap.first() else {
        bail!("durable bootstrap is missing room governance")
    };
    let Some(room) = live_room_publication(before, prepared)? else {
        bail!("durable bootstrap did not advance room governance")
    };
    let mut publications = vec![room];
    publications.extend(
        bootstrap
            .iter()
            .skip(1)
            .cloned()
            .map(StoredPublication::mutation)
            .collect::<Result<Vec<_>>>()?,
    );
    Ok(publications)
}
fn live_room_publication(
    before: &StatePeer,
    prepared: &StatePeer,
) -> Result<Option<StoredPublication>> {
    let before_room = before
        .admitted_room()
        .ok_or_else(|| anyhow!("missing prior room governance"))?
        .clone();
    let mut room = prepared
        .admitted_room()
        .ok_or_else(|| anyhow!("missing prepared room governance"))?
        .clone();
    let changes = room
        .get_changes(&before_room.base_heads())
        .into_iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect::<Vec<_>>();
    if changes.is_empty() {
        return Ok(None);
    }
    Ok(Some(StoredPublication::mutation(NeighbourMsg::SyncResp {
        changes,
        room: prepared.room().to_owned(),
    })?))
}
fn durable_mutation_publications(
    peer: &mut StatePeer,
    before: &StatePeer,
    key: &SigningKey,
    now: web_time::SystemTime,
    generation: u64,
) -> Result<Vec<StoredPublication>> {
    qos_session_peer::media_governance::prepare_media_governance(peer, key, now)?;
    mutation_publications(
        peer.durable_mutation_messages(before, key, now, generation)
            .map_err(anyhow::Error::msg)?,
    )
}
fn durable_bootstrap_messages(
    peer: &mut StatePeer,
    key: &SigningKey,
    now: web_time::SystemTime,
    generation: u64,
) -> Result<Vec<NeighbourMsg>> {
    qos_session_peer::media_governance::prepare_media_governance(peer, key, now)?;
    peer.durable_bootstrap_messages(key, now, generation)
        .map_err(anyhow::Error::msg)
}
#[derive(Clone, Serialize, Deserialize)]
struct Bound {
    root: [u8; 32],
    authority: DeviceCert,
    roster: Vec<DeviceCert>,
    revocations: Vec<qos_user_identity::RevocationEntry>,
    peer: Vec<u8>,
    directory: Vec<u8>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delivery: Option<delivery::DeliveryState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    private_agent_ledger: Option<qos_peer_state::owner_commit::PrivateAgentState>,
    machine_id: String,
    room: String,
    host_secret: [u8; 32],
    challenge: Option<Challenge>,
    bound: Option<Bound>,
    consumed: BTreeMap<String, (String, serde_json::Value)>,
    outbox: Vec<StoredPublication>,
    receipts: BTreeMap<String, (String, ActionReceipt)>,
    pending_models: BTreeMap<String, model::PendingModel>,
    #[serde(default)]
    mesh: mesh::MeshState,
    #[serde(default)]
    join_challenges: BTreeMap<String, collaboration::StoredJoinChallenge>,
    #[serde(default)]
    join_responses: BTreeMap<String, collaboration::StoredJoinResponse>,
    #[serde(default)]
    membership_challenges: BTreeMap<String, collaboration::StoredMembershipChallenge>,
    #[serde(default)]
    membership_responses: BTreeMap<String, collaboration::StoredJoinResponse>,
    #[serde(default)]
    invitations: BTreeMap<String, collaboration::StoredInvitation>,
    #[serde(default)]
    canonical_intents: BTreeMap<String, broker::StoredCanonicalIntent>,
    #[serde(default = "initial_generation")]
    generation: u64,
    // Transient per-commit receipt instruction; consumed by prepare_commit.
    #[serde(skip)]
    delivery_binding: Option<delivery::admission::DeliveryBinding>,
}

const fn initial_generation() -> u64 {
    1
}
/// Which part of a failed save is known to have reached the namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreSaveOutcome {
    NotPublished,
    PublishedDurabilityUnknown,
    PublicationUnknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoreCommitFailure {
    pub generation: u64,
    pub outcome: StoreSaveOutcome,
}
impl std::fmt::Display for StoreCommitFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cloud save failed at generation {} ({:?}); drop and reopen Store",
            self.generation, self.outcome
        )
    }
}
impl std::error::Error for StoreCommitFailure {}
/// Replacement may be visible but its durability is not established.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UncertainStoreCommit {
    pub generation: u64,
}
impl std::fmt::Display for UncertainStoreCommit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cloud commit uncertain at generation {}; reconcile Store before retry",
            self.generation
        )
    }
}
impl std::error::Error for UncertainStoreCommit {}
#[derive(Clone, Copy, Debug)]
pub struct StoreUnavailable;
impl std::fmt::Display for StoreUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cloud owner storage unavailable")
    }
}
impl std::error::Error for StoreUnavailable {}
pub(crate) fn is_store_failure(error: &anyhow::Error) -> bool {
    error.is::<StoreUnavailable>()
        || error.is::<StoreCommitFailure>()
        || error.is::<UncertainStoreCommit>()
}
fn commit_error(error: anyhow::Error, failure: StoreCommitFailure) -> anyhow::Error {
    let error = error.context(failure);
    if failure.outcome == StoreSaveOutcome::NotPublished {
        error
    } else {
        error.context(UncertainStoreCommit {
            generation: failure.generation,
        })
    }
}
/// Input rejection is scoped to a message; an owner failure stops delivery.
#[derive(Debug)]
pub enum ReceiveError {
    Rejected(anyhow::Error),
    Owner(anyhow::Error),
}
impl std::fmt::Display for ReceiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(error) | Self::Owner(error) => std::fmt::Display::fmt(error, f),
        }
    }
}
impl std::error::Error for ReceiveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(match self {
            Self::Rejected(error) | Self::Owner(error) => error.as_ref(),
        })
    }
}
#[derive(Debug)]
struct RejectedMessage;
impl std::fmt::Display for RejectedMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("state message rejected")
    }
}
impl std::error::Error for RejectedMessage {}
/// One exclusive transaction. Drop releases the OS lock even after process failure.
pub struct Store {
    // Process-local time custody only. Never serialized or reconstructed from State.
    policy_time: Option<qos_action_broker::peer_actions::PolicyTime>,
    root: PathBuf,
    directory: File,
    state: State,
    _lock: File,
    failed_commit: std::cell::Cell<Option<StoreCommitFailure>>,
    /// Last persisted aggregate bytes and files under this lock hold: the
    /// diff base of a journal append. `None` checkpoints the next commit.
    tip: std::cell::RefCell<Option<storage::Tip>>,
}
#[derive(Clone, Copy, Eq, PartialEq)]
enum StoreOpenMode {
    Provisioning,
    ExistingDelivery,
}
fn revocations(entries: &[qos_user_identity::RevocationEntry]) -> Result<RevocationList> {
    let mut list = RevocationList::new();
    for entry in entries {
        list.insert(entry.clone())?;
    }
    Ok(list)
}
fn active_roster(
    roster: &[DeviceCert],
    root: [u8; 32],
    revoked: &RevocationList,
    now: u64,
) -> Result<Vec<DeviceCert>> {
    let mut active = vec![];
    for certificate in roster {
        // Preserve historical certificates, but malformed signatures still indicate corruption.
        qos_user_identity::verify_device_cert(certificate)?;
        if certificate.user_public_key != root {
            bail!("roster identity root mismatch")
        }
        if !certificate.is_expired_at(now) && !revoked.is_revoked(&certificate.device_public_key) {
            active.push(certificate.clone());
        }
    }
    Ok(active)
}
fn now_time(ns: u64) -> web_time::SystemTime {
    web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(ns)
}
pub(crate) fn audit(event: &str, fields: serde_json::Value) {
    #[cfg(test)]
    TEST_AUDIT_EVENTS.with(|events| events.borrow_mut().push((event.into(), fields.clone())));
    if tracing::dispatcher::has_been_set() {
        tracing::info!(component = "qos-cloud-host", event, fields = %fields);
    } else {
        eprintln!(
            "{}",
            serde_json::json!({"component":"qos-cloud-host","event":event,"fields":fields})
        );
    }
}
#[cfg(test)]
thread_local! {
    static TEST_AUDIT_EVENTS: std::cell::RefCell<Vec<(String, serde_json::Value)>> = const { std::cell::RefCell::new(Vec::new()) };
}
#[cfg(test)]
pub(crate) fn take_test_audit_events() -> Vec<(String, serde_json::Value)> {
    TEST_AUDIT_EVENTS.with(|events| std::mem::take(&mut *events.borrow_mut()))
}
impl Store {
    pub fn open(root: &Path, machine: &str) -> Result<Self> {
        Self::open_inner(root, machine).context(StoreUnavailable)
    }
    fn open_inner(root: &Path, machine: &str) -> Result<Self> {
        if machine.is_empty()
            || machine.len() > 128
            || !machine
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        {
            bail!("invalid provisioned machine identity")
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(root)?;
            // Preserve the existing host's root privacy repair, then commit the
            // whole chain; directory creation alone is not a durability barrier.
            std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
            atomic_file::provision_directory(root, atomic_file::Privacy::OwnerOnly)?;
        }
        #[cfg(not(unix))]
        bail!("cloud Store durability requires qualified Unix storage");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("state.lock"))?;
        lock.lock()?;
        let directory = File::open(root)?;
        let storage_directory = atomic_file::StorageDirectory::from_file(directory.try_clone()?)?;
        let persisted = storage::read_reconciled(&storage_directory, root)?;
        Self::load_locked(
            root,
            machine,
            directory,
            lock,
            persisted,
            StoreOpenMode::Provisioning,
        )
    }
    fn load_locked(
        root: &Path,
        machine: &str,
        directory: File,
        lock: File,
        persisted: Option<storage::Persisted>,
        mode: StoreOpenMode,
    ) -> Result<Self> {
        let existed = persisted.is_some();
        let mut migrated = false;
        let mut tip = None;
        let state = if let Some(persisted) = persisted {
            let mut value = storage::decode_unique_json(&persisted.bytes)
                .context("corrupt cloud state; refusing reinitialization")?;
            if mode == StoreOpenMode::ExistingDelivery
                && value.get("version").and_then(|v| v.as_u64())
                    != Some(u64::from(delivery::VERSION))
            {
                bail!("existing delivery Store selection required")
            }
            if value.get("version").and_then(|v| v.as_u64()) == Some(1) {
                if value.get("pending_models").is_some() {
                    bail!("invalid v1 pending state")
                }
                value["version"] = 2.into();
                value["pending_models"] = serde_json::json!({});
                migrated = true;
            }
            if value.get("version").and_then(|v| v.as_u64()) == Some(2) {
                value["version"] = 3.into();
                let fields = value
                    .as_object_mut()
                    .ok_or_else(|| anyhow!("cloud state must be an object"))?;
                for name in [
                    "join_challenges",
                    "join_responses",
                    "membership_challenges",
                    "membership_responses",
                    "invitations",
                    "canonical_intents",
                ] {
                    fields
                        .entry(name.to_owned())
                        .or_insert_with(|| serde_json::json!({}));
                }
                fields
                    .entry("generation".to_owned())
                    .or_insert_with(|| initial_generation().into());
                migrated = true;
            }
            if value.get("version").and_then(|v| v.as_u64()) == Some(3) {
                let legacy = value
                    .get_mut("outbox")
                    .ok_or_else(|| anyhow!("legacy publication outbox missing"))?
                    .take();
                let messages = serde_json::from_value::<Vec<NeighbourMsg>>(legacy)
                    .context("invalid legacy publication outbox")?;
                value["outbox"] = serde_json::to_value(baseline_publications(messages)?)?;
                value["version"] = STATE_VERSION.into();
                migrated = true;
            }
            // Exact private shape, including nested legacy structs whose public
            // JSON decoder may otherwise ignore fields. V4 has no private field,
            // even an explicit null; V5 always carries the selected ledger.
            let private_input = value.get("private_agent_ledger").cloned();
            let has_delivery = value.get("delivery").is_some();
            if has_delivery
                != (value.get("version").and_then(|v| v.as_u64())
                    == Some(u64::from(delivery::VERSION)))
            {
                bail!("delivery field requires exact selected outer version")
            }
            let decoded = serde_json::from_value::<State>(value)
                .context("corrupt cloud state; refusing reinitialization")?;
            let private_decoded = decoded
                .private_agent_ledger
                .as_ref()
                .map(serde_json::to_value)
                .transpose()?;
            if private_input != private_decoded {
                bail!("private agent state has a noncanonical or unknown shape")
            }
            tip = persisted.into_tip();
            decoded
        } else {
            if mode == StoreOpenMode::ExistingDelivery {
                bail!("existing delivery Store is missing")
            }
            let mut secret = [0; 32];
            rand::rngs::OsRng.fill_bytes(&mut secret);
            State {
                version: STATE_VERSION,
                delivery: None,
                private_agent_ledger: None,
                machine_id: machine.into(),
                room: uuid::Uuid::new_v4().to_string(),
                host_secret: secret,
                challenge: None,
                bound: None,
                consumed: BTreeMap::new(),
                outbox: vec![],
                receipts: BTreeMap::new(),
                pending_models: BTreeMap::new(),
                mesh: mesh::MeshState::default(),
                join_challenges: BTreeMap::new(),
                join_responses: BTreeMap::new(),
                membership_challenges: BTreeMap::new(),
                membership_responses: BTreeMap::new(),
                invitations: BTreeMap::new(),
                canonical_intents: BTreeMap::new(),
                generation: initial_generation(),
                delivery_binding: None,
            }
        };
        if !matches!(
            state.version,
            STATE_VERSION | AGENT_STATE_VERSION | delivery::VERSION
        ) || state.machine_id != machine
        {
            bail!("persisted machine namespace mismatch")
        }
        match (state.private_schema_version()?, &state.private_agent_ledger) {
            (STATE_VERSION, None) => (),
            (AGENT_STATE_VERSION, Some(ledger)) => {
                ledger
                    .validate(&state.room, state.generation)
                    .map_err(anyhow::Error::msg)?;
                if let qos_peer_state::owner_commit::PrivateAgentState::V2(ledger) = ledger {
                    let bound = state
                        .bound
                        .as_ref()
                        .ok_or_else(|| anyhow!("private v2 requires enrolled owner"))?;
                    for receipt in ledger.facts.values() {
                        if let qos_peer_state::owner_commit::AgentRuntimeFact::Admitted {
                            intent,
                            ..
                        } = &receipt.change.fact
                        {
                            if intent.scope.machine != state.machine_id
                                || intent.scope.host != state.room
                                || intent.scope.caller.user_public_key != bound.root
                                || intent.route.app_peer
                                    != qos_relay::room_workspace_id(&state.room)?.to_string()
                            {
                                bail!("private runtime admission differs from retained Store owner")
                            }
                        }
                    }
                }
            }
            _ => bail!("private agent schema/version mismatch; preserve original state"),
        }
        for publication in &state.outbox {
            publication.validate()?;
        }
        delivery::validate(&state, qos_relay::clock::now_ns())?;
        let mut store = Self {
            policy_time: None,
            root: root.into(),
            directory,
            state,
            _lock: lock,
            failed_commit: std::cell::Cell::new(None),
            tip: std::cell::RefCell::new(tip),
        };
        let mut desktop_migrated = false;
        if let Some(bound) = &store.state.bound {
            if store.key().verifying_key().to_bytes() != bound.authority.device_public_key
                || bound.root != bound.authority.user_public_key
            {
                bail!("persisted host identity mismatch")
            };
            if mode == StoreOpenMode::ExistingDelivery {
                return Ok(store);
            }
            let now = qos_relay::clock::now_ns();
            let (before, mut peer) = store.peer_transition_at(now)?;
            if bootstrap_desktop(&mut peer, &store.key())? {
                let mut state = store.state.clone();
                let messages = durable_bootstrap_messages(
                    &mut peer,
                    &store.key(),
                    now_time(now),
                    store.next_generation()?,
                )?;
                state
                    .outbox
                    .extend(live_projection_from_bootstrap(&before, &peer, &messages)?);
                state.outbox.extend(baseline_publications(messages)?);
                state
                    .bound
                    .as_mut()
                    .ok_or_else(|| anyhow!("workspace not enrolled"))?
                    .peer = peer
                    .durable()
                    .map_err(|error| anyhow::Error::msg(error).context(crate::StoreUnavailable))?;
                store.commit(state)?;
                desktop_migrated = true;
            }
        }
        if !existed || (migrated && !desktop_migrated) {
            store.save(&store.state)?;
        }
        Ok(store)
    }
    fn action_key(&self) -> iroh::SecretKey {
        iroh::SecretKey::from_bytes(&blake3::derive_key(
            "tilo cloud actions endpoint v1",
            &self.state.host_secret,
        ))
    }
    fn key(&self) -> SigningKey {
        SigningKey::from_bytes(&self.state.host_secret)
    }
    fn ensure_reconciled(&self) -> Result<()> {
        if let Some(failure) = self.failed_commit.get() {
            return Err(commit_error(
                anyhow!("failed cloud transaction requires reopen"),
                failure,
            ));
        }
        Ok(())
    }
    fn save(&self, state: &State) -> Result<()> {
        self.ensure_reconciled()?;
        self.policy_now_ns()?;
        let bytes = serde_json::to_vec(state).map_err(|error| {
            let failure = StoreCommitFailure {
                generation: state.generation,
                outcome: StoreSaveOutcome::NotPublished,
            };
            self.failed_commit.set(Some(failure));
            commit_error(anyhow::Error::new(error).context(StoreUnavailable), failure)
        })?;
        delivery::early_forward::fire();
        storage::transaction::publish_bytes_with_tip(
            &self.directory,
            &self.root,
            &self.failed_commit,
            self.policy_time.as_ref(),
            state.generation,
            &bytes,
            &mut self.tip.borrow_mut(),
        )
        .map_err(storage::transaction::PublishFailure::into_error)
    }
    fn commit(&mut self, mut state: State) -> Result<()> {
        self.ensure_reconciled()?;
        state.generation = self.next_generation()?;
        delivery::prepare_commit(&self.state, &mut state, self.policy_now_ns()?)?;
        self.publish_prepared(state)
    }
    fn publish_prepared(&mut self, state: State) -> Result<()> {
        self.ensure_reconciled()?;
        anyhow::ensure!(
            state.generation == self.next_generation()?,
            "prepared Store generation mismatch"
        );
        self.save(&state)?;
        self.state = state;
        Ok(())
    }
    fn next_generation(&self) -> Result<u64> {
        self.ensure_reconciled()?;
        self.state.generation.checked_add(1).ok_or_else(|| {
            let failure = StoreCommitFailure {
                generation: self.state.generation,
                outcome: StoreSaveOutcome::NotPublished,
            };
            self.failed_commit.set(Some(failure));
            commit_error(anyhow!("cloud state generation overflow"), failure)
        })
    }
    pub fn inspect(&mut self, now: u64) -> Result<Challenge> {
        self.ensure_reconciled()?;
        if let Some(c) = &self.state.challenge {
            if c.expires_at_ns > now {
                return Ok(c.clone());
            }
        }
        let mut nonce = [0; 32];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let c = Challenge {
            version: 1,
            machine_id: self.state.machine_id.clone(),
            room: self.state.room.clone(),
            workspace_id: qos_relay::room_workspace_id(&self.state.room)?.to_string(),
            host_public_key: self.key().verifying_key().to_bytes(),
            nonce,
            expires_at_ns: now
                .checked_add(300_000_000_000)
                .ok_or_else(|| anyhow!("clock overflow"))?,
            root_public_key: self.state.bound.as_ref().map(|b| b.root),
            host_certificate: self.state.bound.as_ref().map(|b| b.authority.clone()),
        };
        let mut state = self.state.clone();
        state.challenge = Some(c.clone());
        self.commit(state)?;
        Ok(c)
    }
    pub(crate) fn bind_policy_time(
        &mut self,
        time: qos_action_broker::peer_actions::PolicyTime,
    ) -> Result<()> {
        // Store the context before validation; failed Some can never become legacy None.
        if let Some(original) = &self.policy_time {
            original.now_ns()?;
            anyhow::ensure!(
                original.same_origin(&time),
                "Store policy origin cannot be replaced"
            );
        }
        self.policy_time = Some(time);
        self.policy_time.as_ref().expect("just bound").now_ns()?;
        Ok(())
    }
    pub(crate) fn policy_now_ns(&self) -> Result<u64> {
        match &self.policy_time {
            Some(time) => Ok(time.now_ns()?),
            None => Ok(qos_relay::clock::now_ns()), // Explicit ordinary/CLI legacy entrypoint.
        }
    }
    pub(crate) fn policy_now_or(&self, legacy: u64) -> Result<u64> {
        match &self.policy_time {
            Some(time) => Ok(time.now_ns()?),
            None => Ok(legacy),
        }
    }
    pub(crate) fn policy_now(&self) -> Result<web_time::SystemTime> {
        Ok(now_time(self.policy_now_ns()?))
    }
    pub fn peer(&self) -> Result<StatePeer> {
        self.peer_at(self.policy_now_ns()?)
    }
    fn peer_at(&self, now: u64) -> Result<StatePeer> {
        self.peer_transition_at(now).map(|(_, peer)| peer)
    }
    fn peer_transition_at(&self, now: u64) -> Result<(StatePeer, StatePeer)> {
        self.ensure_reconciled()?;
        let now = self.policy_now_or(now)?;
        let b = self
            .state
            .bound
            .as_ref()
            .ok_or_else(|| anyhow!("workspace not enrolled"))?;
        let before = StatePeer::restore(
            &b.peer,
            &self.state.room,
            b.authority.clone(),
            active_roster(&b.roster, b.root, &revocations(&b.revocations)?, now)?,
            revocations(&b.revocations)?,
            now_time(now),
        )
        .map_err(anyhow::Error::msg)
        .context(StoreUnavailable)?;
        let mut peer = before.clone();
        // A new browser receives only its own certificate and the pinned host.
        // Keep expired identities in durable certificate/history evidence, but
        // retire their current enrollment metadata before publishing governance.
        // The shared receiver still rejects unknown expired certificates.
        use automerge::{ROOT, ReadDoc, transaction::Transactable};
        let expired = peer
            .certificates()
            .into_iter()
            .filter(|certificate| certificate.is_expired_at(now))
            .collect::<Vec<_>>();
        if !expired.is_empty() {
            let mut room = peer
                .admitted_room()
                .cloned()
                .ok_or_else(|| anyhow!("room governance missing"))?;
            let identities = room
                .get(ROOT, "peer_identities_v1")?
                .ok_or_else(|| anyhow!("identities missing"))?
                .1;
            room.set_actor(automerge::ActorId::from(
                self.key().verifying_key().to_bytes().to_vec(),
            ));
            let mut changed = false;
            for certificate in expired {
                let id = hex::encode(certificate.device_public_key);
                if room.get(&identities, &id)?.is_some() {
                    room.delete(&identities, &id)?;
                    changed = true;
                }
            }
            if changed {
                room.commit();
                let message = NeighbourMsg::SyncResp {
                    room: self.state.room.clone(),
                    changes: room
                        .get_changes(&[])
                        .iter()
                        .map(|change| change.raw_bytes().to_vec())
                        .collect(),
                };
                // Use the ordinary pinned-governor admission path: do not edit
                // StatePeer internals or bypass immutable receipts/topology.
                peer.receive_from(
                    TransportOrigin::RelayDelta(self.key().verifying_key().to_bytes()),
                    message,
                    now_time(now),
                )
                .map_err(anyhow::Error::msg)
                .context(StoreUnavailable)?;
            }
        }
        Ok((before, peer))
    }
    pub fn apply(&mut self, r: &EnrollmentRequest, now: u64) -> Result<serde_json::Value> {
        self.ensure_reconciled()?;
        let nonce = hex::encode(r.challenge.nonce);
        let hash = blake3::hash(&serde_json::to_vec(r)?).to_hex().to_string();
        if let Some((old, result)) = self.state.consumed.get(&nonce) {
            if old == &hash {
                return Ok(result.clone());
            }
            bail!("consumed enrollment nonce substitution")
        }
        let expected = self
            .state
            .challenge
            .as_ref()
            .ok_or_else(|| anyhow!("no enrollment challenge issued"))?;
        verify_request(r, expected, now).map_err(anyhow::Error::msg)?;
        if r.challenge.machine_id != self.state.machine_id
            || r.challenge.room != self.state.room
            || r.challenge.host_public_key != self.key().verifying_key().to_bytes()
        {
            bail!("machine challenge binding mismatch")
        }
        let mut state = self.state.clone();
        let key = self.key();
        let workspace = qos_relay::room_workspace_id(&state.room)?;
        let (mut peer, mut directory, mut roster, before) = if let Some(bound) = &state.bound {
            if bound.root != r.browser_certificate.user_public_key
                || bound.authority != r.host_certificate
            {
                bail!("workspace root or authority replacement forbidden")
            }
            let (before, peer) = self.peer_transition_at(now)?;
            (
                peer,
                WorkspaceDirectory::load(
                    bound.root,
                    bound.authority.device_public_key,
                    &bound.directory,
                )?,
                bound.roster.clone(),
                Some(before),
            )
        } else {
            let roster = vec![r.host_certificate.clone()];
            let mut peer = StatePeer::new(
                &state.room,
                r.host_certificate.clone(),
                roster.clone(),
                RevocationList::new(),
                now_time(now),
            )
            .map_err(anyhow::Error::msg)?;
            peer.install_seed(
                initial_state(&state.room, &r.host_certificate, &key)?,
                &key,
                now_time(now),
            )
            .map_err(anyhow::Error::msg)?;
            let mut directory = WorkspaceDirectory::new(
                r.host_certificate.user_public_key,
                r.host_certificate.device_public_key,
            );
            directory.add_workspace(workspace, "My Tilo workspace")?;
            (peer, directory, roster, None)
        };
        peer.admit_certificate(r.browser_certificate.clone(), &key, now_time(now))
            .map_err(anyhow::Error::msg)?;
        roster.retain(|c| c.device_public_key != r.browser_certificate.device_public_key);
        roster.push(r.browser_certificate.clone());
        let active = active_roster(
            &roster,
            r.host_certificate.user_public_key,
            &revocations(
                &state
                    .bound
                    .as_ref()
                    .map(|b| b.revocations.clone())
                    .unwrap_or_default(),
            )?,
            now,
        )?;
        directory.set_member_devices(
            workspace,
            &active
                .iter()
                .map(|c| c.device_public_key)
                .collect::<Vec<_>>(),
        )?;
        directory.set_host_devices(workspace, &[r.host_certificate.device_public_key])?;
        // Emit a full signed directory snapshot for every new device, not a tail requiring prior state.
        let directory_bytes = directory.save();
        // A signed view for this newly enrolled device, independent of fleet history.
        // Keep the complete directory above as the host's canonical durable state.
        let mut view = WorkspaceDirectory::new(
            r.host_certificate.user_public_key,
            r.host_certificate.device_public_key,
        );
        view.add_workspace(workspace, "My Tilo workspace")?;
        view.set_member_devices(
            workspace,
            &[
                r.host_certificate.device_public_key,
                r.browser_certificate.device_public_key,
            ],
        )?;
        view.set_host_devices(workspace, &[r.host_certificate.device_public_key])?;
        let directory_update = view.sync_incremental(&key, now)?;
        let all_revocations = state
            .bound
            .as_ref()
            .map(|b| b.revocations.clone())
            .unwrap_or_default();
        peer.publish_governed_revocations(&all_revocations, &key, now_time(now))
            .map_err(anyhow::Error::msg)?;
        let bootstrap_revocations = all_revocations
            .into_iter()
            .filter(|entry| {
                entry.revoked_device_public_key == r.host_certificate.device_public_key
                    || entry.revoked_device_public_key == r.browser_certificate.device_public_key
            })
            .collect::<Vec<_>>();
        let packet = std::env::var("QOS_CLOUD_PACKET_RELAY")
            .or_else(|e| {
                if cfg!(test) {
                    Ok("https://example.invalid".into())
                } else {
                    Err(e)
                }
            })
            .context("QOS_CLOUD_PACKET_RELAY required")?;
        let packet_url: iroh_base::RelayUrl = packet.parse()?;
        let address = iroh_base::EndpointAddr::new(iroh_base::PublicKey::from_bytes(
            &qos_relay::daemon::relay_service_node_id(&key),
        )?)
        .with_relay_url(packet_url.clone());
        let action_address = iroh_base::EndpointAddr::new(self.action_key().public())
            .with_relay_url(packet_url.clone());
        let result = serde_json::json!({"version":1,"action_address":action_address,"certificate":r.browser_certificate,"root_public_key":r.browser_certificate.user_public_key,"host_certificate":r.host_certificate,"relay_address":address,"packet_relay":packet_url,"revocations":bootstrap_revocations,"bootstrap":{"room":state.room,"workspace_id":workspace.to_string(),"directory_update":directory_update,"directory_author":r.host_certificate,"certified_roster":[r.host_certificate.clone(),r.browser_certificate.clone()]}});
        let messages =
            durable_bootstrap_messages(&mut peer, &key, now_time(now), self.next_generation()?)?;
        if let Some(before) = before {
            state
                .outbox
                .extend(live_projection_from_bootstrap(&before, &peer, &messages)?);
        }
        state.outbox.extend(baseline_publications(messages)?);
        state.bound = Some(Bound {
            root: r.host_certificate.user_public_key,
            authority: r.host_certificate.clone(),
            roster,
            revocations: state
                .bound
                .as_ref()
                .map(|b| b.revocations.clone())
                .unwrap_or_default(),
            peer: peer
                .durable()
                .map_err(|error| anyhow::Error::msg(error).context(crate::StoreUnavailable))?,
            directory: directory_bytes,
        });
        state.challenge = None;
        state.consumed.insert(nonce, (hash, result.clone()));
        self.commit(state)?;
        Ok(result)
    }
    pub fn receive(
        &mut self,
        origin: TransportOrigin,
        message: NeighbourMsg,
        now: u64,
    ) -> std::result::Result<(), ReceiveError> {
        self.receive_inner(origin, message, now).map_err(|error| {
            if error.is::<RejectedMessage>() {
                ReceiveError::Rejected(error)
            } else {
                ReceiveError::Owner(error.context(StoreUnavailable))
            }
        })
    }
    fn receive_inner(
        &mut self,
        origin: TransportOrigin,
        message: NeighbourMsg,
        now: u64,
    ) -> Result<()> {
        self.ensure_reconciled()?;
        let (before, mut peer) = self.peer_transition_at(now)?;
        let cleanup_changed = before.admitted_room().map(|room| room.clone().get_heads())
            != peer.admitted_room().map(|room| room.clone().get_heads());
        let document_audit = match (&origin, &message) {
            (
                TransportOrigin::RelayDelta(source) | TransportOrigin::RetainedRelayDelta(source),
                NeighbourMsg::AppDocument {
                    app_id,
                    record_id,
                    envelope,
                    ..
                },
            ) if envelope.author.as_bytes() == source => {
                let before_heads = peer.document_heads(app_id, record_id).unwrap_or_default();
                let before_sequence = peer
                    .replay_floors()
                    .into_iter()
                    .find(|(app, record, author, _)| {
                        app == app_id && record == record_id && author == source
                    })
                    .map_or(0, |(_, _, _, sequence)| sequence);
                let signer_root = peer
                    .certificates()
                    .into_iter()
                    .find(|certificate| certificate.device_public_key == *source)
                    .map(|certificate| certificate.user_public_key);
                Some((
                    app_id.clone(),
                    record_id.clone(),
                    *source,
                    signer_root,
                    envelope.seq,
                    before_sequence,
                    before_heads,
                ))
            }
            _ => None,
        };
        let request = matches!(&message,NeighbourMsg::SyncReq{room,..} if room==&self.state.room);
        let revocation_updates = match &message {
            NeighbourMsg::IdentityHello { revocations, .. } => revocations.clone(),
            _ => vec![],
        };
        let outcome = peer
            .receive_from(origin, message, now_time(now))
            .map_err(anyhow::Error::msg)
            .context(RejectedMessage)?;
        if outcome.changed
            || outcome.metadata_changed
            || outcome.deferred
            || request
            || !revocation_updates.is_empty()
        {
            let mut state = self.state.clone();
            // Persist a root-approved host revocation even though that host may
            // no longer author governance. Subsequent opens/authentication fail closed.
            let authority_revoked = revocation_updates
                .iter()
                .any(|entry| entry.revoked_device_public_key == peer.authority().device_public_key);
            if authority_revoked && self.state.version == delivery::VERSION {
                let proof = before
                    .verify_security_metadata_only(&peer)
                    .map_err(anyhow::Error::msg)?;
                return self.commit_verified_security_metadata_only(peer, proof, now);
            }
            if !authority_revoked && !revocation_updates.is_empty() {
                peer.publish_governed_revocations(&revocation_updates, &self.key(), now_time(now))
                    .map_err(anyhow::Error::msg)?;
            }
            if !authority_revoked
                && (cleanup_changed || outcome.changed || request || !revocation_updates.is_empty())
            {
                let generation = self.next_generation()?;
                if peer.incomplete_reason().is_some() {
                    qos_session_peer::media_governance::prepare_media_governance(
                        &mut peer,
                        &self.key(),
                        now_time(now),
                    )?;
                    if let Some(publication) = live_room_publication(&before, &peer)? {
                        state.outbox.push(publication);
                    }
                } else if request {
                    let messages = durable_bootstrap_messages(
                        &mut peer,
                        &self.key(),
                        now_time(now),
                        generation,
                    )?;
                    state
                        .outbox
                        .extend(live_projection_from_bootstrap(&before, &peer, &messages)?);
                    state.outbox.extend(baseline_publications(messages)?);
                } else {
                    state.outbox.extend(durable_mutation_publications(
                        &mut peer,
                        &before,
                        &self.key(),
                        now_time(now),
                        generation,
                    )?);
                }
            }
            let bound = state
                .bound
                .as_mut()
                .ok_or_else(|| anyhow!("not enrolled"))?;
            let mut list = revocations(&bound.revocations)?;
            for update in revocation_updates {
                list.insert(update)?;
            }
            bound.revocations = list.iter().cloned().collect();
            bound
                .roster
                .retain(|c| !list.is_revoked(&c.device_public_key));
            bound.peer = peer
                .durable()
                .map_err(|error| anyhow::Error::msg(error).context(crate::StoreUnavailable))?;
            self.commit(state)?;
            if let Some((
                app,
                record,
                signer_device,
                Some(signer_root),
                submitted_sequence,
                before_sequence,
                before_heads,
            )) = document_audit
            {
                let after_heads = peer.document_heads(&app, &record).unwrap_or_default();
                let after_sequence = peer
                    .replay_floors()
                    .into_iter()
                    .find(|(candidate_app, candidate_record, author, _)| {
                        candidate_app == &app
                            && candidate_record == &record
                            && author == &signer_device
                    })
                    .map_or(0, |(_, _, _, sequence)| sequence);
                if outcome.changed
                    && !outcome.deferred
                    && before_heads != after_heads
                    && before_sequence < submitted_sequence
                    && after_sequence >= submitted_sequence
                {
                    audit(
                        "collaboration_document_committed",
                        serde_json::json!({
                            "room": self.state.room,
                            "app": app,
                            "record": record,
                            "attribution": "accepted_transport_signer",
                            "signer_device": hex::encode(signer_device),
                            "signer_root": hex::encode(signer_root),
                            "submitted_sequence": submitted_sequence.to_string(),
                            "before_heads": before_heads.iter().map(ToString::to_string).collect::<Vec<_>>(),
                            "after_heads": after_heads.iter().map(ToString::to_string).collect::<Vec<_>>(),
                            "generation": self.state.generation.to_string(),
                            "result": "committed",
                        }),
                    );
                }
            }
        }
        Ok(())
    }
}
fn initial_state(room: &str, cert: &DeviceCert, key: &SigningKey) -> Result<SeedState> {
    use ui::{
        portable_storage::PortableStorage,
        record::{RecordCheckout, RecordId, RecordOwners},
    };
    if cert.device_public_key != key.verifying_key().to_bytes() {
        bail!("initial desktop requires pinned authority")
    }
    // The regular record reservation binds schema, room, record identity and actor
    // before either app initializes content. No widgets or rendering are involved.
    let cache = PortableStorage::new("cloud-initialization", room).map_err(anyhow::Error::msg)?;
    let mut owners = RecordOwners::portable(cache);
    let mut documents = vec![];
    for definition in [word_processor::definition(), chat::definition()] {
        let primary = definition.app_id == "so.tilo.chat";
        let checkout = if primary {
            RecordCheckout::package_primary(definition.record_kind.clone())
        } else {
            RecordCheckout::new(RecordId::for_tab(1), definition.record_kind.clone())
        };
        let records = owners.open(definition.app_id, room)?;
        let mut reservation = records.reserve_record(&checkout)?;
        reservation.initialize(|doc| {
            if primary {
                chat::doc::ChatDoc::seed_starter(
                    doc,
                    &hex::encode(cert.user_public_key),
                    "Me",
                    "ME",
                );
                Ok(())
            } else {
                word_processor::initialize_empty_document(doc)
            }
        })?;
        let bytes = records
            .read(&checkout.id, |doc| doc.clone().save())
            .ok_or_else(|| anyhow!("canonical initializer did not publish record"))?;
        documents.push(SeedDocument {
            app_id: definition.app_id.into(),
            record_id: if primary {
                String::new()
            } else {
                checkout.id.as_str().into()
            },
            kind: definition.record_kind.as_str().into(),
            bytes,
        });
    }
    let mut room_doc = qos_peer_state::room::candidate(
        None,
        &qos_peer_state::seed_room(room, cert, std::slice::from_ref(cert))
            .map_err(anyhow::Error::msg)?,
    )
    .map_err(anyhow::Error::msg)?;
    install_desktop_topology(
        &mut room_doc,
        key,
        word_processor::definition().app_id,
        chat::definition().app_id,
    )?;
    Ok(SeedState {
        room_changes: room_doc
            .get_changes(&[])
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect(),
        documents,
    })
}

fn install_desktop_topology(
    doc: &mut automerge::AutoCommit,
    key: &SigningKey,
    writer_app: &str,
    chat_app: &str,
) -> Result<bool> {
    use automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
    use qos_desktop::{
        AppKind, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
        projection::RoomProjection,
    };
    if doc.get(ROOT, "contents")?.is_some() {
        return Ok(false);
    }
    doc.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let owners = match doc.get(ROOT, "contents_owners_v1")? {
        Some((_, owners)) => owners,
        None => doc.put_object(ROOT, "contents_owners_v1", ObjType::Map)?,
    };
    let reserved = doc
        .keys(&owners)
        .filter_map(|entry| u16::from_str_radix(&entry, 16).ok())
        .collect::<std::collections::BTreeSet<_>>();
    let digest = blake3::derive_key(
        "tilo cloud desktop topology namespace v1",
        key.verifying_key().as_bytes(),
    );
    let start = u32::from(u16::from_be_bytes([digest[0], digest[1]]));
    let salt = (0..u32::from(u16::MAX))
        .map(|offset| ((start + offset) % u32::from(u16::MAX) + 1) as u16)
        .find(|salt| !reserved.contains(salt))
        .ok_or_else(|| anyhow!("topology namespaces exhausted"))?;
    let mut counter = 1;
    let mut mint =
        || qos_desktop::identity::mint_in_namespace(salt, &mut counter).map_err(anyhow::Error::msg);
    let strip = mint()?;
    let writer_group = mint()?;
    let writer = mint()?;
    let chat_group = mint()?;
    let chat = mint()?;
    let tile = |id, title: &str, app: &str| Tab {
        id,
        title: title.into(),
        tile: TileContent::AppPlaceholder {
            kind: AppKind::Other(app.into()),
            label: title.into(),
        },
    };
    let strips = vec![Strip {
        id: StripId(strip),
        name: "Workspace".into(),
        window_groups: vec![
            WindowGroup {
                id: writer_group,
                tabs: vec![tile(writer, "Writer", writer_app)],
                active_tab: writer,
                width: WindowWidth::Normal,
                maximized: false,
            },
            WindowGroup {
                id: chat_group,
                tabs: vec![tile(chat, "Chat", chat_app)],
                active_tab: chat,
                width: WindowWidth::Normal,
                maximized: false,
            },
        ],
    }];
    doc.put(
        &owners,
        format!("{salt:04x}"),
        hex::encode(key.verifying_key().to_bytes()),
    )?;
    doc.put(ROOT, "desktop_counter_v1", counter)?;
    RoomProjection::project_owned_contents(doc, &strips, salt, None);
    RoomProjection::bind_record(doc, strip, writer_group, writer, "tab:0000000000000001")
        .map_err(anyhow::Error::msg)?;
    RoomProjection::bind_record(doc, strip, chat_group, chat, "").map_err(anyhow::Error::msg)?;
    doc.commit();
    Ok(true)
}

/// Record id of the seeded Writer document.
pub(crate) const WRITER_RECORD: &str = "tab:0000000000000001";

/// Persisted spelling of a seeded first-party record.
///
/// New workspaces are seeded under the current `so.tilo.*` id. Workspaces
/// created before the rename keep their record routes under the retired id,
/// so the existing route is found through the shared first-party rename table
/// and used exactly as stored.
pub(crate) fn seeded_app_id(peer: &StatePeer, current: &'static str, record: &str) -> &'static str {
    std::iter::once(current)
        .chain(ui::application::first_party_app_id_lookup_candidates(
            current,
        ))
        .find(|app| peer.document(app, record).is_some())
        .unwrap_or(current)
}

fn bootstrap_desktop(peer: &mut StatePeer, key: &SigningKey) -> Result<bool> {
    use automerge::{ROOT, ReadDoc};
    if key.verifying_key().to_bytes() != peer.authority().device_public_key {
        bail!("desktop migration requires pinned authority")
    }
    let mut room = peer
        .admitted_room()
        .cloned()
        .ok_or_else(|| anyhow!("room governance missing"))?;
    if room.get(ROOT, "contents")?.is_some() {
        return Ok(false);
    }
    let writer_app = seeded_app_id(peer, word_processor::definition().app_id, WRITER_RECORD);
    let chat_app = seeded_app_id(peer, chat::definition().app_id, "");
    if peer.document(writer_app, WRITER_RECORD).is_none() || peer.document(chat_app, "").is_none() {
        bail!("canonical records missing; desktop migration cannot replace them")
    }
    install_desktop_topology(&mut room, key, writer_app, chat_app)?;
    let seed = SeedState {
        room_changes: room
            .get_changes(&[])
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect(),
        documents: vec![],
    };
    let mut candidate = peer.clone();
    candidate
        .install_seed(seed, key, web_time::SystemTime::now())
        .map_err(anyhow::Error::msg)?;
    *peer = candidate;
    Ok(true)
}
