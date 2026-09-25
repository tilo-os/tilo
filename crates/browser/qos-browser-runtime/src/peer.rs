//! One browser state owner with authenticated HTTPS delivery; no relay fallback.
#[path = "peer_agent_reads.rs"]
mod agent_reads;
#[path = "peer_cloud/wasm.rs"]
mod cloud_api;
#[path = "peer_delivery/wasm.rs"]
mod delivery_api;
#[path = "peer_invitation/wasm.rs"]
mod invitation_api;
#[path = "peer_live/wasm.rs"]
mod live_api;
#[path = "peer_live/members.rs"]
mod live_members_api;
#[path = "peer_media/wasm.rs"]
mod media_api;
use automerge::transaction::Transactable;
use ed25519_dalek::SigningKey;
use qos_peer_state::{NeighbourMsg, StatePeer};
use serde::Deserialize;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};
use tilo_user_identity::{
    DeviceCert, DirectoryUpdate, RevocationEntry, RevocationList, WorkspaceDirectory,
};
use ui::portable_storage::{PendingWrite, PortableStorage, StorageKey};
use wasm_bindgen::prelude::*;
use web_time::SystemTime;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Enrollment {
    version: u32,
    certificate: DeviceCert,
    root_public_key: [u8; 32],
    host_certificate: DeviceCert,
    relay_address: iroh_base::EndpointAddr,
    packet_relay: iroh_base::RelayUrl,
    revocations: Vec<RevocationEntry>,
    bootstrap: Bootstrap,
}
#[derive(Deserialize)]
struct Bootstrap {
    room: String,
    workspace_id: String,
    directory_update: DirectoryUpdate,
    directory_author: DeviceCert,
    certified_roster: Vec<DeviceCert>,
    #[serde(default)]
    delivery_hint: Option<qos_peer_state::join::TransportHint>,
}
use crate::peer_delivery_cursor::CursorIdentity;
use crate::peer_journal::{Journal, JournalStorage};
use crate::peer_storage_batch::PeerStorageBatches;
struct Peer {
    data_host: DeviceCert,
    media_enrollment: media_api::MediaEnrollment,
    media: RefCell<media_api::MediaState>,
    delivery: RefCell<delivery_api::DeliveryState>,
    incoming: crate::peer_delivery::incoming::IncomingBudget,
    actions: RefCell<crate::action_client::PendingActions>,
    agent_reads: RefCell<crate::agent_reads::PendingAgentReads>,
    health_projection: RefCell<crate::peer_journal::HealthProjection>,
    action_v2: std::cell::Cell<bool>,
    admission_failure: RefCell<Option<String>>,
    initial_admission_replay: RefCell<crate::admission_health::InitialAdmissionReplay>,
    was_ready: std::cell::Cell<bool>,
    published: RefCell<BTreeSet<[u8; 32]>>,
    id: u32,
    cache: PortableStorage,
    outbox: RefCell<Vec<NeighbourMsg>>,
    transport_generation: std::cell::Cell<u64>,
    state_revision: std::cell::Cell<u64>,
    transport_connected: std::cell::Cell<bool>,
    admission_progress: RefCell<crate::admission_health::AdmissionProgress>,
    last_relay_contact: std::cell::Cell<Option<f64>>,
    last_peer_contact: std::cell::Cell<Option<f64>>,
    batches: RefCell<PeerStorageBatches>,
    journal_storage: RefCell<JournalStorage>,
    committed_at: std::cell::Cell<Option<f64>>,
    state: RefCell<StatePeer>,
    _key: SigningKey,
    callback: js_sys::Function,
    closed: std::cell::Cell<bool>,
    /// Whether the last full report was `ready`; the light report between
    /// commit boundaries then needs no recomputed health (see `report_light`).
    last_report_ready: std::cell::Cell<bool>,
    /// A report whose text the host already holds is not delivered again.
    report_dedup: RefCell<crate::report_text::ReportDedup>,
}
thread_local! {static PEERS:RefCell<BTreeMap<u32,Rc<Peer>>>=const{RefCell::new(BTreeMap::new())};static NEXT:std::cell::Cell<u32>=const{std::cell::Cell::new(1)};}
thread_local! {
    static LOCAL_CONTEXTS: RefCell<crate::peer_onboarding_custody::Table<crate::peer_onboarding::LocalContext>> = RefCell::new(Default::default());
}
struct LocalUse {
    id: u32,
    entry: Rc<crate::peer_onboarding_custody::Entry<crate::peer_onboarding::LocalContext>>,
}
impl Drop for LocalUse {
    fn drop(&mut self) {
        LOCAL_CONTEXTS.with(|table| table.borrow_mut().settle(self.id, &self.entry));
    }
}
fn local_owner(id: Option<u32>) -> Result<Option<crate::peer_onboarding_custody::Owner>, JsValue> {
    id.map(|id| {
        PEERS.with(|all| {
            let all = all.borrow();
            let peer = all
                .get(&id)
                .filter(|peer| !peer.closed.get())
                .ok_or_else(|| error("closed Peer"))?;
            Ok(crate::peer_onboarding_custody::Owner {
                id,
                generation: peer.transport_generation.get(),
            })
        })
    })
    .transpose()
}
fn error(e: impl ToString) -> JsValue {
    JsValue::from_str(&e.to_string())
}
fn report(peer: &Peer, phase: &str, error: Option<String>) {
    let source = if !peer.transport_connected.get() {
        crate::admission_health::ReportSource::Transport
    } else {
        crate::admission_health::ReportSource::Runtime
    };
    report_from(peer, phase, error, source);
}
fn report_from(
    peer: &Peer,
    phase: &str,
    error: Option<String>,
    source: crate::admission_health::ReportSource,
) {
    let state = peer.state.borrow();
    let complete = crate::desktop::admission(&state).is_ok_and(|admission| admission.complete());
    let outgoing = crate::admission_health::outgoing_projection(
        &state,
        &peer.outbox.borrow(),
        SystemTime::now(),
    );
    let awaiting_governance = peer.initial_admission_replay.borrow_mut().waiting(
        &state,
        peer._key.verifying_key().to_bytes(),
        SystemTime::now(),
    );
    let incomplete = crate::admission_health::permission_failure(
        required_read_failure(peer, &state),
        awaiting_governance,
    )
    .or_else(|| state.incomplete_reason())
    .or_else(|| peer.admission_failure.borrow().clone());
    // One pass over the pending writes for both the phase and the trigger; the
    // health projection below counts them per record.
    let persistence_needed = peer.cache.has_pending_writes();
    let phase = crate::admission_health::runtime_phase(
        phase,
        complete,
        persistence_needed,
        peer.was_ready.get(),
        incomplete.is_some(),
        awaiting_governance,
    );
    if phase == "ready" {
        peer.was_ready.set(true);
    }
    let error = error.or(incomplete);
    let health = health(
        peer,
        &state,
        phase == "ready",
        error.as_deref(),
        source,
        outgoing.blocked_expired,
        awaiting_governance,
    );
    let synchronized = health["records"].as_array().is_some_and(|records| {
        !records.is_empty()
            && records
                .iter()
                .all(|record| record["acknowledgement"]["coversCurrentBasis"] == true)
    });
    let value = serde_json::json!({"phase":phase,"desktop":{"version":1,"mode":"session"},"room":state.room(),"deviceId":peer._key.verifying_key().to_bytes().iter().map(|b|format!("{b:02x}")).collect::<String>(),"records":state.rows(),"pendingPrerequisites":state.pending_count(),"persistenceNeeded":persistence_needed,"outgoingPending":peer.outbox.borrow().len(),"outgoingBlocked":outgoing.blocked_expired,"outgoingExpiryAtMs":outgoing.expiry_at_ms,"synchronization":if outgoing.blocked_expired>0{"blocked"}else if synchronized{"acknowledged"}else{"pending"},"error":error,"health":health});
    let text = value.to_string();
    peer.last_report_ready.set(phase == "ready");
    // Health was recomputed above; only its delivery is skipped when the host
    // already holds this exact text (a forced publication that committed
    // nothing, an inbound frame that changed nothing).
    if peer
        .report_dedup
        .borrow_mut()
        .should_emit(&text, persistence_needed)
    {
        let _ = peer
            .callback
            .call1(&JsValue::NULL, &JsValue::from_str(&text));
    }
}
/// Report between commit boundaries without recomputing health. A typed key
/// otherwise computed the full health report three times (publication,
/// acknowledgement, committed). The light report carries only the retained
/// phase and the persistence trigger; the host merges statuses field by field,
/// so rows, outbox counts and health keep the last full report's values until
/// `peer_committed` or a transport or failure path refreshes them, at most one
/// commit cycle later. Before the peer has ever been ready this is a full
/// report.
fn report_light(peer: &Peer) {
    if !(peer.was_ready.get() && peer.last_report_ready.get()) {
        return report(peer, "admitting", None);
    }
    let persistence_needed = peer.cache.has_pending_writes();
    let text = crate::report_text::light_report("ready", persistence_needed);
    if peer
        .report_dedup
        .borrow_mut()
        .should_emit(&text, persistence_needed)
    {
        let _ = peer
            .callback
            .call1(&JsValue::NULL, &JsValue::from_str(&text));
    }
}
#[wasm_bindgen]
pub async fn connect_peer(
    raw: String,
    identity: Vec<u8>,
    hydrated: JsValue,
    callback: js_sys::Function,
    context: u32,
) -> Result<u32, JsValue> {
    connect_owner(None, raw, identity, hydrated, Some(callback), context).await
}
#[wasm_bindgen]
pub async fn reconnect_peer(
    id: u32,
    raw: String,
    identity: Vec<u8>,
    context: u32,
) -> Result<(), JsValue> {
    connect_owner(Some(id), raw, identity, JsValue::NULL, None, context)
        .await
        .map(|_| ())
}
async fn connect_owner(
    existing: Option<u32>,
    raw: String,
    identity: Vec<u8>,
    hydrated: JsValue,
    callback: Option<js_sys::Function>,
    context: u32,
) -> Result<u32, JsValue> {
    let expected_owner = local_owner(existing)?;
    let entry = LOCAL_CONTEXTS
        .with(|table| {
            table
                .borrow_mut()
                .acquire(context, expected_owner, tilo_relay::clock::now_ns())
        })
        .map_err(error)?;
    let custody = LocalUse { id: context, entry };
    let incoming = custody.entry.context.budget().clone();
    let current = || {
        if !custody.entry.alive.get()
            || tilo_relay::clock::now_ns() >= custody.entry.context.expires_at_ns()
            || local_owner(existing).map_err(|_| "local onboarding owner closed".to_string())?
                != expected_owner
        {
            return Err("local onboarding owner changed or expired".into());
        }
        Ok(())
    };
    let governance =
        crate::peer_onboarding::local_governance(&custody.entry.context, &identity, &raw, current)
            .await
            .map_err(error)?;
    current().map_err(error)?;
    // This projection remains legacy authority admission; incoming outer bytes
    // and its temporary owned DTO are retained on the same Peer ledger.
    let _projection = incoming
        .reserve(
            raw.len()
                .checked_mul(6)
                .and_then(|n| n.checked_add(65536))
                .ok_or_else(|| error("enrollment projection overflow"))?,
        )
        .map_err(error)?;
    let config: Enrollment = serde_json::from_str(&raw).map_err(error)?;
    if config.version != 2 {
        return Err(error("unsupported enrollment version"));
    }
    let key = crate::identity::key(&identity, &raw).map_err(error)?;
    let device = key.verifying_key().to_bytes();
    if device != config.certificate.device_public_key
        || config.certificate.user_public_key != config.root_public_key
        || config.host_certificate.user_public_key != config.root_public_key
    {
        return Err(error("enrollment key/root binding mismatch"));
    }
    let now = tilo_relay::clock::now_ns();
    let mut revoked = RevocationList::new();
    for entry in config.revocations {
        if entry.user_public_key != config.root_public_key {
            return Err(error("revocation root mismatch"));
        }
        revoked.insert(entry).map_err(error)?;
    }
    tilo_user_identity::verify_device_cert_at(&config.certificate, now).map_err(error)?;
    tilo_user_identity::verify_device_cert_at(&config.host_certificate, now).map_err(error)?;
    if revoked.is_revoked(&device) || revoked.is_revoked(&config.host_certificate.device_public_key)
    {
        return Err(error("enrolled device revoked"));
    }
    let workspace = config
        .bootstrap
        .workspace_id
        .parse::<u64>()
        .map_err(error)?;
    if !tilo_relay::room_workspace_id_matches(&config.bootstrap.room, workspace) {
        return Err(error("workspace room binding mismatch"));
    }
    let mut directory = WorkspaceDirectory::new(config.root_public_key, device);
    directory
        .apply_incremental(
            &config.bootstrap.directory_update,
            &config.bootstrap.directory_author,
            &revoked,
            now,
        )
        .map_err(error)?;
    let rows = directory.as_map();
    let row = rows
        .get(&workspace)
        .ok_or_else(|| error("signed directory missing workspace"))?;
    if !row.member_devices.contains(&device)
        || !row
            .host_devices
            .contains(&config.host_certificate.device_public_key)
    {
        return Err(error("signed directory membership/host mismatch"));
    }
    let initial = StatePeer::new(
        &config.bootstrap.room,
        config.bootstrap.directory_author,
        config.bootstrap.certified_roster.clone(),
        revoked.clone(),
        SystemTime::now(),
    )
    .map_err(error)?;
    let config = ValidatedOwner {
        incoming,
        key,
        initial,
        revoked,
        certificate: config.certificate,
        host_certificate: config.host_certificate.clone(),
        delivery_host: config.host_certificate,
        workspace,
        room: config.bootstrap.room,
        delivery_hint: config.bootstrap.delivery_hint,
        alive: Some(Rc::clone(&custody.entry.alive)),
        governance: GovernanceSource::Local(Rc::new(governance)),
    };
    connect_validated(existing, config, hydrated, callback).await
}

