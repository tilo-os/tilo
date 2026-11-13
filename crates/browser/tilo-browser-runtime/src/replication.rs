//! A certified workspace's documents in the page: every record a mount has open
//! and the workspace's desktop, replicated through the relay session in host
//! order.
//!
//! One [`Joined`] exists per started peer and joined workspace. It owns an event
//! queue created when the peer started, so documents the host replays at
//! admission are kept even before any mount exists. Mounts attach their
//! record stores (and desktop, if they present one):
//!
//! - **Records** go out as Automerge deltas on the route `(app, workspace,
//!   record)`, the native shell's own `AppDocument` wire; a record's first
//!   publication in a session is a snapshot, which the host retains as the
//!   route's checkpoint for peers that join later. What lands in this
//!   device's replica of the workspace ([`Event::Landed`]) is read from
//!   the session's engine and merges into the open record
//!   (`RecordStore::merge_replica`, Automerge semantics, local undo untouched)
//!   or, when no mount has it open, into the record's stored document so
//!   opening it later shows them.
//! - **The desktop** is the desk's own document
//!   ([`tilo_desktop_model::desktop_doc`]) on the route native peers and the
//!   workspace host use for it, the one the native desk keeps: every workspace,
//!   strip, group and window its own entry, so concurrent changes to
//!   different ones all land. The page writes only what differs from what
//!   the document holds, one change naming the person who made it, and an
//!   inbound change reaches the surface through `replace_shared_tree`.
//!
//! Nothing is published before the host answered `Ready` for the workspace, so a
//! page never announces a stale snapshot over what the workspace already holds.
//!
//! A package's workspace-primary document travels with an empty record, as the
//! native broker sends it; in the page's stores it is
//! [`RecordId::package_primary`], kept at the workspace index.
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use tilo_sort::CompactSort as _;

use collab_types::DocumentMutation;
use durable_store::{DurableStore as _, MemoryStore, StorageKey, WriteOp};
use neighbour_wire::content::ContentWants;
use tilo_automerge::ApplyIncoming as _;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_session::sync::{Cmd, Event, Outbox};
use tilo_session_client::engine::RecordReading;
use tilo_session_client::{Events, Link};
use ui::automerge::{AutoCommit, ReadDoc as _, transaction::Transactable as _};
use ui::collaboration::record_content::ContentLane;
use ui::record::{RecordId, RecordSync};
use ui::state::DocumentReplica;

use crate::replicated::{Attachment, DESKTOP_APP, DESKTOP_RECORD};
use tilo_relay_protocol::WorkspaceId;

/// How often a page asks again what the workspace lets its person do: a role the
/// workspace grants or withdraws while they are in it reaches their page within
/// this.
const MODERATION_ASK_EVERY_MS: u64 = 15_000;
/// How soon a page asks again when the workspace could not answer (the page was
/// between connections): what it knew stands meanwhile.
const MODERATION_RETRY_MS: u64 = 3_000;

thread_local! {
    static JOINED: RefCell<HashMap<u32, Rc<RefCell<Joined>>>> = RefCell::new(HashMap::new());
}

/// Hand the start module the page's replication (`crate::replicated`), which
/// asks it from a table of functions so that the engine this module reads the
/// workspace through loads with the peer's code, not with the page's first
/// frame. Called from `peer_sync::register`, when a peer starts.
pub(crate) fn install() {
    crate::replicated::install(crate::replicated::Replicated {
        page_store,
        attach,
        detach,
        publish,
        follow_answers,
        ask_content,
        device_of,
        committed_record,
        changes,
        record_document,
        content,
        permit,
        host_then,
        workspace_uuid,
        records,
        desktop_history_heads,
        desk_bytes,
    });
}

/// How a page sends its desktop to the workspace.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DesktopSending {
    /// Nothing sent yet: the next change goes as the whole document.
    Snapshot,
    /// The workspace has the document: changes go as deltas.
    Changes,
}

struct Mounted {
    attachment: Attachment,
    records: HashMap<(String, String), RecordSync>,
}

pub(crate) struct Joined {
    peer: Link,
    /// The certified device key, hex.
    device: String,
    events: Events,
    uuid: String,
    /// `uuid`, as the routes of the workspace's records name it.
    cluster: uuid::Uuid,
    workspace: WorkspaceId,
    store: Rc<MemoryStore>,
    /// Where `store` lives.
    kept: Kept,
    /// The content its mounts' windows keep, which members are answered from.
    content_store: tilo_content::ContentStore,
    ready: bool,
    /// The workspace sends this page a record only once it is opened (a
    /// public workspace): every record a mount opens is asked for, the
    /// copy this page kept from before included.
    when_opened: bool,
    /// The records not here that this page asked the workspace for, as wire
    /// `(app, record)` ([`ui::record::RecordStore::wanted_records`]).
    asked: HashSet<(String, String)>,
    mounts: HashMap<u64, Mounted>,
    desktop: DocumentReplica,
    /// How this page sends its desktop to the workspace.
    desktop_sending: DesktopSending,
    /// The workspace's desktop as this page last published or applied it.
    desktop_value: Option<serde_json::Value>,
    /// This page's own desktop as it last wrote it into the record: a
    /// frame that changed nothing shared writes nothing.
    local_value: Option<serde_json::Value>,
    /// The heads of each record the workspace delivered that no mount had open, until a mount
    /// opens it. The workspace has everything up to them, so what the mount sends
    /// first is what it changed after them, not the whole document: a
    /// checkpoint reaches only devices that lack the record, and the changes
    /// a window makes as it opens (taking a seat) would never reach the
    /// others.
    delivered: HashMap<(String, String), Vec<ui::automerge::ChangeHash>>,
    /// Changes this page made to the workspace: to its records, sent, and to its
    /// desktop's windows (opened, closed, moved), not the desktop it joined.
    authored: u64,
    /// Whether the workspace lets this page's person close and move what others
    /// made on the shared desktop: the attached desktop's flag, set from the
    /// workspace's answer.
    moderates: Arc<AtomicBool>,
    /// Whether the workspace lets this page's person invite others here: the
    /// attached desktop's flag, which its bar's invite button and its invite
    /// sheet follow, set from the workspace's answer.
    invitable: Arc<AtomicBool>,
    /// When the page next asks the workspace that (Unix milliseconds; 0: now).
    moderation_due_ms: Arc<std::sync::atomic::AtomicU64>,
    /// The role the workspace last answered this page's person holds: the
    /// attached desktop's, so its policy offers what the workspace allows.
    standing: crate::desktop::Standing,
    /// The standing the mounts' permissions were last set to.
    followed: u8,
    /// Nodes below the workspace the page's windows asked whether its person may
    /// change (an item someone shares), with the asked node's text form and
    /// the workspace's last answer. Those it allows beyond their role (an item
    /// shared with them to edit) are mirrored into the mounts' permissions;
    /// each is asked again with the person's role, so a share switched to
    /// view or ended stops letting their windows change it.
    permitted: BTreeMap<ui::permissions::TreePath, (String, bool)>,
    /// The workspace's answers about those nodes, not applied yet.
    permit_answers: Arc<Mutex<Vec<(String, bool)>>>,
    /// The workspace's members as its governance lists them, once heard: what
    /// someone no longer among them made on the desktop is released.
    members: Option<std::collections::BTreeSet<String>>,
    /// When the workspace admitted each of its people, once heard, by their person (root key,
    /// hex): who shares a name is numbered in this order.
    joined: std::collections::BTreeMap<String, u64>,
    /// Record routes whose commits this page published the workspace answered all of, since
    /// the last frame: what the record's windows said they did is said
    /// (`RecordStore::workspace_settled`).
    settled: Arc<Mutex<Vec<DocumentRoute>>>,
    /// Content the open records' windows lacked and the page asked the
    /// workspace for, by the wire `(app, record)` that references it.
    content: ContentWants<(String, String)>,
    /// The one stream of record changes this page feeds: what the session's activity and the
    /// person's mentions follow.
    changes: tilo_session::platform::records::Changes,
}