fn incoming_for_owner(
    existing: Option<u32>,
) -> Result<crate::peer_delivery::incoming::IncomingBudget, JsValue> {
    if let Some(id) = existing {
        return PEERS.with(|all| {
            all.borrow()
                .get(&id)
                .filter(|peer| !peer.closed.get())
                .map(|peer| peer.incoming.clone())
                .ok_or_else(|| error("closed Peer"))
        });
    }
    crate::peer_delivery::incoming::IncomingBudget::new(64 * 1024 * 1024).map_err(error)
}

enum GovernanceSource {
    Empty,
    Invited(Rc<crate::peer_onboarding::OnboardingReply>),
    Local(Rc<crate::peer_onboarding::GovernanceReply>),
    /// Authenticated owner membership refresh (`OwnerMembershipComplete`).
    Cloud(Rc<crate::peer_onboarding::OnboardingReply>),
}
impl GovernanceSource {
    fn messages(&self) -> impl Iterator<Item = &[u8]> {
        let invited = match self {
            Self::Invited(reply) => match reply.response() {
                qos_peer_state::join::JoinTransportResponse::Complete(bootstrap)
                | qos_peer_state::join::JoinTransportResponse::MembershipComplete(bootstrap) => {
                    Some(bootstrap)
                }
                _ => None,
            },
            _ => None,
        };
        let owner = match self {
            Self::Cloud(reply) => match reply.response() {
                qos_peer_state::join::JoinTransportResponse::OwnerMembershipComplete(bootstrap) => {
                    Some(bootstrap)
                }
                _ => None,
            },
            _ => None,
        };
        let local = match self {
            Self::Local(reply) => Some(reply),
            _ => None,
        };
        invited
            .into_iter()
            .flat_map(|bootstrap| bootstrap.governance_messages.iter().map(Vec::as_slice))
            .chain(
                owner
                    .into_iter()
                    .flat_map(|bootstrap| bootstrap.governance_messages.iter().map(Vec::as_slice)),
            )
            .chain(
                local
                    .into_iter()
                    .flat_map(|reply| reply.messages().map(|(bytes, _)| bytes)),
            )
    }
    fn is_empty(&self) -> bool {
        self.messages().next().is_none()
    }
}