/// The page's storage key of wire record `record` of `app` in `workspace`.
fn storage_key(app: &str, workspace: &str, record: &str) -> Result<StorageKey, &'static str> {
    if record.is_empty() {
        return Ok(StorageKey::workspace_index(app, workspace));
    }
    let id = RecordId::parse(record).ok_or("not a record id")?;
    Ok(StorageKey::document(app, workspace, id.storage_key()))
}

/// Start replicating `workspace` for peer `id`. `events` must have been created
/// when the peer started.
pub(crate) fn open(
    id: u32,
    device: String,
    peer: Link,
    events: Events,
    workspace: &str,
    (store, kept): (Rc<MemoryStore>, Kept),
) -> Result<(), String> {
    let workspace_id =
        tilo_relay_protocol::WorkspaceId::parse(workspace).ok_or("a workspace must be a uuid")?;
    peer.send(Cmd::KeepContent(
        workspace_id,
        Arc::new(kept_content(id, workspace.to_owned())),
    ))?;
    let joined = Joined {
        peer,
        device,
        events,
        uuid: workspace.to_owned(),
        cluster: uuid::Uuid::parse_str(workspace).map_err(|_| "a workspace must be a uuid")?,
        workspace: workspace_id,
        store,
        kept,
        content_store: tilo_content::ContentStore::page_files(),
        ready: false,
        when_opened: false,
        asked: HashSet::new(),
        mounts: HashMap::new(),
        desktop: DocumentReplica::new(AutoCommit::new()),
        desktop_sending: DesktopSending::Snapshot,
        desktop_value: None,
        local_value: None,
        delivered: HashMap::new(),
        authored: 0,
        moderates: Arc::default(),
        invitable: Arc::default(),
        moderation_due_ms: Arc::default(),
        standing: crate::desktop::Standing::unknown(),
        followed: 0,
        permitted: BTreeMap::new(),
        permit_answers: Arc::default(),
        members: None,
        joined: BTreeMap::new(),
        settled: Arc::default(),
        content: ContentWants::default(),
        changes: changes(),
    };
    JOINED.with_borrow_mut(|replicas| replicas.insert(id, Rc::new(RefCell::new(joined))));
    pump(id);
    Ok(())
}

/// What peer `id`'s page answers a member asking for content of `workspace` from: any app surface
/// it carries or kept (a reader needs the version a record was written with), and the files records
/// of its open apps reference (the workspace's host asks for those to keep its own copy).
fn kept_content(
    id: u32,
    workspace: String,
) -> impl Fn(&str, &str, &str) -> Option<Vec<u8>> + 'static {
    move |app, cluster, hash| {
        if cluster != workspace {
            return None;
        }
        tilo_session_client::surfaces::read(app, hash).or_else(|| {
            let joined = replica_of(id)?;
            let joined = joined.try_borrow().ok()?;
            let open = joined.open_records().any(|(open, _)| open == app);
            open.then(|| joined.content_store.read(hash)).flatten()
        })
    }
}

/// How many changes to the workspace peer `id` made: records it sent, and
/// windows the person opened, closed or moved.
pub(crate) fn authored(id: u32) -> u64 {
    replica_of(id).map_or(0, |joined| joined.borrow().authored)
}

/// The certified device key (hex) of peer `id`, if it replicates a workspace.
pub(crate) fn device_of(id: u32) -> Option<String> {
    replica_of(id).map(|joined| joined.borrow().device.clone())
}

/// Whether peer `id`'s workspace holds record `record` (wire form) of `app`:
/// open in a mount, or delivered and stored for whoever opens it.
pub(crate) fn has_record(id: u32, app: &str, record: &str) -> bool {
    replica_of(id).is_some_and(|replica| {
        let joined = replica.borrow();
        joined.open_records().any(|(a, r)| a == app && r == record)
            || joined.peer.engine().holds_record(&joined.uuid, app, record)
    })
}

/// The workspace's records peer `id` holds, open in a mount or in its replica, as
/// wire `(app, record)`, sorted.
pub(crate) fn records(id: u32) -> Vec<(String, String)> {
    let Some(joined) = replica_of(id) else {
        return Vec::new();
    };
    let joined = joined.borrow();
    let mut records: BTreeSet<(String, String)> = joined
        .peer
        .engine()
        .records(&joined.uuid)
        .unwrap_or_default()
        .into_iter()
        .collect();
    records.extend(joined.open_records());
    records.into_iter().collect()
}

thread_local! {
    /// The one stream of record changes this page feeds.
    static CHANGES: tilo_session::platform::records::Changes =
        tilo_session::platform::records::Changes::default();
}

/// The one stream of record changes this page feeds, as the session's activity and the person's
/// mentions read it: every record this page's replication takes in or sends is noted there.
pub(crate) fn changes() -> tilo_session::platform::records::Changes {
    CHANGES.with(Clone::clone)
}

/// Wire record `record` of `app` as peer `id` holds it now: the open
/// record a mount has, else the workspace's replica of it.
pub(crate) fn record_document(id: u32, app: &str, record: &str) -> Option<AutoCommit> {
    let joined = replica_of(id)?;
    let joined = joined.borrow();
    for mounted in joined.mounts.values() {
        for store in mounted.attachment.stores.borrow().iter() {
            if store.app_and_home().0 != app {
                continue;
            }
            let Some(checkout) = store.open_records().into_iter().find(|checkout| {
                tilo_desktop_surface::host::records::wire_record(&checkout.id) == record
            }) else {
                continue;
            };
            if let Some(doc) = store.read(&checkout.id, AutoCommit::clone) {
                return Some(doc);
            }
        }
    }
    let bytes = joined
        .peer
        .engine()
        .record(&joined.uuid, app, record)
        .ok()??;
    AutoCommit::load(&bytes).ok()
}

/// The name of the workspace in front on peer `id`'s workspace desktop, once the
/// workspace delivered or the page published one.
pub(crate) fn workspace_name(id: u32) -> Option<String> {
    let joined = replica_of(id)?;
    let joined = joined.borrow();
    joined
        .desktop_value
        .as_ref()
        .and_then(tilo_desktop_surface::history_log::workspace_name)
}

/// The workspace's desk document, for the profile's copy of it: what the person's
/// desk is once the workspace has answered.
pub(crate) fn desk_bytes(id: u32) -> Option<Vec<u8>> {
    let joined = replica_of(id)?;
    let bytes = joined.borrow_mut().desktop.document_mut().save();
    (!bytes.is_empty()).then_some(bytes)
}

/// Read the retained desktop's frontier without cloning its whole history.
pub(crate) fn desktop_history_heads(id: u32) -> Option<Vec<ui::automerge::ChangeHash>> {
    let joined = replica_of(id)?;
    let joined = joined.borrow();
    let heads = joined.desktop.document().base_heads();
    (!heads.is_empty()).then_some(heads)
}

pub(crate) fn close(id: u32) {
    crate::feedback_code::forget_feedback_startup(id);
    crate::feedback_code::forget(id);
    JOINED.with_borrow_mut(|replicas| replicas.remove(&id));
}

/// The store peer `id` keeps the workspace's documents in when it is kept in
/// no profile: a mount through it opens its records from there, where
/// what the workspace delivers is kept, instead of from a profile of its own.
pub(crate) fn page_store(id: u32) -> Option<Rc<MemoryStore>> {
    let joined = replica_of(id)?;
    let joined = joined.borrow();
    (joined.kept == Kept::InPage).then(|| Rc::clone(&joined.store))
}

/// The content store every mount of peer `id`'s workspace lends its windows.
pub(crate) fn content(id: u32) -> Option<tilo_content::ContentStore> {
    replica_of(id).map(|joined| joined.borrow().content_store.clone())
}

/// Where a peer keeps the workspace's documents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kept {
    /// In the person's profile, with everything else of theirs.
    InProfile,
    /// Only in the page, for as long as it is open: a peer kept in no
    /// profile (a page's). Its mounts keep their records there too.
    InPage,
}

fn replica_of(id: u32) -> Option<Rc<RefCell<Joined>>> {
    JOINED.with_borrow(|replicas| replicas.get(&id).cloned())
}

/// The workspace peer `id` replicates, by its uuid.
pub(crate) fn workspace_uuid(id: u32) -> Option<String> {
    Some(replica_of(id)?.borrow().uuid.clone())
}

/// Wire record `record` of `app` in peer `id`'s workspace `workspace`, saved as
/// the workspace committed it: without edits still being typed.
pub(crate) fn committed_record(
    id: u32,
    workspace: &str,
    app: &str,
    record: &str,
) -> Option<Vec<u8>> {
    if workspace_uuid(id)? != workspace {
        return None;
    }
    let mut doc = record_document(id, app, record)?;
    if doc.pending_ops() > 0 {
        doc.rollback();
    }
    Some(doc.save())
}

/// Ask peer `id`'s workspace `request` through the host's workspaces door; `done`
/// gets its answer.
///
/// # Errors
/// The peer replicates no workspace, or its session is gone.
pub(crate) fn host_then(
    id: u32,
    request: crate::replicated::Ask,
    done: crate::replicated::Answer,
) -> Result<(), String> {
    let Some(joined) = replica_of(id) else {
        return Err(crate::replicated::NONE.to_owned());
    };
    let joined = joined.borrow();
    Ok(ask_then(&joined.peer, request(&joined.uuid), done)?)
}

/// Ask `peer`'s host `request` of its workspace administration, and hand `answered` the answer
/// once it comes.
fn ask_then(
    peer: &Link,
    request: tilo_relay_protocol::workspaces::WorkspacesRequest,
    answered: impl FnOnce(Result<tilo_relay_protocol::workspaces::WorkspacesReply, String>) + 'static,
) -> Result<(), tilo_session::sync::Closed> {
    let receipt = peer.ask_workspaces(request)?;
    wasm_bindgen_futures::spawn_local(async move {
        answered(
            receipt
                .await
                .unwrap_or_else(|_| Err("the session ended before the host answered".to_owned())),
        );
    });
    Ok(())
}

/// The workspace told peer `id`'s person whether they may change things at
/// `node` (its text form): their windows let them where the workspace will keep
/// it, and stop where it no longer would.
pub(crate) fn permit(id: u32, node: &str, may: bool) {
    let Some(joined) = replica_of(id) else {
        return;
    };
    let mut joined = joined.borrow_mut();
    if joined.note_permit(node, may) {
        joined.apply_standing();
        for mounted in joined.mounts.values() {
            (mounted.attachment.notify)();
        }
    }
}

/// Attach mount `mount` to peer `id`'s workspace.
pub(crate) fn attach(id: u32, mount: u64, attachment: Attachment) -> bool {
    let Some(joined) = replica_of(id) else {
        return false;
    };
    {
        let mut joined = joined.borrow_mut();
        if let (Some(value), Some((handle, shared))) = (&joined.desktop_value, &attachment.desktop)
            && let Err(error) = crate::desktop::apply_shared(handle, shared, value)
        {
            warn(&format!("shared desktop not applied: {error}"));
        }
        if let Some((_, shared)) = &attachment.desktop {
            shared.members.replace(joined.members.clone());
            joined.moderates = Arc::clone(&shared.moderates);
            joined.invitable = Arc::clone(&shared.invitable);
            joined.standing.share(&shared.standing);
        }
        joined.moderation_due_ms.store(0, Ordering::Relaxed);
        // The new mount's apps learn what the workspace answered.
        joined.followed = 0;
        joined.mounts.insert(
            mount,
            Mounted {
                attachment,
                records: HashMap::new(),
            },
        );
    }
    publish(id);
    true
}

pub(crate) fn detach(id: u32, mount: u64) {
    if let Some(joined) = replica_of(id) {
        publish(id);
        joined.borrow_mut().mounts.remove(&mount);
    }
}

/// Apply everything the host delivered since the last call.
pub(crate) fn pump(id: u32) {
    let Some(joined) = replica_of(id) else {
        return;
    };
    let notify = joined.borrow_mut().receive();
    for notify in notify {
        notify();
    }
    publish(id);
}

/// Send what attached mounts changed since the last call.
pub(crate) fn publish(id: u32) {
    if let Some(joined) = replica_of(id) {
        let mut joined = joined.borrow_mut();
        joined.send();
        joined.ask_content();
        // Every frame passes here, so the workspace is asked again on time
        // whether or not anything arrived from it.
        joined.ask_standing_when_due();
        joined.follow_standing();
    }
}

/// Take what the workspace answered about this page's person since the last
/// frame, before the frame decides whether it has anything to do: an
/// answer that changes what their windows may do marks them changed, so an
/// idle page draws it at once instead of whenever something else wakes it.
pub(crate) fn follow_answers(id: u32) {
    if let Some(joined) = replica_of(id) {
        joined.borrow_mut().follow_standing();
    }
}

/// Ask the workspace now for content the open records' windows lack.
pub(crate) fn ask_content(id: u32) {
    if let Some(joined) = replica_of(id) {
        joined.borrow_mut().ask_content();
    }
}

/// What a person reads when the workspace refused a change `app` made: that it
/// was not kept, and the workspace's reason in plain words.
fn refused_change(app: &str, plain: &str) -> String {
    let title = crate::desktop::applications()
        .into_iter()
        .find(|definition| definition.app_id == app)
        .map_or("The app", |definition| &*definition.title);
    format!("{title} didn't keep that. {plain}")
}

fn warn(message: &str) {
    web_sys::console::warn_1(&wasm_bindgen::JsValue::from_str(&format!(
        "Browser runtime replication: {message}"
    )));
}