// Not deserializable: each source must establish its own identity and authority.
struct ValidatedOwner {
    incoming: crate::peer_delivery::incoming::IncomingBudget,
    key: SigningKey,
    initial: StatePeer,
    revoked: RevocationList,
    certificate: DeviceCert,
    host_certificate: DeviceCert,
    delivery_host: DeviceCert,
    workspace: u64,
    room: String,
    delivery_hint: Option<qos_peer_state::join::TransportHint>,
    alive: Option<Rc<std::cell::Cell<bool>>>,
    governance: GovernanceSource,
}
impl ValidatedOwner {
    fn cancelled(&self) -> bool {
        self.alive.as_ref().is_some_and(|alive| !alive.get())
    }
}
async fn connect_validated(
    existing: Option<u32>,
    config: ValidatedOwner,
    hydrated: JsValue,
    callback: Option<js_sys::Function>,
) -> Result<u32, JsValue> {
    let key = config.key.clone();
    let device = key.verifying_key().to_bytes();
    let workspace = config.workspace;
    let workspace_root = config.initial.authority().user_public_key;
    let initial = config.initial.clone();
    let revoked = config.revoked.clone();
    let profile = format!("certified:{}", config.room);
    let device_hex = device
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let owner = existing
        .map(|id| {
            PEERS
                .with(|peers| peers.borrow().get(&id).cloned())
                .ok_or_else(|| error("closed peer"))
        })
        .transpose()?;
    if let Some(peer) = &owner {
        if peer.state.borrow().room() != initial.room()
            || peer.state.borrow().authority() != initial.authority()
            || peer._key.verifying_key() != key.verifying_key()
            || peer.data_host != config.host_certificate
            || peer.media_enrollment.host != config.delivery_host
            || peer.media_enrollment.device != config.certificate
            || peer.media_enrollment.workspace != workspace
        {
            return Err(error("replacement transport owner binding mismatch"));
        }
        if !config.governance.is_empty() {
            crate::browser::prepare_peer_remote(peer.id).map_err(error)?;
        }
    }
    let cache = if let Some(peer) = &owner {
        peer.cache.clone()
    } else {
        crate::browser::load_cache(&profile, &device_hex, &hydrated)?
    };
    let cursor_identity = CursorIdentity {
        workspace,
        room: config.room.clone(),
        workspace_root: workspace_root,
        device,
        device_root: config.certificate.user_public_key,
        host: config.delivery_host.device_public_key,
        host_root: config.delivery_host.user_public_key,
    };
    let (mut state, mut outbox, journal_storage) = if let Some(peer) = &owner {
        (
            peer.state.borrow().clone(),
            peer.outbox.borrow().clone(),
            peer.journal_storage.borrow().clone(),
        )
    } else if let Some(saved) = cache.get(&StorageKey::Admission {
        workspace: config.room.clone(),
    }) {
        let journal = Journal::decode(&saved.bytes).map_err(error)?;
        let restored = StatePeer::restore(
            &journal.state,
            &config.room,
            initial.authority().clone(),
            initial.certificates(),
            revoked.clone(),
            SystemTime::now(),
        )
        .map_err(error)?;
        let storage = journal.storage.clone();
        let outbox = journal.into_unretired_outbox(&restored);
        (restored, outbox, storage)
    } else {
        (initial, vec![], JournalStorage::default())
    };
    // Both local and invited handoffs carry original authority and member
    // publications. They enter one bounded native cut through StatePeer's
    // validated incoming path; a refused item leaves the retained offline
    // owner's state untouched. An already admitted invitation replays as a
    // no-op, so the reconnecting owner keeps its offline edits.
    if !config.governance.is_empty() {
        let now = SystemTime::now();
        let mut cut = crate::peer_delivery::admission::CutAdmission::begin(
            &state,
            config.incoming.clone(),
            now,
        )
        .map_err(error)?;
        for bytes in config.governance.messages() {
            cut.push_retained(bytes, now).map_err(error)?;
        }
        state = cut.finish(now).map_err(error)?;
    }
    if !config.governance.is_empty() {
        if !state.certificates().contains(&config.certificate) {
            return Err(error("invited current device certificate mismatch"));
        }
        state
            .permissions(device, SystemTime::now())
            .map_err(error)?;
    }
    if journal_storage
        .delivery
        .as_ref()
        .is_some_and(|cursor| !cursor.matches_identity(&cursor_identity))
    {
        return Err(error(
            "stored cursor identity does not match enrolled workspace; retain local storage for recovery",
        ));
    }
    if journal_storage
        .descriptor
        .as_ref()
        .is_some_and(|floor| floor.identity != cursor_identity)
    {
        return Err(error(
            "stored descriptor identity does not match enrolled workspace; retain local storage for recovery",
        ));
    }
    // A fresh owner's retained record can be committed ahead of its cursor
    // journal (a record-only batch inside the publication window). Advance the
    // journal to that verified descendant before any projection or staging, or
    // refuse explicitly; never merge it away.
    if owner.is_none() {
        crate::admitted_records::repair_record_basis(
            &cache,
            &mut state,
            &mut outbox,
            &key,
            |app| crate::desktop::definition(app).is_some(),
            SystemTime::now(),
        )
        .map_err(error)?;
    }
    if let Some(peer) = &owner {
        if !config.governance.is_empty() {
            install_candidate(peer, state.clone()).map_err(error)?;
        }
    }
    // Identity, StatePeer and durable cache exist before any network proof.
    // JS binds the actual storage lease and ACKs the floor before HTTP setup.
    if config.cancelled() {
        return Err(error("invited connection cancelled"));
    }
    let id = match &owner {
        Some(peer) => peer.id,
        None => NEXT.with(|next| {
            let id = next.get();
            next.set(
                id.checked_add(1)
                    .ok_or_else(|| error("Peer handle exhausted"))?,
            );
            Ok::<_, JsValue>(id)
        })?,
    };
    let callback = match (&owner, callback) {
        (Some(peer), _) => peer.callback.clone(),
        (_, Some(callback)) => callback,
        _ => return Err(error("missing Peer status callback")),
    };
    let fresh_owner = owner.is_none();
    let peer = if let Some(peer) = owner {
        delivery_api::stop(&peer);
        *peer.actions.borrow_mut() = Default::default();
        *peer.agent_reads.borrow_mut() = Default::default();
        peer.published.borrow_mut().clear();
        peer.media_enrollment
            .hint
            .replace(config.delivery_hint.clone());
        peer
    } else {
        Rc::new(Peer {
            data_host: config.host_certificate.clone(),
            media_enrollment: media_api::MediaEnrollment {
                workspace,
                governor: state.authority().clone(),
                host: config.delivery_host.clone(),
                device: config.certificate.clone(),
                hint: RefCell::new(config.delivery_hint.clone()),
            },
            media: RefCell::default(),
            delivery: RefCell::default(),
            incoming: config.incoming.clone(),
            health_projection: RefCell::default(),
            actions: RefCell::default(),
            agent_reads: RefCell::default(),
            action_v2: std::cell::Cell::new(false),
            admission_failure: RefCell::new(None),
            initial_admission_replay: RefCell::new(
                crate::admission_health::InitialAdmissionReplay::new(&state),
            ),
            was_ready: std::cell::Cell::new(false),
            published: RefCell::new(BTreeSet::new()),
            id,
            cache,
            outbox: RefCell::new(outbox),
            state: RefCell::new(state),
            _key: key,
            callback,
            closed: std::cell::Cell::new(false),
            last_report_ready: std::cell::Cell::new(false),
            report_dedup: RefCell::default(),
            transport_generation: std::cell::Cell::new(1),
            state_revision: std::cell::Cell::new(1),
            transport_connected: std::cell::Cell::new(false),
            admission_progress: RefCell::default(),
            last_relay_contact: std::cell::Cell::new(None),
            last_peer_contact: std::cell::Cell::new(None),
            batches: RefCell::new(PeerStorageBatches::new(id)),
            journal_storage: RefCell::new(journal_storage),
            committed_at: std::cell::Cell::new(None),
        })
    };
    // A replacement owner installs through `install_candidate` above. A fresh
    // owner's restored/handoff state can already be complete, and a desktop may
    // mount from it before any delivery cut: project its records now so the
    // portable record owners can open exactly what admission reports.
    if fresh_owner {
        crate::admitted_records::install_initial(
            &peer.cache,
            &peer.state.borrow(),
            peer._key.verifying_key().to_bytes(),
        )
        .map_err(error)?;
    }
    stage_journal(&peer, &peer.state.borrow(), &peer.outbox.borrow()).map_err(error)?;
    PEERS.with(|peers| peers.borrow_mut().insert(id, peer.clone()));
    report(&peer, "admitting", None);
    Ok(id)
}

fn receive_private_action(
    peer: &Rc<Peer>,
    event: tilo_relay::transport_v2::EnvelopeV2,
    transport_generation: u64,
) {
    if peer.closed.get() || peer.transport_generation.get() != transport_generation {
        return;
    }
    // The closed HTTPS owner admitted this correlated native response.
    // Current membership/certificate validity is rechecked for every response.
    if let Ok(identity) = action_context(&peer) {
        if matches!(&event, tilo_relay::transport_v2::EnvelopeV2::Delivered { action, .. }
                        if matches!(action.message, tilo_relay::peer_actions::ActionMessage::CommittedDesktop { .. }))
        {
            // Flush local edits before cloning admission, just as on
            // the retained channel. A complete batch installs once.
            if let Err(error) = crate::browser::prepare_peer_remote(peer.id) {
                mark_failure(peer.id, error);
                return;
            }
            let admission = crate::committed_desktop::admit(
                &peer.state.borrow(),
                &event,
                peer._key.verifying_key().to_bytes(),
                SystemTime::now(),
            );
            let emit_notice = |notice: &crate::committed_desktop::CommittedDesktopNotice| {
                let value = serde_json::json!({"committedDesktop":{"invocation":notice.invocation,"room":notice.room,"opened":notice.opened}}).to_string();
                let _ = peer
                    .callback
                    .call1(&JsValue::NULL, &JsValue::from_str(&value));
            };
            match admission {
                Ok(crate::committed_desktop::Admission::Ready { candidate, notice }) => {
                    peer.last_peer_contact.set(Some(js_sys::Date::now()));
                    if let Err(e) = install_candidate(&peer, candidate) {
                        *peer.admission_failure.borrow_mut() = Some(e.clone());
                        report(&peer, "incomplete", Some(e));
                    } else if let Some(notice) = notice {
                        emit_notice(&notice);
                    }
                }
                Ok(crate::committed_desktop::Admission::AwaitingRetainedState(notice)) => {
                    peer.last_peer_contact.set(Some(js_sys::Date::now()));
                    emit_notice(&notice);
                }
                Ok(crate::committed_desktop::Admission::Ignored) => (),
                Err(error) => web_sys::console::debug_1(&JsValue::from_str(&format!(
                    "Committed desktop direct admission rejected: {error}"
                ))),
            }
            return;
        }
        let root = peer.state.borrow().authority().user_public_key;
        peer.agent_reads
            .borrow_mut()
            .receive(&event, &identity, transport_generation);
        let catalogue = match &event {
            tilo_relay::transport_v2::EnvelopeV2::Delivered { action, .. } => {
                Some(action.message.clone())
            }
            _ => None,
        };
        if peer.actions.borrow_mut().receive(event, root) {
            if let Some(message) = catalogue {
                if let tilo_relay::peer_actions::ActionMessage::BrokerCatalogue { host, .. } =
                    &message
                {
                    peer.delivery.borrow_mut().broker = Some(*host);
                }
                peer.agent_reads.borrow_mut().observe_catalogue(
                    &message,
                    &identity,
                    transport_generation,
                );
            }
        }
        // Wake command waiters as soon as the authenticated response
        // is installed. The JS host keeps only a deadline timer.
        let _ = peer.callback.call1(
            &JsValue::NULL,
            &JsValue::from_str("{\"actionResponseReady\":true}"),
        );
    }
}
/// Request bootstrap on the retained authenticated transport without replacing
/// the owner or requiring the very document permissions bootstrap may supply.
#[wasm_bindgen]
pub fn peer_request_sync(id: u32, expected_transport_generation: String) -> Result<(), JsValue> {
    let expected = expected_transport_generation
        .parse::<u64>()
        .map_err(error)?;
    PEERS.with(|peers| {
        let peer = peers
            .borrow()
            .get(&id)
            .cloned()
            .ok_or_else(|| error("closed peer"))?;
        if peer.closed.get()
            || !peer.transport_connected.get()
            || peer.transport_generation.get() != expected
        {
            return Err(error("sync transport generation is unavailable"));
        }
        delivery_api::request_snapshot(&peer).map_err(error)
    })
}
/// Disconnect transport without destroying durable cache/outbox ownership.
#[wasm_bindgen]
pub async fn disconnect_peer(id: u32) {
    let peer = PEERS.with(|peers| peers.borrow().get(&id).cloned());
    if let Some(peer) = peer {
        *peer.agent_reads.borrow_mut() = Default::default();
        delivery_api::stop(&peer);
    }
}
#[wasm_bindgen]
pub async fn close_peer(id: u32) {
    let peer = PEERS.with(|peers| peers.borrow_mut().remove(&id));
    if let Some(peer) = peer {
        *peer.agent_reads.borrow_mut() = Default::default();
        peer.closed.set(true);
        peer.media.borrow_mut().close();
        peer.delivery.borrow_mut().close();
        if let Some(next) = peer.transport_generation.get().checked_add(1) {
            peer.transport_generation.set(next);
        }
        report(&peer, "closed", None);
    }
}