impl Joined {
    /// Ask the workspace, at most every [`MODERATION_ASK_EVERY_MS`], what it
    /// lets this page's person do: whether they write, moderate (and so
    /// close and move what others made on its shared desktop) and govern
    /// it. The answers set the desktop's flag and the role the mounts' apps
    /// follow, and wake them.
    fn ask_standing_when_due(&self) {
        if !self.ready || self.mounts.is_empty() {
            return;
        }
        let now = crate::shell::now_ms();
        if now < self.moderation_due_ms.load(Ordering::Relaxed) {
            return;
        }
        self.moderation_due_ms.store(
            now.saturating_add(MODERATION_ASK_EVERY_MS),
            Ordering::Relaxed,
        );
        self.ask_permits();
        self.ask_invitable(now);
        let wake: Vec<_> = self
            .mounts
            .values()
            .map(|mounted| Arc::clone(&mounted.attachment.notify))
            .collect();
        let asked = tilo_action_policy::roles::telling_nodes(&self.uuid);
        let answers: Arc<Mutex<[Option<bool>; 3]>> = Arc::default();
        for (index, node) in asked.into_iter().enumerate() {
            let answers = Arc::clone(&answers);
            let moderates = Arc::clone(&self.moderates);
            let standing = self.standing.clone();
            let due = Arc::clone(&self.moderation_due_ms);
            let wake = wake.clone();
            let request = tilo_relay_protocol::workspaces::WorkspacesRequest::Permits {
                workspace: self.uuid.clone(),
                node: node.to_string(),
            };
            if let Err(error) = ask_then(&self.peer, request, move |answer| {
                let may = match &answer {
                    Ok(tilo_relay_protocol::workspaces::WorkspacesReply::Permits(may)) => *may,
                    Ok(other) => {
                        warn(&format!(
                            "the workspace did not say what this page may do: {other:?}"
                        ));
                        false
                    }
                    Err(_) => {
                        due.store(
                            crate::shell::now_ms().saturating_add(MODERATION_RETRY_MS),
                            Ordering::Relaxed,
                        );
                        return;
                    }
                };
                let answered = {
                    let mut answers = answers.lock().unwrap_or_else(PoisonError::into_inner);
                    if let Some(slot) = answers.get_mut(index) {
                        *slot = Some(may);
                    }
                    *answers
                };
                let [Some(writes), Some(moderating), Some(governs)] = answered else {
                    return;
                };
                let role = tilo_action_policy::roles::role_answered([writes, moderating, governs]);
                let moved = moderates.swap(moderating, Ordering::Relaxed) != moderating;
                if standing.set(Some(role)) || moved {
                    for wake in &wake {
                        wake();
                    }
                }
            }) {
                warn(&format!(
                    "could not ask the workspace what this page may do: {error}"
                ));
                self.moderation_due_ms
                    .store(now.saturating_add(MODERATION_RETRY_MS), Ordering::Relaxed);
                return;
            }
        }
    }

    /// Ask the workspace whether this page's person may invite others here:
    /// the node its governance decides inviting by. Its bar's invite button
    /// and its invite sheet offer a link only where the workspace says so,
    /// asked again with the rest of the standing, so a role granted while
    /// the page is open reaches it.
    fn ask_invitable(&self, now: u64) {
        let node = tilo_action_policy::roles::governance_action(
            &self.uuid,
            tilo_action_policy::roles::governance::INVITE,
        );
        let invitable = Arc::clone(&self.invitable);
        let due = Arc::clone(&self.moderation_due_ms);
        let wake: Vec<_> = self
            .mounts
            .values()
            .map(|mounted| Arc::clone(&mounted.attachment.notify))
            .collect();
        let request = tilo_relay_protocol::workspaces::WorkspacesRequest::Permits {
            workspace: self.uuid.clone(),
            node: node.to_string(),
        };
        if let Err(error) = ask_then(&self.peer, request, move |answer| {
            let may = match &answer {
                Ok(tilo_relay_protocol::workspaces::WorkspacesReply::Permits(may)) => *may,
                Ok(other) => {
                    warn(&format!(
                        "the workspace did not say whether this page may invite: {other:?}"
                    ));
                    return;
                }
                Err(_) => {
                    due.store(
                        crate::shell::now_ms().saturating_add(MODERATION_RETRY_MS),
                        Ordering::Relaxed,
                    );
                    return;
                }
            };
            if invitable.swap(may, Ordering::Relaxed) != may {
                for wake in &wake {
                    wake();
                }
            }
        }) {
            warn(&format!(
                "could not ask the workspace whether this page may invite: {error}"
            ));
            self.moderation_due_ms
                .store(now.saturating_add(MODERATION_RETRY_MS), Ordering::Relaxed);
        }
    }

    /// Hold the workspace's answer whether this page's person may change `node`
    /// (its text form); whether what their windows may do changed.
    fn note_permit(&mut self, node: &str, may: bool) -> bool {
        let Ok(path) = node.parse::<ui::permissions::TreePath>() else {
            return false;
        };
        let held = self
            .permitted
            .insert(path.within_workspace(), (node.to_owned(), may));
        match held {
            Some((_, before)) => before != may,
            None => may,
        }
    }

    /// Ask the workspace again about every node this page's windows asked of,
    /// with the person's role: what a share gives changes when it is
    /// switched or ends, whether or not the app that asked is running.
    fn ask_permits(&self) {
        for (node, _) in self.permitted.values() {
            let answers = Arc::clone(&self.permit_answers);
            let asked = node.clone();
            let wake: Vec<_> = self
                .mounts
                .values()
                .map(|mounted| Arc::clone(&mounted.attachment.notify))
                .collect();
            let request = tilo_relay_protocol::workspaces::WorkspacesRequest::Permits {
                workspace: self.uuid.clone(),
                node: node.clone(),
            };
            if let Err(error) = ask_then(&self.peer, request, move |answer| {
                if let Ok(tilo_relay_protocol::workspaces::WorkspacesReply::Permits(may)) = answer {
                    answers
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push((asked, may));
                    for wake in &wake {
                        wake();
                    }
                }
            }) {
                warn(&format!(
                    "could not ask the workspace what this page may change: {error}"
                ));
                return;
            }
        }
    }

    /// Once the workspace answered every write said so far, `route`'s windows say what they did.
    /// `false` when the session is gone.
    fn settle_when_durable(&self, route: DocumentRoute) -> bool {
        let settled = Arc::clone(&self.settled);
        let notify: Vec<_> = self
            .mounts
            .values()
            .map(|mounted| Arc::clone(&mounted.attachment.notify))
            .collect();
        let Ok(durable) = self.peer.barrier() else {
            return false;
        };
        wasm_bindgen_futures::spawn_local(async move {
            let _ = durable.await;
            settled
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(route);
            for notify in notify {
                notify();
            }
        });
        true
    }

    /// Give every mount's apps the role the workspace last answered, once per
    /// answer that changed it.
    fn follow_standing(&mut self) {
        let answers = std::mem::take(
            &mut *self
                .permit_answers
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        let mut changed = false;
        for (node, may) in answers {
            changed |= self.note_permit(&node, may);
        }
        if changed {
            self.apply_standing();
            for mounted in self.mounts.values() {
                (mounted.attachment.notify)();
            }
        }
        let standing = self.standing.code();
        if standing == self.followed {
            return;
        }
        self.apply_standing();
    }

    /// Set every mount's permissions to the role the workspace last answered,
    /// with what it permits beyond it.
    fn apply_standing(&mut self) {
        let Some(role) = self.standing.role() else {
            return;
        };
        self.followed = self.standing.code();
        for mounted in self.mounts.values() {
            let attachment = &mounted.attachment;
            attachment
                .permissions
                .set(self.permitted.iter().filter(|(_, (_, may))| *may).fold(
                    ui::permissions::PermissionContext::member(attachment.author.clone(), role),
                    |context, (node, _)| {
                        context.permitted_at(node, ui::permissions::KindSet::UNDOABLE)
                    },
                ));
            // Its windows sync again, so what they offer (a bar's "View
            // only", an editor that takes typing) follows at once rather
            // than at the next change of their record.
            attachment.changed.set(true);
            // A window waiting to learn whether its person starts the
            // workspace's record now knows.
            if let Some((handle, shared)) = &attachment.desktop {
                shared.retry_waiting(handle);
            }
        }
    }

    /// What `value`, this page's desktop, closes, moves or renames on the
    /// workspace's desktop that its person did not make and may not moderate:
    /// the workspace refuses such a write, so it is never sent. Said in words
    /// the person reads; `None` when it touches nothing of anyone else's.
    fn rearranges_others(&self, value: &serde_json::Value, author: &str) -> Option<String> {
        if self.moderates.load(Ordering::Relaxed) {
            return None;
        }
        let before_value = self.desktop_value.as_ref()?;
        let parse = |value: &serde_json::Value| {
            serde_json::from_value::<tilo_desktop_model::ownership::SharedDesktop>(value.clone())
                .ok()
        };
        let (Some(before), Some(after)) = (parse(before_value), parse(value)) else {
            return None;
        };
        let acts = tilo_desktop_model::ownership::foreign_acts(
            &before,
            &after,
            author,
            self.members.as_ref(),
        );
        let first = acts.first()?;
        warn(&format!("undid a change to what others made: {acts:?}"));
        Some(
            tilo_desktop_model::commands::Refusal::Others {
                id: first.id,
                owner: first.owner.clone(),
            }
            .to_string(),
        )
    }

    /// Every record a mount has open in this workspace, as wire `(app, record)`.
    /// The route record `record` (its wire name) of app `app` travels on in
    /// this workspace: its content lanes are this workspace's alone.
    fn content_route(&self, app: &str, record: &str) -> ui::collaboration::RouteSpec {
        ui::collaboration::RouteSpec::record(app, self.cluster, record)
    }

    fn open_records(&self) -> impl Iterator<Item = (String, String)> + '_ {
        self.mounts.values().flat_map(|mounted| {
            let stores = mounted.attachment.stores.borrow();
            stores
                .iter()
                .filter(|store| store.app_and_home().1 == self.uuid)
                .flat_map(|store| {
                    let app = store.app_and_home().0;
                    store.open_records().into_iter().map(move |checkout| {
                        (
                            app.clone(),
                            tilo_desktop_surface::host::records::wire_record(&checkout.id)
                                .to_owned(),
                        )
                    })
                })
                .collect::<Vec<_>>()
        })
    }

    fn route(&self, app: &str, record: &str) -> DocumentRoute {
        DocumentRoute {
            app_id: app.to_owned(),
            cluster_id: self.uuid.clone(),
            record: record.to_owned(),
        }
    }

    /// Apply inbound events; returns the mounts to wake.
    ///
    /// A late join or a reconnect lands a record's whole history at once: each record that
    /// landed is read from the replica and merged once per frame, however many writes it took.
    fn receive(&mut self) -> Vec<Arc<dyn Fn() + Send + Sync>> {
        let mut woken: HashSet<u64> = HashSet::new();
        let mut landed: BTreeSet<DocumentRoute> = BTreeSet::new();
        let mut refusals: Vec<(DocumentRoute, Vec<[u8; 32]>, String)> = Vec::new();
        let mut opened: Vec<DocumentRoute> = Vec::new();
        let mut content_arrived = false;
        for event in self.events.drain() {
            match event {
                Event::HostReady {
                    ws: workspace_id,
                    records_when_opened,
                    ..
                } if workspace_id == self.workspace => {
                    self.ready = true;
                    self.when_opened = records_when_opened;
                    self.moderation_due_ms.store(0, Ordering::Relaxed);
                    // A frame asks what this person may do here, and only now
                    // may it ask: every mount draws once more, so a page that
                    // drew and went idle before the workspace was ready (one
                    // app full tab, untouched) does not stay read-only.
                    woken.extend(self.mounts.keys().copied());
                }
                Event::Landed { route, .. } if route.cluster_id == self.uuid => {
                    landed.insert(route);
                }
                Event::RecordContent {
                    ws: workspace_id,
                    frame,
                    ..
                } if workspace_id == self.workspace => {
                    content_arrived |= self.content_piece(&frame);
                }
                Event::Refused {
                    route,
                    changes,
                    reason,
                } if route.cluster_id == self.uuid => {
                    refusals.push((route, changes, reason));
                }
                Event::RecordOpened {
                    ws: workspace_id,
                    route: Some(route),
                    frames: 0,
                    ..
                } if workspace_id == self.workspace => opened.push(route),
                _ => {}
            }
        }
        self.follow_members(&mut woken);
        // Withdrawn before what arrived since is merged: a peer's change can
        // never build on one the workspace refused.
        for (route, changes, reason) in refusals {
            self.withdraw(&route, &changes, &reason, &mut woken);
        }
        let settled =
            std::mem::take(&mut *self.settled.lock().unwrap_or_else(PoisonError::into_inner));
        for route in settled {
            self.settle(&route, &mut woken);
        }
        // Records first: a desktop naming a new tab's record finds it.
        let mut landed: Vec<DocumentRoute> = landed.into_iter().collect();
        landed.compact_sort_by_key(|route| route.app_id == DESKTOP_APP);
        for route in landed {
            // A record kept for whoever opens it is read as what changed since the store took
            // it last: a record that lands a write at a time (a sealed chat's history, written
            // by many) would otherwise be read, loaded and saved whole once per write.
            let kept = self.kept_heads(&route);
            let read = self.peer.engine().record_update(
                &route.cluster_id,
                &route.app_id,
                &route.record,
                kept.as_deref(),
            );
            let applied = match read {
                Ok(RecordReading::Captured { bytes, heads, .. }) => {
                    self.apply(&route, (bytes, &heads), &mut woken)
                }
                Ok(RecordReading::Changes { bytes, heads, .. }) => {
                    self.apply_kept_changes(&route, bytes, &heads, &mut woken)
                }
                // Still arriving (its keys, or the checkpoint it builds on): it lands again.
                Ok(RecordReading::Unchanged(_) | RecordReading::Pending) => Ok(()),
                Err(error) => Err(error),
            };
            if let Err(error) = applied {
                warn(&format!("{}/{}: {error}", route.app_id, route.record));
            }
        }
        for route in opened {
            self.workspace_answered(&route, &mut woken);
        }
        self.wakes(woken, content_arrived)
    }

    /// What wakes the mounts in `woken`, each marked changed, and every
    /// mount besides when content arrived.
    fn wakes(
        &self,
        woken: HashSet<u64>,
        content_arrived: bool,
    ) -> Vec<Arc<dyn Fn() + Send + Sync>> {
        let mut notify: Vec<Arc<dyn Fn() + Send + Sync>> = woken
            .into_iter()
            .filter_map(|mount| self.mounts.get(&mount))
            .map(|mounted| {
                mounted.attachment.changed.set(true);
                Arc::clone(&mounted.attachment.notify)
            })
            .collect();
        // Content changes no record: its windows draw what they waited for.
        if content_arrived {
            notify.extend(
                self.mounts
                    .values()
                    .map(|mounted| Arc::clone(&mounted.attachment.notify)),
            );
        }
        notify
    }