#[wasm_bindgen]
pub fn create_identity() -> Vec<u8> {
    crate::identity::create()
}
#[wasm_bindgen]
pub fn identity_request(material: &[u8]) -> Result<String, JsValue> {
    crate::identity::request(material).map_err(error)
}
#[wasm_bindgen]
pub fn identity_complete(material: &[u8], challenge: &str) -> Result<String, JsValue> {
    crate::identity::complete(material, challenge).map_err(error)
}
#[wasm_bindgen]
pub fn bind_identity(material: &[u8], challenge: &str, raw: &str) -> Result<Vec<u8>, JsValue> {
    crate::identity::bind(material, challenge, raw).map_err(error)
}
#[wasm_bindgen]
pub fn bind_identity_for_peer(
    material: &[u8],
    challenge: &str,
    raw: &str,
    existing: Option<u32>,
) -> Result<String, JsValue> {
    let owner = local_owner(existing)?;
    let bound = crate::peer_onboarding::bind_local(
        material,
        challenge,
        raw,
        incoming_for_owner(existing)?,
        tilo_relay::clock::now_ns(),
    )
    .map_err(error)?;
    let material = bound.material().to_vec();
    let context = LOCAL_CONTEXTS
        .with(|table| {
            table
                .borrow_mut()
                .insert(bound.into_context(), owner, tilo_relay::clock::now_ns())
        })
        .map_err(error)?;
    Ok(serde_json::json!({"material":material,"context":context}).to_string())
}
#[wasm_bindgen]
pub fn cancel_local_onboarding(context: u32) {
    LOCAL_CONTEXTS.with(|table| table.borrow_mut().cancel(context));
}
#[wasm_bindgen]
pub fn peer_enrollment(raw: &str) -> Result<String, JsValue> {
    let config: Enrollment = serde_json::from_str(raw).map_err(error)?;
    let device = config
        .certificate
        .device_public_key
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    Ok(serde_json::json!({"profile":format!("certified:{}",config.bootstrap.room),"device":device,"room":config.bootstrap.room}).to_string())
}
fn storage_key(room: &str, app: &str, record: &str) -> StorageKey {
    if record.is_empty() {
        StorageKey::Primary {
            package: app.into(),
            workspace: room.into(),
        }
    } else {
        StorageKey::Record {
            package: app.into(),
            workspace: room.into(),
            record: record.into(),
        }
    }
}
fn install_records(peer: &Peer, state: &StatePeer) -> Result<(), String> {
    crate::admitted_records::install(&peer.cache, state, peer._key.verifying_key().to_bytes())
}

fn stage_journal(peer: &Peer, state: &StatePeer, outbox: &[NeighbourMsg]) -> Result<(), String> {
    let mut journal = Journal::from_state(state, outbox.to_vec())?;
    // Reader-first migration: never activate v3 here, or drop hydrated v3 metadata.
    journal.storage = peer.journal_storage.borrow().clone();
    let bytes = journal.encode()?;
    peer.cache.put(
        StorageKey::Admission {
            workspace: state.room().into(),
        },
        bytes,
        None,
    )
}
/// The canonical record owner has already committed this document. Avoid a
/// full save/load round trip merely to recover its existing CRDT graph.
pub(crate) fn local_owned_document(
    id: u32,
    app: &str,
    record: &str,
    mut current: automerge::AutoCommit,
) -> Result<(), String> {
    if current.pending_ops() != 0 {
        return Err("record mutation is still unfinished".into());
    }
    PEERS.with(|peers| {
        let peer = peers
            .borrow()
            .get(&id)
            .cloned()
            .ok_or("certified peer is closed")?;
        let before = peer
            .state
            .borrow()
            .document_heads(app, record)
            .ok_or("canonical native record not admitted")?;
        if current.base_heads() == before {
            return Ok(());
        }
        // LIVE_V1: this edit builds on any speculative forwarded changes.
        live_api::promote_for_local_publication(&peer);
        let next_revision = peer
            .state_revision
            .get()
            .checked_add(1)
            .ok_or("canonical Peer revision exhausted")?;
        let delta = current.save_after(&before);
        let mut candidate = peer.state.borrow().clone();
        let message =
            candidate.publish_incremental(app, record, delta, &peer._key, SystemTime::now())?;
        // LIVE_V1: members get this exact signed item from the author's own
        // ops track at once (before the IndexedDB commit and upload).
        let live_forward = live_api::author_canonical(&peer, &message);
        let mut outbox = peer.outbox.borrow().clone();
        outbox.push(message);
        let key = storage_key(candidate.room(), app, record);
        let actor = peer
            .cache
            .get(&key)
            .ok_or("record actor custody missing")?
            .actor;
        // Queue the record with the heads of its canonical owner, so batch
        // capture and validation never decode these bytes to learn them.
        peer.cache.put_document(key, &mut current, actor)?;
        stage_journal(&peer, &candidate, &outbox)?;
        *peer.state.borrow_mut() = candidate;
        peer.state_revision.set(next_revision);
        *peer.outbox.borrow_mut() = outbox;
        live_api::author_published(&peer, live_forward);
        report_light(&peer);
        Ok(())
    })
}
#[wasm_bindgen]
pub fn peer_pending_writes(id: u32) -> Result<String, JsValue> {
    PEERS.with(|peers| {
        let peers = peers.borrow();
        let peer = peers.get(&id).ok_or_else(|| error("closed peer"))?;
        serde_json::to_string(
            &peer
                .cache
                .pending_writes()
                .iter()
                .map(crate::browser::write_json)
                .collect::<Vec<_>>(),
        )
        .map_err(error)
    })
}
/// Recompute the read-only projection when a signed outbox envelope reaches
/// its freshness deadline. This never publishes or changes durable custody.
#[wasm_bindgen]
pub fn peer_refresh_status(id: u32) -> Result<(), JsValue> {
    PEERS.with(|peers| {
        let peer = peers
            .borrow()
            .get(&id)
            .cloned()
            .ok_or_else(|| error("closed peer"))?;
        if !peer.closed.get() {
            report(&peer, "admitting", None);
        }
        Ok(())
    })
}
#[wasm_bindgen]
pub fn peer_committed(id: u32) -> Result<(), JsValue> {
    PEERS.with(|peers| {
        let peer = peers
            .borrow()
            .get(&id)
            .cloned()
            .ok_or_else(|| error("closed peer"))?;
        if peer.cache.has_pending_writes() {
            return Err(error("admission/record transaction not committed"));
        }
        if peer.closed.get() {
            return Ok(());
        }
        delivery_api::queue_committed(&peer).map_err(error)?;
        // Keep durable outgoing bytes until a validated governor checkpoint covers them.
        // A queued transport send alone is not delivery acknowledgement.
        let complete = crate::desktop::admission(&peer.state.borrow())
            .is_ok_and(|admission| admission.complete());
        report(&peer, if complete { "ready" } else { "admitting" }, None);
        Ok(())
    })
}
pub(crate) fn permissions(id: u32) -> Result<ui::permissions::PermissionContext, String> {
    PEERS.with(|peers| {
        let peer = peers.borrow().get(&id).cloned().ok_or("closed peer")?;
        let (role, caps, person) = peer
            .state
            .borrow()
            .permissions(peer._key.verifying_key().to_bytes(), SystemTime::now())?;
        Ok(ui::permissions::PermissionContext::from_capabilities(
            ui::permissions::Actor::person(Some(role), &person),
            caps,
        ))
    })
}