    /// Ask the workspace for the content open records' windows lack: what they
    /// queued on the records' lanes ([`ContentLane`]). Content a window
    /// stored for others is not offered from a page.
    fn ask_content(&mut self) {
        if !self.ready {
            return;
        }
        let device = self.peer.status().device;
        let now = crate::shell::now_ms();
        let records: BTreeSet<(String, String)> = self.open_records().collect();
        for (app, record) in records {
            // The record's windows each queue on a lane of their own.
            for lane in ContentLane::on_route(&self.content_route(&app, &record)) {
                for hash in lane.take_requests().wants {
                    let Some(request) = self.content.ask(
                        &device,
                        (app.clone(), record.clone()),
                        (&app, &self.uuid),
                        &hash,
                        now,
                    ) else {
                        continue;
                    };
                    if self
                        .peer
                        .send(Cmd::WantContent(self.workspace, request))
                        .is_err()
                    {
                        lane.retry_want(hash);
                    }
                }
            }
        }
    }

    /// Keep a piece of content a member sent; whether it completed content
    /// a record's window asked for, now on the record's lane.
    fn content_piece(&mut self, frame: &[u8]) -> bool {
        let device = self.peer.status().device;
        let Some(arrived) = self
            .content
            .piece(&device, frame, tilo_clock::unix_now_ms())
        else {
            return false;
        };
        let (app, record) = arrived.tag;
        for lane in ContentLane::on_route(&self.content_route(&app, &record)) {
            lane.arrive(arrived.hash.clone(), arrived.bytes.clone());
        }
        true
    }

    /// Withdraw what the host refused of `route` from the open record that
    /// sent it, which tells its app, and move the record's replication
    /// frontier back to what it keeps. The raw `reason` is for the log; the
    /// app shows a person its plain form.
    fn withdraw(
        &mut self,
        route: &DocumentRoute,
        changes: &[[u8; 32]],
        reason: &str,
        woken: &mut HashSet<u64>,
    ) {
        warn(&format!(
            "{}/{}: the workspace refused a change: {reason}",
            route.app_id, route.record
        ));
        if route.app_id == DESKTOP_APP {
            return;
        }
        if changes.is_empty() {
            return;
        }
        let refused: Vec<ui::automerge::ChangeHash> = changes
            .iter()
            .map(|hash| ui::automerge::ChangeHash(*hash))
            .collect();
        let plain = tilo_relay_protocol::refusal::plain_reason(reason);
        for (&id, mounted) in &mut self.mounts {
            for store in mounted.attachment.stores.borrow().iter() {
                if store.app_and_home().0 != route.app_id {
                    continue;
                }
                let Some(checkout) = store.open_records().into_iter().find(|checkout| {
                    tilo_desktop_surface::host::records::wire_record(&checkout.id) == route.record
                }) else {
                    continue;
                };
                match store.withdraw_refused(&checkout.id, &refused, plain) {
                    Ok(withdrawn) => {
                        if let Some(heads) = withdrawn {
                            // What the person just did is taken back: say so
                            // over the desktop, whatever the app shows, so a
                            // change the workspace refused never looks kept.
                            if let Some((handle, _)) = &mounted.attachment.desktop {
                                handle.acknowledge(refused_change(&route.app_id, plain));
                            }
                            if let Some(replica) = mounted
                                .records
                                .get_mut(&(route.app_id.clone(), route.record.clone()))
                            {
                                replica.advance(heads);
                            }
                        }
                        woken.insert(id);
                    }
                    Err(error) => warn(&format!(
                        "{}/{}: a refused change stays: {error}",
                        route.app_id, route.record
                    )),
                }
            }
        }
    }

    /// The host retains nothing of `route`, a record this page opened or
    /// asked for: each store holding or wanting it tells what reads it
    /// ([`ui::record::RecordStore::note_workspace_answered`]), and a window
    /// that waited for it is built again, now on a new record.
    fn workspace_answered(&self, route: &DocumentRoute, woken: &mut HashSet<u64>) {
        if route.cluster_id != self.uuid {
            return;
        }
        for (&id, mounted) in &self.mounts {
            let mut answered = false;
            for store in mounted.attachment.stores.borrow().iter() {
                if store.app_and_home().0 != route.app_id {
                    continue;
                }
                let record = store
                    .open_records()
                    .into_iter()
                    .map(|checkout| checkout.id)
                    .chain(store.wanted_records())
                    .find(|record| {
                        tilo_desktop_surface::host::records::wire_record(record) == route.record
                    });
                if let Some(record) = record {
                    store.note_workspace_answered(record);
                    answered = true;
                }
            }
            if answered {
                if let Some((handle, shared)) = &mounted.attachment.desktop {
                    shared.retry_waiting(handle);
                }
                woken.insert(id);
            }
        }
    }

    /// The workspace answered every commit of `route` this page published: what the record's
    /// windows said they did is theirs to say, a refused change withdrawn first, with what was
    /// said of it.
    fn settle(&self, route: &DocumentRoute, woken: &mut HashSet<u64>) {
        for (&id, mounted) in &self.mounts {
            for store in mounted.attachment.stores.borrow().iter() {
                if store.app_and_home().0 != route.app_id {
                    continue;
                }
                if let Some(checkout) = store.open_records().into_iter().find(|checkout| {
                    tilo_desktop_surface::host::records::wire_record(&checkout.id) == route.record
                }) {
                    store.workspace_settled(&checkout.id);
                    woken.insert(id);
                }
            }
        }
    }

    /// Merge `bytes`, the replica's whole document of `route`, standing at `heads`, into the
    /// open record, the desktop, or the record's stored document for whoever opens it.
    fn apply(
        &mut self,
        route: &DocumentRoute,
        (bytes, heads): (Vec<u8>, &[[u8; 32]]),
        woken: &mut HashSet<u64>,
    ) -> Result<(), String> {
        if route.app_id == DESKTOP_APP {
            return self.apply_desktop(&bytes, woken);
        }
        let app = route.app_id.clone();
        let record = route.record.clone();
        // What reaches the record is news only once the workspace has replayed what it already
        // held: before that it is what the device had.
        self.changes.note(&self.uuid, &app, &record, !self.ready);
        let mut merged = false;
        for (&id, mounted) in &mut self.mounts {
            for store in mounted.attachment.stores.borrow().iter() {
                if store.app_and_home().0 != app {
                    continue;
                }
                let Some(checkout) = store.open_records().into_iter().find(|checkout| {
                    tilo_desktop_surface::host::records::wire_record(&checkout.id) == record
                }) else {
                    continue;
                };
                let replica = mounted
                    .records
                    .entry((app.clone(), record.clone()))
                    .or_default();
                if replica.merge(store, &checkout, DocumentMutation::Snapshot(bytes.clone()))? {
                    woken.insert(id);
                }
                merged = true;
            }
        }
        if merged {
            return Ok(());
        }
        // The host takes up a record a member created with an empty
        // document of its own before the member's content arrives: no
        // record yet, which a page opening it would refuse.
        let Some(heads) = self.store_unopened(&app, &record, bytes, heads)? else {
            return Ok(());
        };
        self.kept_for_opening(&app, record, heads, woken);
        Ok(())
    }