pub(crate) fn mark_failure(id: u32, failure: String) {
    PEERS.with(|peers| {
        if let Some(peer) = peers.borrow().get(&id) {
            *peer.admission_failure.borrow_mut() = Some(failure.clone());
            report(peer, "incomplete", Some(failure));
        }
    });
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BatchResult {
    batch_id: String,
    epoch: String,
    observed_at_ms: f64,
    result: BatchOutcome,
}
#[derive(Deserialize)]
struct BatchOutcome {
    ok: bool,
    code: Option<String>,
}
fn encode_pending_batch<T>(
    id: u32,
    encode: impl FnOnce(
        &str,
        &[PendingWrite],
        &[qos_peer_state::DocumentRow],
        Option<&str>,
    ) -> Result<T, JsValue>,
) -> Result<T, JsValue> {
    PEERS.with(|peers| {
        let peer = peers
            .borrow()
            .get(&id)
            .cloned()
            .ok_or_else(|| error("closed peer"))?;
        let state = peer.state.borrow();
        let records = state.rows();
        let required = records
            .iter()
            .filter(|row| crate::desktop::definition(&row.app_id).is_some())
            .map(|row| storage_key(state.room(), &row.app_id, &row.record_id))
            .collect::<Vec<_>>();
        // Only a cursor journal binds record heads at capture and reload.
        let admitted = if peer.journal_storage.borrow().delivery.is_some() {
            records.as_slice()
        } else {
            &[]
        };
        peer.batches
            .borrow_mut()
            .capture_transfer(
                &peer.cache,
                &required,
                admitted,
                peer.transport_generation.get(),
                |revision| peer.admission_progress.borrow().batch_basis(revision),
                |batch_id, writes, transfer| {
                    encode(batch_id, writes, &records, transfer).map_err(|failure| {
                        failure
                            .as_string()
                            .unwrap_or_else(|| "storage batch encoding failed".into())
                    })
                },
            )
            .map_err(error)
    })
}
#[wasm_bindgen]
pub fn peer_pending_batch(id: u32) -> Result<String, JsValue> {
    encode_pending_batch(id, |id, writes, records, transfer| {
        let values = writes
            .iter()
            .map(crate::browser::write_json)
            .collect::<Vec<_>>();
        let mut result = serde_json::json!({"id":id,"writes":values,"records":records});
        if let Some(transfer) = transfer {
            result["transferId"] = transfer.into();
        }
        Ok(result.to_string())
    })
}
/// JS-owned typed buffers avoid expanding every binary byte into JSON digits.
/// The Rust-owned batch remains the sole authority for acknowledgement.
#[wasm_bindgen]
pub fn peer_pending_batch_bytes(id: u32) -> Result<JsValue, JsValue> {
    encode_pending_batch(id, |id, writes, records, transfer| {
        let result = js_sys::Object::new();
        let values = js_sys::Array::new();
        for write in writes {
            values.push(&crate::browser::write_bytes(write)?);
        }
        js_sys::Reflect::set(&result, &"id".into(), &id.into())?;
        if let Some(transfer) = transfer {
            js_sys::Reflect::set(&result, &"transferId".into(), &transfer.into())?;
        }
        js_sys::Reflect::set(&result, &"writes".into(), &values)?;
        let records = js_sys::JSON::parse(&serde_json::to_string(records).map_err(error)?)?;
        js_sys::Reflect::set(&result, &"records".into(), &records)?;
        Ok(result.into())
    })
}
#[wasm_bindgen]
pub fn peer_bind_storage_epoch(id: u32, epoch: String) -> Result<(), JsValue> {
    PEERS.with(|peers| {
        let peer = peers
            .borrow()
            .get(&id)
            .cloned()
            .ok_or_else(|| error("closed peer"))?;
        if peer.closed.get() {
            return Err(error("closed peer"));
        }
        peer.batches.borrow_mut().bind_lease(&epoch).map_err(error)
    })
}
#[wasm_bindgen]
pub fn acknowledge_peer_storage_batch(id: u32, raw: String) -> Result<(), JsValue> {
    let result: BatchResult = serde_json::from_str(&raw).map_err(error)?;
    if result.epoch.is_empty()
        || result.epoch.len() > 128
        || !result.observed_at_ms.is_finite()
        || result.observed_at_ms < 0.0
    {
        return Err(error("invalid storage batch observation"));
    }
    PEERS.with(|peers| {
        let peer = peers
            .borrow()
            .get(&id)
            .cloned()
            .ok_or_else(|| error("closed peer"))?;
        let basis = peer
            .batches
            .borrow_mut()
            .settle(
                &result.batch_id,
                &result.epoch,
                if result.result.ok {
                    Ok(())
                } else {
                    Err(result
                        .result
                        .code
                        .clone()
                        .unwrap_or_else(|| "storage-transaction-failed".into()))
                },
            )
            .map_err(error)?;
        if result.result.ok {
            peer.committed_at.set(Some(result.observed_at_ms));
            peer.admission_progress.borrow_mut().committed(basis);
        }
        report_light(&peer);
        Ok(())
    })
}
fn required_read_failure(peer: &Peer, state: &StatePeer) -> Option<String> {
    crate::desktop::admission(state)
        .ok()?
        .required
        .iter()
        .find_map(|record| {
            state
                .check_document_read(
                    peer._key.verifying_key().to_bytes(),
                    &record.app_id,
                    &record.record_id,
                    SystemTime::now(),
                )
                .err()
        })
}
fn health(
    peer: &Peer,
    state: &StatePeer,
    ready: bool,
    reported_error: Option<&str>,
    source: crate::admission_health::ReportSource,
    outgoing_blocked: usize,
    awaiting_governance: bool,
) -> serde_json::Value {
    // Metadata and borrowed bytes only: copying the committed journal and every
    // record per report grew with the session (several reports per publication).
    let journal_key = StorageKey::Admission {
        workspace: state.room().into(),
    };
    let journal_status = peer.cache.status_meta(&journal_key);
    let journal = peer
        .cache
        .with_committed(&journal_key, |_, bytes| {
            peer.health_projection.borrow_mut().journal(&bytes.bytes)
        })
        .flatten();
    let durable_failure = crate::admission_health::durable_admission_failure(
        state,
        peer._key.verifying_key().to_bytes(),
        SystemTime::now(),
        peer.admission_failure.borrow().clone(),
        awaiting_governance,
    );
    let required_failure = crate::admission_health::permission_failure(
        required_read_failure(peer, state),
        awaiting_governance,
    );
    let durable_failure = durable_failure.or(required_failure);
    let failure = crate::admission_health::reported_failure(
        durable_failure.as_deref(),
        reported_error,
        source,
        ready && state.pending_count() == 0,
    );
    let mut failures=failure.map(|text|{
        let (code,message,recovery)=crate::admission_health::failure_policy(text,source,durable_failure.is_some(),peer.transport_connected.get());
        serde_json::json!({"id":"runtime:admission","code":code,"scope":{"kind":if !peer.transport_connected.get(){"connection"}else{"admission"}},"severity":if durable_failure.is_some()||matches!(source,crate::admission_health::ReportSource::ReceiveRejection){"error"}else{"notice"},"message":message,"recovery":[if recovery=="review-permissions"{serde_json::json!({"kind":recovery,"failureId":"runtime:admission"})}else{serde_json::json!({"kind":recovery})}],"occurredAtMs":js_sys::Date::now()})
    }).into_iter().collect::<Vec<_>>();
    if outgoing_blocked > 0 {
        failures.push(serde_json::json!({
            "id":"runtime:outgoing",
            "code":"checkpoint-expired",
            "scope":{"kind":"admission"},
            "severity":"notice",
            "message":crate::admission_health::OUTGOING_CHECKPOINT_MESSAGE,
            "recovery":[{"kind":"refresh-checkpoint"}],
            "occurredAtMs":js_sys::Date::now()
        }));
    }
    if let Ok(admission) = crate::desktop::admission(state) {
        for app in admission.unsupported {
            failures.push(serde_json::json!({"id":format!("runtime:application:{app}"),"code":"runtime-unavailable","scope":{"kind":"admission"},"severity":"notice","message":format!("{app}: application integration or required service unavailable"),"recovery":[],"occurredAtMs":js_sys::Date::now()}));
        }
    }
    let epoch = peer.batches.borrow().lease_epoch().map(str::to_owned);
    let revision = |revision: u64| {
        epoch
            .as_ref()
            .map(|epoch| serde_json::json!({"epoch":epoch,"revision":revision.to_string()}))
    };
    // Keys and sizes only: copying every queued document per report grew
    // with the session.
    let pending = peer.cache.pending_write_sizes();
    let records=state.rows().iter().map(|row|{
        let key=storage_key(state.room(),&row.app_id,&row.record_id);
        let status=peer.cache.status_meta(&key);
        let durable_heads=journal.as_ref().and_then(|journal| {
            let heads=peer.cache.committed_document_heads(&key)?.ok()?.iter().map(ToString::to_string).collect::<Vec<_>>();
            journal.match_document_heads(&row.app_id,&row.record_id,&heads)
        });
        let durable_basis=durable_heads.as_ref().map(|heads|serde_json::json!({"heads":heads}));
        let is_durable=durable_basis.is_some()&&durable_heads.as_ref()==Some(&row.heads);
        let failed=status.as_ref().is_some_and(|status|status.error.is_some())||journal_status.as_ref().is_some_and(|status|status.error.is_some());
        let coverage=state.durable_coverage_with_current(&row.app_id,&row.record_id).ok().flatten().filter(|(receipt,_)|journal.as_ref().is_some_and(|journal|journal.receipts.get(&qos_peer_state::coverage::route_key(&row.app_id,&row.record_id))==Some(&receipt.receipt_id)));
        let covered=is_durable&&coverage.as_ref().is_some_and(|(_,current)|*current);
        let receipt=coverage.map(|(receipt,_)|receipt);
        let outgoing=peer.outbox.borrow().iter().filter(|message|matches!(message,NeighbourMsg::AppDocument{app_id,record_id,..} if app_id==&row.app_id&&record_id==&row.record_id)).count();
        let durable_outgoing=journal.as_ref().map(|journal|journal.outbox.iter().filter(|message|matches!(message,NeighbourMsg::AppDocument{app_id,record_id,..} if app_id==&row.app_id&&record_id==&row.record_id)).count()).unwrap_or(0);
        serde_json::json!({"record":{"roomId":state.room(),"appId":row.app_id,"recordId":row.record_id},"admission":if state.check_document_read(peer._key.verifying_key().to_bytes(),&row.app_id,&row.record_id,SystemTime::now()).is_ok(){"admitted"}else{"unavailable"},"current":{"heads":row.heads},"editing":{"mode":if durable_failure.is_some(){"blocked"}else if peer.transport_connected.get(){"online"}else{"offline-queued"}},"local":{"state":if failed{"failed"}else if is_durable{"durable"}else{"pending"},"durableBasis":durable_basis,"recordRevision":status.as_ref().and_then(|status|status.committed_revision).and_then(|r|revision(r)),"journalRevision":journal_status.as_ref().and_then(|status|status.committed_revision).and_then(|r|revision(r)),"pendingWrites":pending.iter().filter(|(write_key,_)|*write_key==key||matches!(write_key,StorageKey::Admission{..})).count(),"pendingBytes":pending.iter().filter(|(write_key,_)|*write_key==key||matches!(write_key,StorageKey::Admission{..})).map(|(_,bytes)|*bytes).sum::<usize>(),"committedAtMs":peer.committed_at.get()},"outgoing":{"pendingOperations":outgoing,"durableOperations":durable_outgoing},"acknowledgement":{"state":if covered{"covered"}else if receipt.is_some(){"pending"}else{"unknown"},"targetPeerId":receipt.as_ref().map(|receipt|receipt.coverage.author.iter().map(|byte|format!("{byte:02x}")).collect::<String>()),"coveredBasis":receipt.as_ref().map(|receipt|serde_json::json!({"heads":receipt.coverage.heads})),"coversCurrentBasis":covered,"receiptId":receipt.map(|receipt|receipt.receipt_id),"observedAtMs":if covered{peer.committed_at.get()}else{None}},"failureIds":[]})
    }).collect::<Vec<_>>();
    let admission_phase =
        crate::admission_health::admission_phase(state, ready, durable_failure.as_deref());
    serde_json::json!({"version":1,"transport":{"phase":if peer.closed.get(){"closed"}else if peer.transport_connected.get(){"connected"}else{"offline"},"authenticated":peer.transport_connected.get(),"generation":peer.transport_generation.get().to_string(),"lastRelayContactAtMs":peer.last_relay_contact.get(),"lastSessionPeerContactAtMs":peer.last_peer_contact.get()},"admission":{"phase":admission_phase,"progressRevision":peer.admission_progress.borrow().revision().to_string(),"requiredRecords":crate::desktop::admission(state).map(|admission|admission.required.iter().map(|row|serde_json::json!({"roomId":state.room(),"appId":row.app_id,"recordId":row.record_id})).collect::<Vec<_>>()).unwrap_or_default(),"pendingPrerequisites":state.pending_count(),"deadlineAtMs":null},"records":records,"capabilities":{"automaticRecovery":true,"retrySaving":true,"recoverRenderer":true,"durablePeerAcknowledgement":true},"failures":failures})
}

/// Read-only handoff from the committed admission owner to the retained desktop.
pub(crate) fn desktop_state(id: u32) -> Result<StatePeer, String> {
    PEERS.with(|peers| {
        let peer = peers.borrow().get(&id).cloned().ok_or("closed peer")?;
        let state = peer.state.borrow();
        state.permissions(peer._key.verifying_key().to_bytes(), SystemTime::now())?;
        let admission = crate::desktop::admission(&state)?;
        if !admission.complete() {
            return Err("canonical desktop records are not admitted".into());
        }
        if let Some(reason) = required_read_failure(&peer, &state) {
            return Err(reason);
        }
        if let Some(reason) = state.incomplete_reason() {
            return Err(reason);
        }
        Ok(state.clone())
    })
}

/// One admission installation path for retained and immediate committed state.
fn install_candidate(peer: &Peer, mut candidate: StatePeer) -> Result<(), String> {
    let next_revision = peer
        .state_revision
        .get()
        .checked_add(1)
        .ok_or("canonical Peer revision exhausted")?;
    let (mut outbox, reconciliation_error) =
        match candidate.reconcile_outbox(&peer.outbox.borrow(), &peer._key, SystemTime::now()) {
            Ok(outbox) => (outbox, None),
            // Never discard newly accepted revocations/prerequisites
            // merely because local outgoing recovery is forbidden.
            Err(error) => (
                peer.outbox
                    .borrow()
                    .iter()
                    .filter(|message| !candidate.covers(message))
                    .cloned()
                    .collect(),
                Some(error),
            ),
        };
    // Acceptance may reconcile author sequences, but cannot retire the
    // immutable original without an exact designated durable receipt.
    for original in peer.outbox.borrow().iter() {
        if !candidate.outbox_message_retired(original)
            && !outbox.iter().any(|other| {
                postcard::to_allocvec(other).ok() == postcard::to_allocvec(original).ok()
            })
        {
            outbox.push(original.clone());
        }
    }
    let install = if candidate.incomplete_reason().is_some() {
        Ok(())
    } else {
        install_records(&peer, &candidate)
    };
    install?;
    stage_journal(peer, &candidate, &outbox)?;
    let journal = StorageKey::Admission {
        workspace: candidate.room().into(),
    };
    let revision = peer
        .cache
        .status_meta(&journal)
        .filter(|status| !status.durable)
        .map(|status| status.revision);
    peer.admission_progress.borrow_mut().accepted(revision);
    // A complete, unrefused admission (the steady state: every echo of this
    // peer's own publications) changes no phase; its rows and health are
    // refreshed by the full report at the commit boundary it now waits for
    // (`peer_committed`), exactly as for a local publication. Anything that
    // can change the phase or surface a failure still reports in full now.
    let steady = reconciliation_error.is_none()
        && candidate.incomplete_reason().is_none()
        && crate::desktop::admission(&candidate).is_ok_and(|admission| admission.complete());
    *peer.admission_failure.borrow_mut() = reconciliation_error;
    *peer.outbox.borrow_mut() = outbox;
    *peer.state.borrow_mut() = candidate;
    peer.state_revision.set(next_revision);
    crate::browser::refresh_peer_mounts(peer.id);
    if steady {
        report_light(peer);
    } else {
        report(peer, "admitting", None);
    }
    // LIVE_V1: rebase or promote forwarded changes over the durable install.
    live_api::after_durable_install(peer);
    Ok(())
}

fn action_context(peer: &Peer) -> Result<serde_json::Value, String> {
    if peer.closed.get() || !peer.transport_connected.get() {
        return Err("action transport disconnected".into());
    }
    if !peer.action_v2.get() {
        return Err("state-only peer: actions unavailable".into());
    }
    let state = peer.state.borrow();
    let target = state.authority();
    let sender = peer._key.verifying_key().to_bytes();
    state.permissions(target.device_public_key, SystemTime::now())?;
    state.permissions(sender, SystemTime::now())?;
    let own = state
        .certificates()
        .into_iter()
        .find(|cert| cert.device_public_key == sender)
        .ok_or("sender certificate missing")?;
    let hex = |bytes: [u8; 32]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    Ok(
        serde_json::json!({"room":state.room(),"sender":hex(sender),"senderRoot":hex(own.user_public_key),"target":hex(target.device_public_key),"targetRoot":hex(target.user_public_key),"certificate":serde_json::to_string(target).map_err(|e|e.to_string())?}),
    )
}
#[wasm_bindgen]
pub fn peer_action_notifications() -> bool {
    true
}
#[wasm_bindgen]
pub fn peer_action_context(id: u32) -> Result<String, JsValue> {
    let peer = PEERS
        .with(|peers| peers.borrow().get(&id).cloned())
        .ok_or_else(|| error("closed peer"))?;
    action_context(&peer)
        .map(|value| value.to_string())
        .map_err(error)
}
#[wasm_bindgen]
pub fn peer_action_request(id: u32, message: String) -> Result<String, JsValue> {
    use tilo_relay::peer_actions::{ActionMessage, DirectedAction};
    let peer = PEERS
        .with(|peers| peers.borrow().get(&id).cloned())
        .ok_or_else(|| error("closed peer"))?;
    action_context(&peer).map_err(error)?;
    let message: ActionMessage = serde_json::from_str(&message).map_err(error)?;
    if !matches!(
        message,
        ActionMessage::CatalogueRefresh { .. }
            | ActionMessage::InvokeBound { .. }
            | ActionMessage::Status { .. }
            | ActionMessage::Cancel { .. }
    ) {
        return Err(error("unsupported client request"));
    }
    let state = peer.state.borrow();
    let action = DirectedAction {
        room: state.room().parse().map_err(error)?,
        request_id: uuid::Uuid::new_v4(),
        source: peer._key.verifying_key().to_bytes(),
        target: state.authority().device_public_key,
        message,
    };
    let request = action.request_id;
    peer.actions
        .borrow_mut()
        .insert(action.clone())
        .map_err(error)?;
    let result = delivery_api::queue_action(&peer, action);
    if let Err(failure) = result {
        peer.actions.borrow_mut().forget(request);
        return Err(error(failure));
    }
    Ok(request.to_string())
}
#[wasm_bindgen]
pub fn peer_action_response(id: u32, request: String) -> Result<Option<String>, JsValue> {
    let peer = PEERS
        .with(|peers| peers.borrow().get(&id).cloned())
        .ok_or_else(|| error("closed peer"))?;
    action_context(&peer).map_err(error)?;
    peer.actions
        .borrow_mut()
        .take(request.parse().map_err(error)?)
        .map(|value| Ok(crate::action_client::browser_response(value).to_string()))
        .transpose()
}
#[wasm_bindgen]
pub fn peer_action_forget(id: u32, request: String) {
    if let (Some(peer), Ok(request)) = (
        PEERS.with(|peers| peers.borrow().get(&id).cloned()),
        request.parse(),
    ) {
        peer.actions.borrow_mut().forget(request);
    }
}