    /// The heads the store holds `route` at for whoever opens it, when no mount has it open
    /// and the store took it from the replica: what a read of it need only bring the changes
    /// since. `None` for the desktop, an open record, or one the store does not hold.
    fn kept_heads(&self, route: &DocumentRoute) -> Option<Vec<[u8; 32]>> {
        if route.app_id == DESKTOP_APP || self.is_open(&route.app_id, &route.record) {
            return None;
        }
        let heads = self
            .delivered
            .get(&(route.app_id.clone(), route.record.clone()))?;
        let mut heads: Vec<[u8; 32]> = heads.iter().map(|head| head.0).collect();
        heads.sort_unstable();
        Some(heads)
    }

    /// Whether a mount has `record` of `app` open.
    fn is_open(&self, app: &str, record: &str) -> bool {
        self.mounts.values().any(|mounted| {
            mounted.attachment.stores.borrow().iter().any(|store| {
                store.app_and_home().0 == app
                    && store.open_records().into_iter().any(|checkout| {
                        tilo_desktop_surface::host::records::wire_record(&checkout.id) == record
                    })
            })
        })
    }

    /// Keep `changes`, what changed of `route` since the heads the store held it at
    /// ([`Self::kept_heads`]), after what the store holds: the record then stands at `heads`
    /// for whoever opens it, and nothing of it is loaded to get there.
    fn apply_kept_changes(
        &mut self,
        route: &DocumentRoute,
        changes: Vec<u8>,
        heads: &[[u8; 32]],
        woken: &mut HashSet<u64>,
    ) -> Result<(), String> {
        let (app, record) = (route.app_id.clone(), route.record.clone());
        self.changes.note(&self.uuid, &app, &record, !self.ready);
        let key = storage_key(&app, &self.uuid, &record)?;
        let writer = self
            .store
            .read(&key)
            .map_err(|error| error.to_string())?
            .and_then(|stored| stored.writer);
        self.store
            .write(&key, WriteOp::Append(changes), writer.as_deref())
            .map_err(|error| error.to_string())?;
        let heads = heads
            .iter()
            .copied()
            .map(ui::automerge::ChangeHash)
            .collect();
        self.kept_for_opening(&app, record, heads, woken);
        Ok(())
    }

    /// The store holds `record` of `app` at `heads` for whoever opens it: the mounts look
    /// again.
    fn kept_for_opening(
        &mut self,
        app: &str,
        record: String,
        heads: Vec<ui::automerge::ChangeHash>,
        woken: &mut HashSet<u64>,
    ) {
        self.delivered.insert((app.to_owned(), record), heads);
        // A window of the app that follows what its store keeps (a chain's
        // newer head) takes it up at its next frame.
        for mounted in self.mounts.values() {
            for store in mounted.attachment.stores.borrow().iter() {
                if store.app_and_home().0 == app {
                    store.note_kept();
                }
            }
        }
        // Every mount looks again: a desktop builds the windows that waited
        // for it, and a page showing one app builds the app whose record it
        // waited for.
        for (&id, mounted) in &self.mounts {
            if let Some((handle, shared)) = &mounted.attachment.desktop {
                shared.retry_waiting(handle);
            }
            woken.insert(id);
        }
    }

    /// Merge the replica's document `bytes`, which stands at `heads`, into the record's stored
    /// document for whoever opens it next, and answer its heads once it holds any change. Into
    /// an empty store it is kept as it came, unloaded: opening the record loads and checks it
    /// once. A document with no change in it is no record yet and is not kept.
    fn store_unopened(
        &self,
        app: &str,
        record: &str,
        bytes: Vec<u8>,
        heads: &[[u8; 32]],
    ) -> Result<Option<Vec<ui::automerge::ChangeHash>>, String> {
        let key = storage_key(app, &self.uuid, record)?;
        let stored = self.store.read(&key).map_err(|error| error.to_string())?;
        let writer = stored.as_ref().and_then(|stored| stored.writer.clone());
        let stored = stored
            .map(|stored| stored.bytes)
            .filter(|bytes| !bytes.is_empty());
        let Some(stored) = stored else {
            if heads.is_empty() {
                return Ok(None);
            }
            let heads = heads
                .iter()
                .copied()
                .map(ui::automerge::ChangeHash)
                .collect();
            return self
                .store
                .write(&key, WriteOp::Replace(bytes), writer.as_deref())
                .map(|_| Some(heads))
                .map_err(|error| error.to_string());
        };
        let mut doc = AutoCommit::load(&stored).map_err(|error| error.to_string())?;
        let incoming = AutoCommit::load(&bytes).map_err(|error| error.to_string())?;
        let before = doc.get_heads();
        let shared: Vec<_> = before
            .iter()
            .copied()
            .filter(|head| incoming.get_change_by_hash(head).is_some())
            .collect();
        let mut incoming = incoming;
        doc.load_incoming(&incoming.save_after(&shared))
            .map_err(|error| error.to_string())?;
        let heads = doc.get_heads();
        if heads.is_empty() {
            return Ok(None);
        }
        if heads == before {
            return Ok(Some(heads));
        }
        self.store
            .write(&key, WriteOp::Replace(doc.save()), writer.as_deref())
            .map(|_| Some(heads))
            .map_err(|error| error.to_string())
    }

    fn apply_desktop(&mut self, bytes: &[u8], woken: &mut HashSet<u64>) -> Result<(), String> {
        self.desktop
            .merge_snapshot(bytes)
            .map_err(|error| error.to_string())?;
        // The workspace holds its desktop: what this page sends of it from now on is
        // what this page changed, never the whole document. A whole document carries
        // every member's changes, and the workspace decides each change it does not
        // hold yet as this page's person's own: a window someone else opened would
        // read as one this person attributed to them.
        self.desktop_sending = DesktopSending::Changes;
        let Some(value) = desktop_value(self.desktop.document()) else {
            return Ok(());
        };
        if self.desktop_value.as_ref() == Some(&value) {
            return Ok(());
        }
        // A peer renaming the workspace is news to everyone else here; the
        // first desktop this page receives is not.
        let renamed = tilo_desktop_surface::history_log::WorkspaceRename::merged(
            self.desktop.document(),
            self.desktop_value.as_ref(),
            &value,
        );
        for (&id, mounted) in &self.mounts {
            if let Some((handle, shared)) = &mounted.attachment.desktop {
                crate::desktop::apply_shared(handle, shared, &value)?;
                if let Some(rename) = &renamed {
                    shared.renames.borrow_mut().push(rename.clone());
                }
                woken.insert(id);
            }
        }
        self.desktop_value = Some(value);
        Ok(())
    }

    fn send(&mut self) {
        if !self.ready {
            return;
        }
        let mut outgoing: Vec<Pending> = Vec::new();
        let mut opening: Vec<DocumentRoute> = Vec::new();
        for (&id, mounted) in &mut self.mounts {
            for store in mounted.attachment.stores.borrow().iter() {
                let (app, home) = store.app_and_home();
                if home != self.uuid {
                    continue;
                }
                for checkout in store.open_records() {
                    let key = (
                        app.clone(),
                        tilo_desktop_surface::host::records::wire_record(&checkout.id).to_owned(),
                    );
                    // A record a join does not replay is asked for when a
                    // mount first opens it.
                    if !mounted.records.contains_key(&key)
                        && (self.when_opened
                            || neighbour_wire::record_sync(&key.1)
                                != neighbour_wire::RecordSync::Eager)
                    {
                        opening.push(DocumentRoute {
                            app_id: app.clone(),
                            cluster_id: self.uuid.clone(),
                            record: key.1.clone(),
                        });
                    }
                    let delivered = &mut self.delivered;
                    let replica = mounted.records.entry(key.clone()).or_insert_with(|| {
                        match delivered.remove(&key) {
                            // The workspace has the record as it delivered it.
                            Some(heads) => RecordSync::delivered(heads),
                            None => RecordSync::new(),
                        }
                    });
                    let route = DocumentRoute {
                        app_id: app.clone(),
                        cluster_id: self.uuid.clone(),
                        record: key.1.clone(),
                    };
                    match replica.take_outbound(store, &checkout.id) {
                        Ok(Some(next)) => {
                            // A window here wrote the record: the session's activity and the
                            // person's mentions read it as any other change of it.
                            self.changes
                                .note(&self.uuid, &route.app_id, &route.record, false);
                            outgoing.push((route, next.mutation, (id, key, next.heads)));
                        }
                        Ok(None) => {}
                        Err(error) => warn(&format!("{}/{}: {error}", key.0, key.1)),
                    }
                }
            }
        }
        opening.extend(self.wanted_routes());
        for route in opening {
            if !self
                .asked
                .insert((route.app_id.clone(), route.record.clone()))
            {
                continue;
            }
            if self
                .peer
                .send(Cmd::OpenRecordAt(self.workspace, route))
                .is_err()
            {
                return;
            }
        }
        // Records first: a desktop naming a new tab's record must not reach
        // a peer before the record does.
        for (route, mutation, advance) in outgoing {
            // A first snapshot carries what was there already; an
            // increment is what this page changed.
            let authored = matches!(mutation, DocumentMutation::Incremental(_));
            if self.peer.write(route.clone(), &mutation).is_err() {
                return;
            }
            if !self.settle_when_durable(route) {
                return;
            }
            if authored {
                self.authored = self.authored.saturating_add(1);
            }
            let (id, key, heads) = advance;
            if let Some(replica) = self
                .mounts
                .get_mut(&id)
                .and_then(|m| m.records.get_mut(&key))
            {
                replica.advance(heads);
            }
        }
        self.send_desktop();
    }

    /// The records windows opened that are not here and that this page has
    /// not asked the workspace for yet: it sends each, or says it has none
    /// of it, and only then is it new.
    fn wanted_routes(&self) -> Vec<DocumentRoute> {
        let mut wanted = Vec::new();
        for mounted in self.mounts.values() {
            for store in mounted.attachment.stores.borrow().iter() {
                let (app, home) = store.app_and_home();
                if home != self.uuid {
                    continue;
                }
                for record in store.wanted_records() {
                    // A workspace that replays every chain's head answered
                    // for each with its replay: what this page holds of a
                    // chain is all of its head there is.
                    if record.is_chain_head() && !self.when_opened {
                        if self.ready {
                            store.note_workspace_answered(record);
                        }
                        continue;
                    }
                    let key = (
                        app.clone(),
                        tilo_desktop_surface::host::records::wire_record(&record).to_owned(),
                    );
                    if !self.asked.contains(&key) {
                        wanted.push(DocumentRoute {
                            app_id: key.0,
                            cluster_id: self.uuid.clone(),
                            record: key.1,
                        });
                    }
                }
            }
        }
        wanted
    }

    /// The workspace's members and when it admitted each of them, as the peer last heard its
    /// governance say: the attached desktops release what someone who left made, and number
    /// people who share a name in the workspace's own order.
    fn follow_members(&mut self, woken: &mut HashSet<u64>) {
        let status = self.peer.status();
        let members = status.room_members.get(&self.uuid).cloned();
        let joined = status
            .room_joined
            .get(&self.uuid)
            .cloned()
            .unwrap_or_default();
        if members == self.members && joined == self.joined {
            return;
        }
        self.members = members;
        self.joined = joined;
        for (&id, mounted) in &self.mounts {
            if let Some((_, shared)) = &mounted.attachment.desktop {
                shared.members.replace(self.members.clone());
                shared.joined.replace(self.joined.clone());
                woken.insert(id);
            }
        }
    }

    fn send_desktop(&mut self) {
        let Some((desktop, author)) = self.mounts.values().find_map(|mounted| {
            mounted.attachment.desktop.as_ref().map(|(handle, shared)| {
                (
                    crate::desktop::shared_value(handle, shared),
                    mounted.attachment.author.clone(),
                )
            })
        }) else {
            return;
        };
        // The desk as it reads to a peer: what it says moved is what the
        // workspace decides on, and an unchanged desk writes nothing.
        let value = serde_json::to_value(&desktop).unwrap_or(serde_json::Value::Null);
        if self.local_value.as_ref() == Some(&value) {
            return;
        }
        if let Some(refusal) = self.rearranges_others(&value, &author) {
            // Whatever moved someone else's window that the desktop did not
            // refuse first (a race with their own move) is undone here: it
            // springs back to where the workspace has it, nothing is sent, and
            // the person is told why.
            if let Some(before_value) = &self.desktop_value {
                for mounted in self.mounts.values() {
                    if let Some((handle, shared)) = &mounted.attachment.desktop {
                        if let Err(error) = crate::desktop::return_to(handle, shared, before_value)
                        {
                            warn(&format!("shared desktop not restored: {error}"));
                        }
                        handle.acknowledge(refusal.clone());
                        (mounted.attachment.notify)();
                    }
                }
            }
            return;
        }
        // The desktop this page joined with is not something the person did.
        let joined = self.desktop_value.is_some();
        let doc = self.desktop.document_mut();
        // What differs is written into the desk's own document, as one change
        // stamped with the time and who made it, which the history pane tells
        // by and every peer reads back as the desk.
        let wrote = tilo_desktop_model::desktop_doc::write_shared(
            doc,
            &desktop.tree,
            &desktop.records,
            &desktop.authors,
        );
        if wrote {
            tilo_desktop_surface::history_log::commit(doc, &author, ui::time::unix_now_i64());
        }
        if wrote && joined {
            // Windows opened, closed, moved or renamed: the person's own
            // work on the desktop.
            self.authored = self.authored.saturating_add(1);
        }
        if !wrote && self.desktop_sending == DesktopSending::Changes {
            self.local_value = Some(value);
            return;
        }
        let mutation = if self.desktop_sending == DesktopSending::Changes {
            let (delta, heads) = self.desktop.pending_outbound_delta();
            if delta.is_empty() {
                return;
            }
            (DocumentMutation::Incremental(delta), heads)
        } else {
            let doc = self.desktop.document_mut();
            (DocumentMutation::Snapshot(doc.save()), doc.get_heads())
        };
        let route = self.route(DESKTOP_APP, DESKTOP_RECORD);
        if self.peer.write(route, &mutation.0).is_ok() {
            self.desktop.advance_outbound(mutation.1);
            self.desktop_sending = DesktopSending::Changes;
            self.desktop_value = desktop_value(self.desktop.document());
            self.local_value = Some(value);
        }
    }
}

/// A publication and the replica frontier it advances once accepted.
type Pending = (
    DocumentRoute,
    DocumentMutation,
    (u64, (String, String), Vec<ui::automerge::ChangeHash>),
);

/// Who wrote the newest change of the workspace's desktop, as its message names
/// them (`history_log::commit`).
fn desktop_value(doc: &AutoCommit) -> Option<serde_json::Value> {
    tilo_desktop_model::shared_record::read(doc, None)
        .and_then(|desktop| serde_json::to_value(desktop).ok())
}
