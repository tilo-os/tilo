//! A certified workspace's documents in the page: every record a mount has open
//! and the workspace's desktop, replicated through the server session in host
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
//!   (`ResourceStore::merge_replica`, Automerge semantics, local undo untouched)
//!   or, when no mount has it open, into the record's stored document so
//!   opening it later shows them.
//! - **The desktop** is the layout's own document
//!   ([`tilo_desktop_model::desktop_doc`]) on the route native peers and the
//!   workspace host use for it, the one the native shell keeps: every workspace,
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
//! [`ui::resource::DocumentId::package_primary`], kept at the workspace index.
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use tilo_sort::CompactSort as _;

use collab_types::DocumentMutation;
use durable_store::{DurableStore as _, MemoryStore, WriteOp};
use neighbour_wire::content::ContentWants;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_session::sync::{Cmd, Event, Outbox};
use tilo_session_client::engine::ResourceReading;
use tilo_session_client::{DocumentAnswer, Events, Link};
use ui::automerge::{AutoCommit, transaction::Transactable as _};
use ui::collaboration::document_content::ContentLink;
use ui::resource::{DocumentSync, WorkspaceAnswer};
use ui::state::DocumentReplica;

use crate::replicated::{Attachment, DESKTOP_APP, DESKTOP_DOCUMENT};
use tilo_server_protocol::WorkspaceId;

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
        permits_due,
        follow_answers,
        ask_content,
        device_of,
        committed_record,
        changes,
        resource_document,
        record_since,
        record_heads,
        ask_resource,
        resource_answer,
        reads_alone: crate::browser::Mount::answer_reads_alone,
        content,
        permit,
        host_then,
        workspace_uuid,
        records,
        desktop_history_heads,
        layout_bytes,
        publish_view,
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
    documents: HashMap<(String, String), DocumentSync>,
    /// The heads, sorted, at which the workspace last read each record this mount has open, as
    /// the mount took them in: everything up to them is in its replica, so the next read brings
    /// only what changed since, not the record whole.
    merged: HashMap<(String, String), Vec<[u8; 32]>>,
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
    /// `(app, record)` ([`ui::resource::ResourceStore::wanted_resources`]).
    asked: HashSet<(String, String)>,
    /// Whether the workspace sent anything of each document this page asked it for, once it
    /// answered. Until it answered, and until what it sent is merged, a window's copy of the
    /// document is not sent whole as this page's own write ([`crate::landing::sends_whole`]).
    opened: HashMap<(String, String), bool>,
    mounts: HashMap<u64, Mounted>,
    desktop: DocumentReplica,
    /// How this page sends its desktop to the workspace.
    desktop_sending: DesktopSending,
    /// The workspace's desktop as this page last published or applied it.
    desktop_value: Option<serde_json::Value>,
    /// This page's own desktop as it last wrote it into the record: a
    /// frame that changed nothing shared writes nothing.
    local_value: Option<serde_json::Value>,
    /// The person's view of this workspace's layout as they left it
    /// ([`tilo_desktop_model::person_view`]): theirs on every device of theirs.
    view: DocumentReplica,
    /// How this page sends its view to the workspace.
    view_sending: DesktopSending,
    /// The person's view of this workspace's layout as this page last showed it from the
    /// workspace's copy: what the person changed here since is theirs, and stays over it.
    view_shown: Option<tilo_desktop_model::person_view::PersonView>,
    /// Whether `view` was read from the workspace: until it is, the page shows the view its
    /// profile kept.
    view_read: bool,
    /// The person at this page, whose view of the layout `view` holds.
    person: Option<String>,
    /// The heads of each record the workspace delivered that no mount had open, until a mount
    /// opens it. The workspace has everything up to them, so what the mount sends
    /// first is what it changed after them, not the whole document: a
    /// checkpoint reaches only devices that lack the record, and the changes
    /// a window makes as it opens (taking an admission) would never reach the
    /// others.
    delivered: HashMap<(String, String), Vec<ui::automerge::ChangeHash>>,
    /// The heads each record stood at when the workspace last delivered it, as the page read it:
    /// where a reader of the page's records starts to ask what changed since.
    read_heads: HashMap<(String, String), Vec<[u8; 32]>>,
    /// The resources a reader on this page named that it held none of, as wire `(app, document)`:
    /// each is asked of the workspace as one a window opens is, so it arrives and its changes
    /// follow though no mount ever opens it.
    read_wanted: HashSet<(String, String)>,
    /// What the workspace answered of a resource it sent this page none of: it holds none of
    /// it, or it is another person's own. Let go once the resource lands here.
    read_answers: HashMap<(String, String), WorkspaceAnswer>,
    /// Records whose read waits for the page's record worker to verify them
    /// (`crate::record_verify`), and how many verifications had finished
    /// then: once more have, they are read again.
    verifying: (BTreeSet<DocumentRoute>, u64),
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
    /// Whether the workspace lets this page's person make administrators:
    /// the attached desktop's flag, which its invite sheet's administrator
    /// choice follows.
    administers: Arc<AtomicBool>,
    /// When the page next asks the workspace that (Unix milliseconds; 0: now).
    moderation_due_ms: Arc<std::sync::atomic::AtomicU64>,
    /// The role the workspace last answered this page's person holds: the
    /// attached desktop's, so its policy offers what the workspace allows.
    role: crate::desktop::SharedRole,
    /// The member access the mounts' permissions were last set to.
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
    /// What the workspace's governance calls each of its people, by their person (root key, hex).
    names: std::collections::BTreeMap<String, String>,
    /// Record routes whose commits this page published the workspace answered all of, since
    /// the last frame: what the record's windows said they did is said
    /// (`ResourceStore::workspace_settled`).
    settled: Arc<Mutex<Vec<DocumentRoute>>>,
    /// Content the open records' windows lacked and the page asked the
    /// workspace for, by the wire `(app, record)` that references it.
    content: ContentWants<(String, String)>,
    /// The one stream of record changes this page feeds: what the session's activity and the
    /// person's mentions follow.
    changes: tilo_session::platform::records::Changes,
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
        tilo_server_protocol::WorkspaceId::parse(workspace).ok_or("a workspace must be a uuid")?;
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
        opened: HashMap::new(),
        mounts: HashMap::new(),
        desktop: DocumentReplica::new(AutoCommit::new()),
        desktop_sending: DesktopSending::Snapshot,
        desktop_value: None,
        local_value: None,
        view: DocumentReplica::new(AutoCommit::new()),
        view_sending: DesktopSending::Snapshot,
        view_shown: None,
        view_read: false,
        person: None,
        delivered: HashMap::new(),
        read_heads: HashMap::new(),
        read_wanted: HashSet::new(),
        read_answers: HashMap::new(),
        verifying: (BTreeSet::new(), 0),
        authored: 0,
        moderates: Arc::default(),
        invitable: Arc::default(),
        administers: Arc::default(),
        moderation_due_ms: Arc::default(),
        role: crate::desktop::SharedRole::unknown(),
        followed: 0,
        permitted: BTreeMap::new(),
        permit_answers: Arc::default(),
        members: None,
        names: BTreeMap::new(),
        settled: Arc::default(),
        content: ContentWants::default(),
        changes: changes(),
    };
    // The large records it reads are verified in the page's record worker.
    crate::record_verify::install();
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
            let open = joined.open_resources().any(|(open, _)| open == app);
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
        joined
            .open_resources()
            .any(|(a, r)| a == app && r == record)
            || joined
                .peer
                .engine()
                .holds_resource(&joined.uuid, app, record)
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
        .documents(&joined.uuid)
        .unwrap_or_default()
        .into_iter()
        .collect();
    records.extend(joined.open_resources());
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

/// Wire document `wire` of `app` as a window of this page holds it open, if one does.
fn open_resource(id: u32, app: &str, wire: &str) -> Option<AutoCommit> {
    let joined = replica_of(id)?;
    let joined = joined.borrow();
    for mounted in joined.mounts.values() {
        for store in mounted.attachment.stores.borrow().iter() {
            if store.app_and_home().0 != app {
                continue;
            }
            let Some(checkout) = store.open_resources().into_iter().find(|checkout| {
                tilo_desktop_surface::host::records::wire_document(&checkout.id) == wire
            }) else {
                continue;
            };
            if let Some(doc) = store.read(&checkout.id, AutoCommit::clone) {
                return Some(doc);
            }
        }
    }
    None
}

/// Wire document `wire` of `app` as peer `id` holds it now: the open
/// document a mount has, else the workspace's replica of it.
pub(crate) fn resource_document(id: u32, app: &str, wire: &str) -> Option<AutoCommit> {
    if let Some(doc) = open_resource(id, app, wire) {
        return Some(doc);
    }
    let joined = replica_of(id)?;
    let joined = joined.borrow();
    let bytes = joined
        .peer
        .engine()
        .record(&joined.uuid, app, wire)
        .ok()??;
    tilo_automerge::shared::load(&bytes).ok()
}

/// What changed of `wire` of `app` since `heads`, as the workspace holds it: its changes alone
/// where it holds those heads, and the document whole otherwise.
pub(crate) fn record_since(
    id: u32,
    app: &str,
    wire: &str,
    heads: &[ui::automerge::ChangeHash],
) -> tilo_session::platform::records::Since {
    use tilo_session::platform::records::Since;
    let Some(joined) = replica_of(id) else {
        return Since::Whole;
    };
    let held = sorted_heads(&heads.iter().map(|head| head.0).collect::<Vec<_>>());
    if held.is_empty() {
        return Since::Whole;
    }
    // A window here holds this document open: what it holds answers, without opening its sealed
    // history again for what is already on this page. A person's edit reads here before it
    // paints, and a document of many notes opens once, not once per reader.
    if let Some(mut doc) = open_resource(id, app, wire) {
        let base = held
            .iter()
            .copied()
            .map(ui::automerge::ChangeHash)
            .collect::<Vec<_>>();
        if base.iter().all(|head| doc.has_change(head)) {
            let mut at = doc.get_heads();
            at.sort();
            if at == base {
                return Since::Unchanged;
            }
            let mut bytes = Vec::new();
            for change in doc.get_changes(&base) {
                bytes.extend_from_slice(change.raw_bytes());
            }
            if !bytes.is_empty() {
                return Since::Changes { heads: at, bytes };
            }
        }
    }
    let joined = joined.borrow();
    match joined
        .peer
        .engine()
        .record_update(&joined.uuid, app, wire, Some(&held))
    {
        Ok(ResourceReading::Changes { heads, bytes, .. }) => Since::Changes {
            heads: heads.into_iter().map(ui::automerge::ChangeHash).collect(),
            bytes,
        },
        Ok(ResourceReading::Unchanged(_) | ResourceReading::Pending) => Since::Unchanged,
        Ok(ResourceReading::Captured { .. }) | Err(_) => Since::Whole,
    }
}

/// The heads the workspace last delivered `record` of `app` at, as the page read it.
pub(crate) fn record_heads(
    id: u32,
    app: &str,
    record: &str,
) -> Option<Vec<ui::automerge::ChangeHash>> {
    let joined = replica_of(id)?;
    let joined = joined.borrow();
    let heads = joined
        .read_heads
        .get(&(app.to_owned(), record.to_owned()))?;
    Some(
        heads
            .iter()
            .copied()
            .map(ui::automerge::ChangeHash)
            .collect(),
    )
}

/// Ask peer `id`'s workspace for `wire` of `app`, which a reader on this page named and the page
/// holds none of: by the command a window's open of it sends, once, so the workspace sends what
/// it holds of it and every change to it from then on. What the workspace answered of one it
/// sent none of; `None` while the document or the answer is on its way.
pub(crate) fn ask_resource(id: u32, app: &str, wire: &str) -> Option<WorkspaceAnswer> {
    let joined = replica_of(id)?;
    // A read is answered on a mount's own pass: a replication busy elsewhere is asked on the next.
    let mut joined = joined.try_borrow_mut().ok()?;
    let key = (app.to_owned(), wire.to_owned());
    if let Some(answer) = joined.read_answers.get(&key) {
        return Some(*answer);
    }
    if joined.read_wanted.insert(key) {
        joined.open_read_wanted();
    }
    None
}

/// What peer `id`'s workspace answered of `wire` of `app` when it sent this page none of it.
pub(crate) fn resource_answer(id: u32, app: &str, wire: &str) -> Option<WorkspaceAnswer> {
    let joined = replica_of(id)?;
    let joined = joined.try_borrow().ok()?;
    joined
        .read_answers
        .get(&(app.to_owned(), wire.to_owned()))
        .copied()
}

/// The role the workspace last answered the person of peer `id` holds, as
/// [`crate::desktop::SharedRole::code`] counts it: 0 until it has, and for a page that joined
/// no workspace.
pub(crate) fn role_code(id: u32) -> u8 {
    replica_of(id).map_or(0, |joined| joined.borrow().role.code())
}

/// Who mount `mount` of peer `id` shows its workspace to, and what the workspace lets them do
/// there: what the mount attached with. `None` for a mount this replication does not hold.
pub(crate) fn reader(id: u32, mount: u64) -> Option<(String, ui::permissions::SharedPermissions)> {
    let joined = replica_of(id)?;
    let joined = joined.borrow();
    let mounted = joined.mounts.get(&mount)?;
    Some((
        mounted.attachment.author.clone(),
        mounted.attachment.permissions.clone(),
    ))
}

/// The name the workspace in front is called on peer `id`: its governance's, which every device
/// and every list of it reads.
pub(crate) fn workspace_name(id: u32) -> Option<String> {
    let joined = replica_of(id)?;
    let joined = joined.borrow();
    joined.peer.engine().workspace_name(joined.workspace)
}

/// How far the governance this page holds has moved: what it holds of its workspaces changed when
/// this did.
pub(crate) fn governance_revision(id: u32) -> Option<u64> {
    let joined = replica_of(id)?;
    let joined = joined.borrow();
    Some(joined.peer.engine().owner().revision())
}

/// What `workspace`'s governance says of its people, as this page holds it: who is in it, in
/// what part and since when, which the page projects onto its shell as every device does.
pub(crate) fn governance_policy(
    id: u32,
    workspace: WorkspaceId,
) -> Option<tilo_action_policy::WorkspacePolicy> {
    let joined = replica_of(id)?;
    let joined = joined.borrow();
    joined.peer.engine().policy(&workspace.to_string()).ok()
}

/// `workspace`'s people as its governance names them, as this page holds it: each with the name
/// they gave themselves, which the page's shell names someone who is not here by.
pub(crate) fn governance_people(
    id: u32,
    workspace: WorkspaceId,
) -> Option<Vec<collab_types::Member>> {
    let joined = replica_of(id)?;
    let joined = joined.borrow();
    joined.peer.engine().people(&workspace.to_string()).ok()
}

/// The name `workspace`'s governance records, as this page holds it: the workspace's one name,
/// which every device and every list of it reads.
pub(crate) fn governance_name(id: u32, workspace: WorkspaceId) -> Option<String> {
    let joined = replica_of(id)?;
    let joined = joined.borrow();
    joined.peer.engine().workspace_name(workspace)
}

/// The workspace's layout document, for the profile's copy of it: what the person's
/// layout is once the workspace has answered.
pub(crate) fn layout_bytes(id: u32) -> Option<Vec<u8>> {
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
    let mut doc = resource_document(id, app, record)?;
    if doc.pending_ops() > 0 {
        doc.rollback();
    }
    Some(doc.save())
}

/// Ask peer `id`'s workspace `request` through the host's workspaces API; `done`
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
    request: tilo_server_protocol::workspaces::WorkspacesRequest,
    answered: impl FnOnce(Result<tilo_server_protocol::workspaces::WorkspacesReply, String>) + 'static,
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
        joined.apply_role();
        for mounted in joined.mounts.values() {
            (mounted.attachment.notify)();
        }
    }
}

/// When peer `id`'s page must ask the workspace again what its person may do
/// there, in Unix milliseconds (`0` is now), or `None` for a peer no mount is
/// attached to. The page's loop wakes for it: the question is asked as part of
/// a frame's pass, and a page whose own people and windows change nothing
/// draws none, so an idle page would otherwise wait for someone else's change
/// to hear that its person was promoted.
pub(crate) fn permits_due(id: u32) -> Option<u64> {
    let joined = replica_of(id)?;
    let joined = joined.borrow();
    (!joined.mounts.is_empty()).then(|| joined.moderation_due_ms.load(Ordering::Relaxed))
}

/// Write `tree`, the layout a mount shows, as the person's view of the workspace's layout in their
/// own view: every device of theirs reads it there.
pub(crate) fn publish_view(id: u32, tree: &tilo_desktop_model::Layout) {
    if let Some(joined) = replica_of(id) {
        joined.borrow_mut().write_view(tree);
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
        if let Some((handle, _)) = &attachment.desktop {
            if joined.view_read {
                joined.show_view(handle);
            } else if joined.view_shown.is_none()
                && let Some(workspace) = tilo_desktop_model::WorkspaceId::parse(&joined.uuid)
            {
                // What this page shows before the person's view reaches it: a change they make
                // from here is theirs over it.
                joined.view_shown = Some(tilo_desktop_model::person_view::PersonView::of(
                    &handle.read(Clone::clone),
                    workspace,
                ));
            }
        }
        if let Some((_, shared)) = &attachment.desktop {
            shared.members.replace(joined.members.clone());
            shared.names.replace(joined.names.clone());
            joined.moderates = Arc::clone(&shared.moderates);
            joined.invitable = Arc::clone(&shared.invitable);
            joined.administers = Arc::clone(&shared.administers);
            joined.role.share(&shared.role);
        }
        joined.moderation_due_ms.store(0, Ordering::Relaxed);
        // The new mount's apps learn what the workspace answered.
        joined.followed = 0;
        // Every mount of a page is the one person's, whose view of the layout is theirs.
        joined.person = Some(attachment.author.clone());
        joined.mounts.insert(
            mount,
            Mounted {
                attachment,
                documents: HashMap::new(),
                merged: HashMap::new(),
            },
        );
        joined.follow_link();
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

/// The page's record worker finished a verification: every replica reads the
/// records that waited for it.
pub(crate) fn verified() {
    let ids: Vec<u32> = JOINED.with_borrow(|replicas| replicas.keys().copied().collect());
    for id in ids {
        pump(id);
    }
}

/// Send what attached mounts changed since the last call.
pub(crate) fn publish(id: u32) {
    if let Some(joined) = replica_of(id) {
        let mut joined = joined.borrow_mut();
        joined.send();
        joined.ask_content();
        // Every frame passes here, so the workspace is asked again on time
        // whether or not anything arrived from it.
        joined.ask_role_when_due();
        joined.follow_role();
    }
}

/// Take what the workspace answered about this page's person since the last
/// frame, before the frame decides whether it has anything to do: an
/// answer that changes what their windows may do marks them changed, so an
/// idle page draws it at once instead of whenever something else wakes it.
pub(crate) fn follow_answers(id: u32) {
    if let Some(joined) = replica_of(id) {
        joined.borrow_mut().follow_role();
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
    fn ask_role_when_due(&self) {
        if !self.ready || self.mounts.is_empty() {
            // A workspace the page has not joined yet has nothing to answer:
            // ask again shortly, rather than on every pass of the page's loop.
            self.moderation_due_ms.store(
                crate::shell::now_ms().saturating_add(MODERATION_RETRY_MS),
                Ordering::Relaxed,
            );
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
        self.ask_flag(
            &tilo_action_policy::roles::governance_action(
                &self.uuid,
                tilo_action_policy::roles::governance::INVITE,
            ),
            &self.invitable,
            now,
        );
        self.ask_flag(
            &tilo_action_policy::roles::administrators_node(&self.uuid),
            &self.administers,
            now,
        );
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
            let shared_role = self.role.clone();
            let due = Arc::clone(&self.moderation_due_ms);
            let wake = wake.clone();
            let request = tilo_server_protocol::workspaces::WorkspacesRequest::Permits {
                workspace: self.uuid.clone(),
                node: node.to_string(),
            };
            if let Err(error) = ask_then(&self.peer, request, move |answer| {
                let may = match &answer {
                    Ok(tilo_server_protocol::workspaces::WorkspacesReply::Permits(may)) => *may,
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
                if shared_role.set(Some(role)) || moved {
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
    /// asked again with the rest of the role, so a role granted while
    /// the page is open reaches it.
    ///
    /// Whether they may make administrators is asked the same way, of the
    /// node the workspace decides that by, into `flag`.
    fn ask_flag(&self, node: &tilo_action_policy::TreePath, flag: &Arc<AtomicBool>, now: u64) {
        let invitable = Arc::clone(flag);
        let due = Arc::clone(&self.moderation_due_ms);
        let wake: Vec<_> = self
            .mounts
            .values()
            .map(|mounted| Arc::clone(&mounted.attachment.notify))
            .collect();
        let request = tilo_server_protocol::workspaces::WorkspacesRequest::Permits {
            workspace: self.uuid.clone(),
            node: node.to_string(),
        };
        if let Err(error) = ask_then(&self.peer, request, move |answer| {
            let may = match &answer {
                Ok(tilo_server_protocol::workspaces::WorkspacesReply::Permits(may)) => *may,
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
            let request = tilo_server_protocol::workspaces::WorkspacesRequest::Permits {
                workspace: self.uuid.clone(),
                node: node.clone(),
            };
            if let Err(error) = ask_then(&self.peer, request, move |answer| {
                if let Ok(tilo_server_protocol::workspaces::WorkspacesReply::Permits(may)) = answer
                {
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
    fn follow_role(&mut self) {
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
            self.apply_role();
            for mounted in self.mounts.values() {
                (mounted.attachment.notify)();
            }
        }
        let role = self.role.code();
        if role == self.followed {
            return;
        }
        self.apply_role();
    }

    /// Set every mount's permissions to the role the workspace last answered,
    /// with what it permits beyond it.
    fn apply_role(&mut self) {
        let Some(role) = self.role.role() else {
            return;
        };
        self.followed = self.role.code();
        for mounted in self.mounts.values() {
            let attachment = &mounted.attachment;
            attachment
                .permissions
                .set(self.permitted.iter().filter(|(_, (_, may))| *may).fold(
                    ui::permissions::PermissionContext::member(
                        &ui::permissions::MemberId::person(attachment.author.clone()),
                        role,
                    ),
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
    /// this workspace: its content content links are this workspace's alone.
    fn content_route(&self, app: &str, record: &str) -> ui::collaboration::RouteSpec {
        ui::collaboration::RouteSpec::document(app, self.cluster, record)
    }

    fn open_resources(&self) -> impl Iterator<Item = (String, String)> + '_ {
        self.mounts.values().flat_map(|mounted| {
            let stores = mounted.attachment.stores.borrow();
            stores
                .iter()
                .filter(|store| store.app_and_home().1 == self.uuid)
                .flat_map(|store| {
                    let app = store.app_and_home().0;
                    store.open_resources().into_iter().map(move |checkout| {
                        (
                            app.clone(),
                            tilo_desktop_surface::host::records::wire_document(&checkout.id)
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
            document: record.to_owned(),
        }
    }

    /// Apply inbound events; returns the mounts to wake.
    ///
    /// A late join or a reconnect lands a record's whole history at once: each record that
    /// landed is read from the replica and merged once per frame, however many writes it took.
    #[allow(
        clippy::too_many_lines,
        reason = "one pass over a batch: every kind of event lands, and the mounts it woke go back together"
    )]
    fn receive(&mut self) -> Vec<Arc<dyn Fn() + Send + Sync>> {
        let mut woken: HashSet<u64> = HashSet::new();
        let mut landed: BTreeSet<DocumentRoute> = BTreeSet::new();
        let mut refusals = Vec::new();
        let mut opened: Vec<(DocumentRoute, WorkspaceAnswer)> = Vec::new();
        let mut content_arrived = false;
        for event in self.events.drain() {
            match event {
                Event::HostReady {
                    ws: workspace_id,
                    documents_when_opened,
                    ..
                } if workspace_id == self.workspace => {
                    self.ready = true;
                    self.when_opened = documents_when_opened;
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
                Event::DocumentContent {
                    ws: workspace_id,
                    frame,
                    ..
                } if workspace_id == self.workspace => {
                    content_arrived |= self.content_piece(&frame);
                }
                Event::Refused {
                    route,
                    changes,
                    refusal,
                } if route.cluster_id == self.uuid => {
                    refusals.push((route, changes, refusal));
                }
                Event::DocumentOpened {
                    ws: workspace_id,
                    route: Some(route),
                    answer,
                    ..
                } if workspace_id == self.workspace => {
                    // A workspace that holds nothing of the person's view yet leaves this page's
                    // view the one the person changes, so the page writes it from then on.
                    if answered_without_frames(answer).is_some()
                        && self.person.as_deref().is_some_and(|person| {
                            tilo_desktop_model::person_view::person_of(&route.document)
                                == Some(person)
                        })
                    {
                        self.view_read = true;
                        self.view_sending = DesktopSending::Snapshot;
                    }
                    let key = (route.app_id.clone(), route.document.clone());
                    let sent = answered_without_frames(answer).is_none();
                    self.opened.insert(key, sent);
                    // What it sent landed before its answer, or the replica held all of it
                    // already and nothing landed: either way the windows with it open take it in
                    // now, which tells them what the workspace holds.
                    if sent && route.cluster_id == self.uuid {
                        landed.insert(route.clone());
                    }
                    opened.extend(answered_without_frames(answer).map(|said| (route, said)));
                }
                _ => {}
            }
        }
        self.follow_members(&mut woken);
        self.follow_link();
        // Withdrawn before what arrived since is merged: a peer's change can
        // never build on one the workspace refused.
        for (route, changes, refusal) in refusals {
            self.withdraw(&route, &changes, &refusal, &mut woken);
        }
        let settled =
            std::mem::take(&mut *self.settled.lock().unwrap_or_else(PoisonError::into_inner));
        for route in settled {
            self.settle(&route, &mut woken);
        }
        // A record verified since its read waited for that lands again.
        if self.verifying.1 != tilo_automerge_isolate::away::verified() {
            landed.append(&mut self.verifying.0);
        }
        for route in read_order(landed) {
            // A record kept for whoever opens it is read as what changed since the store took
            // it last: a record that lands a write at a time (a sealed chat's history, written
            // by many) would otherwise be read, loaded and saved whole once per write.
            let Some(read) = self.read_landed(&route) else {
                continue;
            };
            // What reaches a record is news only once the workspace replayed what it held, and
            // only once this page holds the record: the first that reaches it (a record the
            // workspace sends when a window opens it) is what the workspace already held.
            let key = (route.app_id.clone(), route.document.clone());
            let held = self.read_heads.contains_key(&key);
            let replayed = !self.ready || !held;
            if let Ok(
                ResourceReading::Captured { heads, .. } | ResourceReading::Changes { heads, .. },
            ) = &read
            {
                // It is here: what the workspace answered while it sent none of it stands no
                // longer, and a mount whose reader named it answers that reader now.
                self.read_answers.remove(&key);
                if self.read_wanted.contains(&key) {
                    woken.extend(self.mounts.keys().copied());
                }
                self.read_heads.insert(key, heads.clone());
            }
            let applied = match read {
                Ok(ResourceReading::Captured { bytes, heads, .. }) => {
                    self.apply(&route, (bytes, &heads), replayed, &mut woken)
                }
                Ok(ResourceReading::Changes { bytes, heads, .. })
                    if self.is_open(&route.app_id, &route.document) =>
                {
                    self.apply_open_changes(&route, &bytes, &heads, replayed, &mut woken)
                }
                Ok(ResourceReading::Changes { bytes, heads, .. }) => {
                    self.apply_kept_changes(&route, bytes, &heads, replayed, &mut woken)
                }
                // The workspace holds the windows' copy as it is: they send what they change
                // after it.
                Ok(ResourceReading::Unchanged(_)) => {
                    self.hold_open(&route);
                    Ok(())
                }
                // Still arriving (its keys, or the checkpoint it builds on): it lands again.
                Ok(ResourceReading::Pending) => Ok(()),
                Err(error) => Err(error),
            };
            if let Err(error) = applied {
                warn(&format!("{}/{}: {error}", route.app_id, route.document));
            }
        }
        for (route, answer) in opened {
            self.workspace_answered(&route, answer, &mut woken);
            self.reader_answered(&route, answer, &mut woken);
        }
        self.wakes(woken, content_arrived)
    }

    /// What the replica holds of `route` now, as a record that landed is read: only what
    /// changed since the store took it last, where it keeps the record. `None` while the
    /// record waits for the page's record worker to verify it (`crate::record_verify`): the
    /// records that are ready show meanwhile, and this one lands again once it is verified.
    fn read_landed(&mut self, route: &DocumentRoute) -> Option<Result<ResourceReading, String>> {
        let kept = self.kept_heads(route);
        let read = tilo_automerge_isolate::away::deferring(|| {
            self.peer.engine().record_update(
                &route.cluster_id,
                &route.app_id,
                &route.document,
                kept.as_deref(),
            )
        });
        if read.is_err() && tilo_automerge_isolate::away::verifying() {
            self.verifying.1 = tilo_automerge_isolate::away::verified();
            self.verifying.0.insert(route.clone());
            return None;
        }
        Some(read)
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
    /// queued on the records' content links ([`ContentLink`]). Content a window
    /// stored for others is not offered from a page.
    fn ask_content(&mut self) {
        if !self.ready {
            return;
        }
        let device = self.peer.status().device;
        let now = crate::shell::now_ms();
        let records: BTreeSet<(String, String)> = self.open_resources().collect();
        for (app, record) in records {
            // The record's windows each queue on a content link of their own.
            for link in ContentLink::on_route(&self.content_route(&app, &record)) {
                for hash in link.take_requests().wants {
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
                        link.retry_want(hash);
                    }
                }
            }
        }
    }

    /// Keep a piece of content a member sent; whether it completed content
    /// a record's window asked for, now on the record's content link.
    fn content_piece(&mut self, frame: &[u8]) -> bool {
        let device = self.peer.status().device;
        let Some(arrived) = self
            .content
            .piece(&device, frame, tilo_clock::unix_now_ms())
        else {
            return false;
        };
        let (app, record) = arrived.tag;
        for link in ContentLink::on_route(&self.content_route(&app, &record)) {
            link.arrive(arrived.hash.clone(), arrived.bytes.clone());
        }
        true
    }

    /// Withdraw what the host refused of `route` from the open record that
    /// sent it, which tells its app, and move the record's replication
    /// frontier back to what it keeps. The raw `reason` is for the log; the
    /// app shows a person its plain form. The layout and each person's view of
    /// it are no open document and nothing of theirs is withdrawn: what the
    /// person changed is theirs, their own view ([`Self::write_view`]),
    /// and what they change of the workspace's layout is sent only when their
    /// role writes it ([`Self::send_desktop`]).
    fn withdraw(
        &mut self,
        route: &DocumentRoute,
        changes: &[[u8; 32]],
        refusal: &tilo_server_protocol::refusal::Refusal,
        woken: &mut HashSet<u64>,
    ) {
        warn(&format!(
            "{}/{}: the workspace refused a change: {refusal}",
            route.app_id, route.document
        ));
        if changes.is_empty() {
            return;
        }
        let refused: Vec<ui::automerge::ChangeHash> = changes
            .iter()
            .map(|hash| ui::automerge::ChangeHash(*hash))
            .collect();
        let plain = refusal.plain();
        for (&id, mounted) in &mut self.mounts {
            for store in mounted.attachment.stores.borrow().iter() {
                if store.app_and_home().0 != route.app_id {
                    continue;
                }
                let Some(checkout) = store.open_resources().into_iter().find(|checkout| {
                    tilo_desktop_surface::host::records::wire_document(&checkout.id)
                        == route.document
                }) else {
                    continue;
                };
                match store.withdraw_refused(&checkout.id, &refused, &plain) {
                    Ok(withdrawn) => {
                        if let Some(heads) = withdrawn {
                            // What the person just did is taken back: say so
                            // over the desktop, whatever the app shows, so a
                            // change the workspace refused never looks kept.
                            if let Some((handle, _)) = &mounted.attachment.desktop {
                                handle.acknowledge(refused_change(&route.app_id, &plain));
                            }
                            if let Some(replica) = mounted
                                .documents
                                .get_mut(&(route.app_id.clone(), route.document.clone()))
                            {
                                replica.advance(heads);
                            }
                        }
                        woken.insert(id);
                    }
                    Err(error) => warn(&format!(
                        "{}/{}: a refused change stays: {error}",
                        route.app_id, route.document
                    )),
                }
            }
        }
    }

    /// The host answered `route`, a record this page opened or asked for,
    /// with `answer`: it retains nothing of it or holds it with no change in it,
    /// or it is another person's own. Each store holding or wanting it tells
    /// what reads it ([`ui::resource::ResourceStore::note_workspace_answer`]), and a
    /// window that waited for it is built again: on a new record, or saying
    /// whose it is.
    fn workspace_answered(
        &self,
        route: &DocumentRoute,
        answer: WorkspaceAnswer,
        woken: &mut HashSet<u64>,
    ) {
        if route.cluster_id != self.uuid {
            return;
        }
        for (&id, mounted) in &self.mounts {
            let answered = crate::landing::answered(
                &mounted.attachment.stores.borrow(),
                &route.app_id,
                &route.document,
                answer,
            );
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
                if let Some(checkout) = store.open_resources().into_iter().find(|checkout| {
                    tilo_desktop_surface::host::records::wire_document(&checkout.id)
                        == route.document
                }) {
                    store.workspace_settled(&checkout.id);
                    woken.insert(id);
                }
            }
        }
    }

    /// Merge `bytes`, the replica's whole document of `route`, at `heads`, into the
    /// open record, the desktop, or the record's stored document for whoever opens it.
    fn apply(
        &mut self,
        route: &DocumentRoute,
        (bytes, heads): (Vec<u8>, &[[u8; 32]]),
        replayed: bool,
        woken: &mut HashSet<u64>,
    ) -> Result<(), String> {
        if route.app_id == DESKTOP_APP {
            // The layout, and the person's view of it: another member's view of it is not this
            // page's to show.
            if let Some(owner) = tilo_desktop_model::person_view::person_of(&route.document) {
                return if self.person.as_deref() == Some(owner) {
                    self.apply_view(&bytes, woken)
                } else {
                    Ok(())
                };
            }
            return self.apply_desktop(&bytes, woken);
        }
        let app = route.app_id.clone();
        let record = route.document.clone();
        self.changes.note(&self.uuid, &app, &record, replayed);
        let mut merged = false;
        for (&id, mounted) in &mut self.mounts {
            for store in mounted.attachment.stores.borrow().iter() {
                if store.app_and_home().0 != app {
                    continue;
                }
                let Some(checkout) = store.open_resources().into_iter().find(|checkout| {
                    tilo_desktop_surface::host::records::wire_document(&checkout.id) == record
                }) else {
                    continue;
                };
                let replica = mounted
                    .documents
                    .entry((app.clone(), record.clone()))
                    .or_default();
                if replica.merge_delivered(
                    store,
                    &checkout,
                    DocumentMutation::Snapshot(bytes.clone()),
                    delivered_heads(heads),
                )? {
                    woken.insert(id);
                }
                mounted
                    .merged
                    .insert((app.clone(), record.clone()), sorted_heads(heads));
                merged = true;
            }
        }
        if merged {
            return Ok(());
        }
        let key = tilo_session::platform::records::storage_key(&app, &self.uuid, &record)
            .ok_or("not a record id")?;
        match crate::landing::keep(&*self.store, &key, bytes, heads)? {
            crate::landing::Landed::Kept(heads) => {
                self.kept_for_opening(&app, record, heads, woken);
            }
            // The host holds the record with nothing in it (a window founded it
            // empty, or a member's content has yet to follow): that is all it
            // holds, and a window waiting for it opens it as a new record.
            crate::landing::Landed::Empty => {
                self.workspace_answered(route, WorkspaceAnswer::HoldsNone, woken);
            }
        }
        Ok(())
    }

    /// The heads the store holds `route` at for whoever opens it, when no mount has it open
    /// and the store took it from the replica: what a read of it need only bring the changes
    /// since. `None` for the desktop, an open record, or one the store does not hold.
    fn kept_heads(&self, route: &DocumentRoute) -> Option<Vec<[u8; 32]>> {
        if route.app_id == DESKTOP_APP {
            return None;
        }
        if self.is_open(&route.app_id, &route.document) {
            return self.merged_heads(&route.app_id, &route.document);
        }
        let heads = self
            .delivered
            .get(&(route.app_id.clone(), route.document.clone()))?;
        let mut heads: Vec<[u8; 32]> = heads.iter().map(|head| head.0).collect();
        heads.sort_unstable();
        Some(heads)
    }

    /// The heads every mount with `record` of `app` open last took it in at, when they all took
    /// it in at the same ones: what a read of it then need only bring the changes since. `None`
    /// when a mount has not taken it in yet, or two took in different reads.
    fn merged_heads(&self, app: &str, record: &str) -> Option<Vec<[u8; 32]>> {
        let key = (app.to_owned(), record.to_owned());
        let mut shared: Option<Vec<[u8; 32]>> = None;
        for mounted in self.mounts.values() {
            for store in mounted.attachment.stores.borrow().iter() {
                if store.app_and_home().0 != app {
                    continue;
                }
                let Some(checkout) = store.open_resources().into_iter().find(|checkout| {
                    tilo_desktop_surface::host::records::wire_document(&checkout.id) == record
                }) else {
                    continue;
                };
                let heads = match mounted.merged.get(&key) {
                    // A record closed and opened again starts from what its store kept, which may
                    // stand before the heads this mount took in last.
                    Some(heads) => store
                        .read(&checkout.id, |doc| {
                            heads.iter().all(|head| {
                                ui::automerge::ReadDoc::has_change(
                                    doc,
                                    &ui::automerge::ChangeHash(*head),
                                )
                            })
                        })
                        .filter(|holds| *holds)
                        .map(|_| heads.clone())?,
                    // Not taken in yet: the window's copy, which this page kept from the
                    // workspace before. A read since its heads brings only what changed, or
                    // the document whole where the workspace lacks some of it.
                    None => sorted_heads(
                        &store
                            .with_action_state(&checkout.id, |doc, _, actions| {
                                actions.commit_for_save(doc);
                                doc.get_heads()
                            })
                            .ok()?
                            .iter()
                            .map(|head| head.0)
                            .collect::<Vec<_>>(),
                    ),
                };
                if shared.as_ref().is_some_and(|shared| *shared != heads) {
                    return None;
                }
                shared = Some(heads);
            }
        }
        shared.filter(|heads| !heads.is_empty())
    }

    /// The workspace holds `route` where the mounts with it open stand ([`Self::merged_heads`]):
    /// each sends only what it changes after that, never its copy whole.
    fn hold_open(&mut self, route: &DocumentRoute) {
        let key = (route.app_id.clone(), route.document.clone());
        let Some(heads) = self.merged_heads(&route.app_id, &route.document) else {
            // The mounts stand at different heads: each sends its copy whole, as one the
            // workspace held none of, so no change of theirs waits on a read that never comes.
            self.opened.insert(key, false);
            return;
        };
        for mounted in self.mounts.values_mut() {
            let replica = mounted.documents.get_mut(&key);
            if let Some(replica) = replica
                && !replica.snapshotted()
            {
                replica.advance(delivered_heads(&heads));
                mounted.merged.insert(key.clone(), heads.clone());
            }
        }
    }

    /// Whether a mount has `record` of `app` open.
    fn is_open(&self, app: &str, record: &str) -> bool {
        self.mounts.values().any(|mounted| {
            mounted.attachment.stores.borrow().iter().any(|store| {
                store.app_and_home().0 == app
                    && store.open_resources().into_iter().any(|checkout| {
                        tilo_desktop_surface::host::records::wire_document(&checkout.id) == record
                    })
            })
        })
    }

    /// Merge `changes`, what changed of `route` since the heads every mount with it open last took
    /// it in at ([`Self::merged_heads`]), into those mounts' replicas: the record then stands at
    /// `heads` in each, and none of them loads the record whole to get there.
    fn apply_open_changes(
        &mut self,
        route: &DocumentRoute,
        changes: &[u8],
        heads: &[[u8; 32]],
        replayed: bool,
        woken: &mut HashSet<u64>,
    ) -> Result<(), String> {
        let (app, record) = (route.app_id.clone(), route.document.clone());
        self.changes.note(&self.uuid, &app, &record, replayed);
        for (&id, mounted) in &mut self.mounts {
            for store in mounted.attachment.stores.borrow().iter() {
                if store.app_and_home().0 != app {
                    continue;
                }
                let Some(checkout) = store.open_resources().into_iter().find(|checkout| {
                    tilo_desktop_surface::host::records::wire_document(&checkout.id) == record
                }) else {
                    continue;
                };
                let replica = mounted
                    .documents
                    .entry((app.clone(), record.clone()))
                    .or_default();
                if replica.merge_delivered(
                    store,
                    &checkout,
                    DocumentMutation::Incremental(changes.to_vec()),
                    delivered_heads(heads),
                )? {
                    woken.insert(id);
                }
                mounted
                    .merged
                    .insert((app.clone(), record.clone()), sorted_heads(heads));
            }
        }
        Ok(())
    }

    /// Keep `changes`, what changed of `route` since the heads the store held it at
    /// ([`Self::kept_heads`]), after what the store holds: the record then stands at `heads`
    /// for whoever opens it, and nothing of it is loaded to get there.
    fn apply_kept_changes(
        &mut self,
        route: &DocumentRoute,
        changes: Vec<u8>,
        heads: &[[u8; 32]],
        replayed: bool,
        woken: &mut HashSet<u64>,
    ) -> Result<(), String> {
        let (app, record) = (route.app_id.clone(), route.document.clone());
        self.changes.note(&self.uuid, &app, &record, replayed);
        let key = tilo_session::platform::records::storage_key(&app, &self.uuid, &record)
            .ok_or("not a record id")?;
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
        // A view asked of this person's layout (an agent's open, from a device that holds no
        // layout) is put on it here, as this person's change, and sent with the next one.
        self.place_asked();
        let Some(value) = desktop_value(self.desktop.document()) else {
            return Ok(());
        };
        if self.desktop_value.as_ref() == Some(&value) {
            return Ok(());
        }
        let mut desktops = Vec::new();
        for (&id, mounted) in &self.mounts {
            if let Some((handle, shared)) = &mounted.attachment.desktop {
                crate::desktop::apply_shared(handle, shared, &value)?;
                desktops.push((id, (*handle).clone()));
            }
        }
        for (id, handle) in desktops {
            if self.view_read {
                self.show_view(&handle);
            }
            woken.insert(id);
        }
        self.desktop_value = Some(value);
        Ok(())
    }

    /// Show the person's view of this workspace's layout, as this page holds it, over what the
    /// person changed here since this page last showed that view: what they changed stays, and
    /// everything else takes the view's. A page that showed none of it takes it whole. Whether the
    /// shell changed.
    fn show_view(&mut self, handle: &tilo_desktop_surface::DesktopHandle) -> bool {
        let Some(workspace) = tilo_desktop_model::WorkspaceId::parse(&self.uuid) else {
            return false;
        };
        let read = tilo_desktop_model::person_view::PersonView::read(self.view.document());
        let mut tree = handle.read(Clone::clone);
        let here = tilo_desktop_model::person_view::PersonView::of(&tree, workspace);
        let view = read.over(&here, self.view_shown.as_ref());
        // What the view says the layout shows: a change the person makes after it is theirs over
        // it.
        read.applied(&mut tree, workspace);
        self.view_shown = Some(tilo_desktop_model::person_view::PersonView::of(
            &tree, workspace,
        ));
        handle.show_view(&view, workspace)
    }

    /// Merge `bytes`, the person's view of this workspace's layout as they left it, and show it
    /// on every desktop of this page: the strip and group in front, the tab in front of
    /// each group, and how wide each group is.
    fn apply_view(&mut self, bytes: &[u8], woken: &mut HashSet<u64>) -> Result<(), String> {
        self.view
            .merge_snapshot(bytes)
            .map_err(|error| format!("the view of the layout did not read: {error}"))?;
        self.view_read = true;
        self.view_sending = DesktopSending::Changes;
        let desktops: Vec<(u64, tilo_desktop_surface::DesktopHandle)> = self
            .mounts
            .iter()
            .filter_map(|(&id, mounted)| {
                mounted
                    .attachment
                    .desktop
                    .as_ref()
                    .map(|(handle, _)| (id, (*handle).clone()))
            })
            .collect();
        for (id, handle) in desktops {
            if self.show_view(&handle) {
                woken.insert(id);
            }
        }
        Ok(())
    }

    /// Read the person's view of the layout from this device's replica of it, when the
    /// page holds it: a page that learned whose view it is after it arrived still shows
    /// the view. Once it is read, the view's changes reach the page as they land.
    fn read_view(&mut self) {
        if self.view_read {
            return;
        }
        let Some(person) = self.person.clone() else {
            return;
        };
        let document = tilo_desktop_model::person_view::resource_of(&person);
        let Some(bytes) = self
            .peer
            .engine()
            .record(&self.uuid, DESKTOP_APP, &document)
            .ok()
            .flatten()
        else {
            return;
        };
        let mut woken = HashSet::new();
        if self.apply_view(&bytes, &mut woken).is_ok() {
            for id in woken {
                if let Some(mounted) = self.mounts.get(&id) {
                    mounted.attachment.changed.set(true);
                    (mounted.attachment.notify)();
                }
            }
        }
    }

    /// Write `tree`, the layout this page shows, as the person here's view of this workspace's
    /// layout into their own view, as the native session does: every device of theirs shows it.
    fn write_view(&mut self, tree: &tilo_desktop_model::Layout) {
        // What the workspace holds is read before this page writes over it: a browser that just
        // joined shows the view it kept until the person's view reaches it.
        if !self.view_read {
            return;
        }
        let Some(person) = self.person.clone() else {
            return;
        };
        let Some(workspace) = tilo_desktop_model::WorkspaceId::parse(&self.uuid) else {
            return;
        };
        let view = tilo_desktop_model::person_view::PersonView::of(tree, workspace);
        match view.write(self.view.document_mut()) {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                warn(&format!("the view of the layout was not written: {error}"));
                return;
            }
        }
        // What the view holds now: a change of it after this is the person's over it.
        self.view_shown = Some(view);
        tilo_desktop_surface::history_log::commit(
            self.view.document_mut(),
            &person,
            ui::time::unix_now_i64(),
        );
        let route = self.route(
            DESKTOP_APP,
            &tilo_desktop_model::person_view::resource_of(&person),
        );
        let (mutation, heads) = if self.view_sending == DesktopSending::Changes {
            let (delta, heads) = self.view.pending_outbound_delta();
            if delta.is_empty() {
                return;
            }
            (DocumentMutation::Incremental(delta), heads)
        } else {
            let doc = self.view.document_mut();
            (DocumentMutation::Snapshot(doc.save()), doc.get_heads())
        };
        if self.peer.write(route, &mutation).is_ok() {
            self.view.advance_outbound(heads);
            self.view_sending = DesktopSending::Changes;
        }
    }

    /// Put the views asked of this page's person's layout on the layout this page holds
    /// ([`tilo_desktop_model::asked`]), as that person's change. Says whether it placed any.
    fn place_asked(&mut self) -> bool {
        let Some(author) = self.mounts.values().find_map(|mounted| {
            mounted
                .attachment
                .desktop
                .as_ref()
                .map(|_| mounted.attachment.author.clone())
        }) else {
            return false;
        };
        let doc = self.desktop.document_mut();
        match tilo_desktop_model::asked::place_asked(doc, &author) {
            Ok(true) => {
                tilo_desktop_surface::history_log::commit(doc, &author, ui::time::unix_now_i64());
                true
            }
            Ok(false) => false,
            Err(error) => {
                warn(&format!(
                    "a view asked of this layout was not placed: {error}"
                ));
                false
            }
        }
    }

    fn send(&mut self) {
        if !self.ready {
            return;
        }
        self.open_read_wanted();
        let mut outgoing: Vec<Pending> = Vec::new();
        let mut opening: Vec<DocumentRoute> = Vec::new();
        for (&id, mounted) in &mut self.mounts {
            for store in mounted.attachment.stores.borrow().iter() {
                let (app, home) = store.app_and_home();
                if home != self.uuid {
                    continue;
                }
                for checkout in store.open_resources() {
                    let key = (
                        app.clone(),
                        tilo_desktop_surface::host::records::wire_document(&checkout.id).to_owned(),
                    );
                    // A record a join does not replay is asked for when a
                    // mount first opens it.
                    if !mounted.documents.contains_key(&key)
                        && (self.when_opened
                            || neighbour_wire::document_sync(&key.1)
                                != neighbour_wire::DocumentSync::Eager)
                    {
                        opening.push(DocumentRoute {
                            app_id: app.clone(),
                            cluster_id: self.uuid.clone(),
                            document: key.1.clone(),
                        });
                    }
                    let replica = mounted.documents.entry(key.clone()).or_insert_with(|| {
                        first_sync(&mut self.delivered, &mut mounted.merged, &key)
                    });
                    if !replica.snapshotted()
                        && !crate::landing::sends_whole(
                            asked_for(&self.asked, &opening, &key),
                            self.opened.get(&key).copied(),
                        )
                    {
                        continue;
                    }
                    let route = DocumentRoute {
                        app_id: app.clone(),
                        cluster_id: self.uuid.clone(),
                        document: key.1.clone(),
                    };
                    match replica.take_outbound(store, &checkout.id) {
                        Ok(Some(next)) => {
                            // A window here wrote the record: the session's activity and the
                            // person's mentions read it as any other change of it.
                            self.changes
                                .note(&self.uuid, &route.app_id, &route.document, false);
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
                .insert((route.app_id.clone(), route.document.clone()))
            {
                continue;
            }
            if self
                .peer
                .send(Cmd::OpenDocumentAt(self.workspace, route))
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
                .and_then(|m| m.documents.get_mut(&key))
            {
                replica.advance(heads);
            }
        }
        self.send_desktop();
        self.read_view();
    }

    /// Ask the workspace for each resource a reader on this page named that it was not asked
    /// for yet, as one a window opens is asked for, once the workspace is ready to be asked.
    fn open_read_wanted(&mut self) {
        if !self.ready {
            return;
        }
        let wanted: Vec<(String, String)> = self
            .read_wanted
            .iter()
            .filter(|key| !self.asked.contains(*key))
            .cloned()
            .collect();
        for (app, wire) in wanted {
            let route = self.route(&app, &wire);
            if self
                .peer
                .send(Cmd::OpenDocumentAt(self.workspace, route))
                .is_err()
            {
                return;
            }
            self.asked.insert((app, wire));
        }
    }

    /// The workspace answered `route` and sent none of it: a reader on this page that named it
    /// is answered so, on the pass this wakes its mount for.
    fn reader_answered(
        &mut self,
        route: &DocumentRoute,
        answer: WorkspaceAnswer,
        woken: &mut HashSet<u64>,
    ) {
        if route.cluster_id != self.uuid {
            return;
        }
        let key = (route.app_id.clone(), route.document.clone());
        if self.read_wanted.contains(&key) {
            woken.extend(self.mounts.keys().copied());
        }
        self.read_answers.insert(key, answer);
    }

    /// The records windows opened that are not here and that this page has
    /// not asked the workspace for yet: it sends each, or says it has none
    /// of it, and only then is it new.
    fn wanted_routes(&self) -> Vec<DocumentRoute> {
        let mut wanted = Vec::new();
        // The person's view of the layout is their own, which a page showing the shell asks
        // for as it asks for the resources its windows open: their other devices' changes reach it
        // from then on.
        if self
            .mounts
            .values()
            .any(|mounted| mounted.attachment.desktop.is_some())
            && let Some(person) = &self.person
        {
            wanted.push(self.route(
                DESKTOP_APP,
                &tilo_desktop_model::person_view::resource_of(person),
            ));
        }
        for mounted in self.mounts.values() {
            for store in mounted.attachment.stores.borrow().iter() {
                let (app, home) = store.app_and_home();
                if home != self.uuid {
                    continue;
                }
                for record in store.wanted_resources() {
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
                        tilo_desktop_surface::host::records::wire_document(&record).to_owned(),
                    );
                    if !self.asked.contains(&key) {
                        wanted.push(DocumentRoute {
                            app_id: key.0,
                            cluster_id: self.uuid.clone(),
                            document: key.1,
                        });
                    }
                }
            }
        }
        wanted
    }

    /// Tell the mounted desktops why the workspace cannot answer this page now, as its link says,
    /// or that it can again: their windows waiting for a resource show that failure rather than wait
    /// for an answer that cannot come.
    fn follow_link(&self) {
        let cause = self
            .peer
            .status()
            .unanswerable(self.workspace)
            .map(|failure| crate::desktop::Unanswered::of(failure.code, failure.message.clone()));
        for mounted in self.mounts.values() {
            if let Some((handle, shared)) = &mounted.attachment.desktop {
                shared.follow_unanswerable(cause.clone(), handle);
            }
        }
    }

    /// The workspace's members and their names, as the peer last heard its governance say: the
    /// attached desktops release what someone who left made. When each was admitted is projected
    /// onto the shell with the rest of the governance (`Desktop::project_governance`).
    fn follow_members(&mut self, woken: &mut HashSet<u64>) {
        let status = self.peer.status();
        let members = status.workspace_members.get(&self.uuid).cloned();
        let names = status
            .workspace_names
            .get(&self.uuid)
            .cloned()
            .unwrap_or_default();
        if members == self.members && names == self.names {
            return;
        }
        self.members = members;
        self.names = names;
        for (&id, mounted) in &self.mounts {
            if let Some((_, shared)) = &mounted.attachment.desktop {
                shared.members.replace(self.members.clone());
                shared.names.replace(self.names.clone());
                woken.insert(id);
            }
        }
    }

    fn send_desktop(&mut self) {
        // A person who only looks at the workspace writes nothing of its layout: what they
        // change of how they see it is their own view ([`Self::write_view`]),
        // and the workspace refuses every other write their role makes. A role not answered yet
        // sends nothing either.
        if !tilo_desktop_model::commands::policy::writes(self.role.role()) {
            return;
        }
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
        // The shell as it reads to a peer: what it says moved is what the
        // workspace decides on, and an unchanged shell writes nothing.
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
                let view = self.view_read.then(|| {
                    tilo_desktop_model::person_view::PersonView::read(self.view.document())
                });
                let workspace = tilo_desktop_model::WorkspaceId::parse(&self.uuid);
                for mounted in self.mounts.values() {
                    if let Some((handle, shared)) = &mounted.attachment.desktop {
                        if let Err(error) = crate::desktop::return_to(handle, shared, before_value)
                        {
                            warn(&format!("shared desktop not restored: {error}"));
                        }
                        if let (Some(view), Some(workspace)) = (&view, &workspace) {
                            handle.show_view(view, *workspace);
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
        // What differs is written into the layout's own document, as one change
        // stamped with the time and who made it, which the history pane tells
        // by and every peer reads back as the layout.
        let wrote = tilo_desktop_model::desktop_doc::write_shared(
            doc,
            &desktop.tree,
            &desktop.records,
            &desktop.authors,
        );
        if wrote {
            tilo_desktop_surface::history_log::commit(doc, &author, ui::time::unix_now_i64());
            // A view asked before this page had a layout to put it on is put there now.
            if self.place_asked()
                && let Some(placed) = desktop_value(self.desktop.document())
            {
                for mounted in self.mounts.values() {
                    if let Some((handle, shared)) = &mounted.attachment.desktop {
                        if let Err(error) = crate::desktop::apply_shared(handle, shared, &placed) {
                            warn(&format!("an asked view did not show: {error}"));
                        }
                        (mounted.attachment.notify)();
                    }
                }
            }
        }
        if wrote && joined {
            // Windows opened, closed, moved or renamed: the person's own
            // work on the desktop.
            self.authored = self.authored.saturating_add(1);
        }
        // Nothing new from the shell, and nothing placed here since the last send either.
        if !wrote
            && self.desktop_sending == DesktopSending::Changes
            && self.desktop.pending_outbound_delta().0.is_empty()
        {
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
        let route = self.route(DESKTOP_APP, DESKTOP_DOCUMENT);
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
    tilo_desktop_model::shared_resource::read(doc, None)
        .and_then(|desktop| serde_json::to_value(desktop).ok())
}

/// The events replication reads: what lands in, is refused from, opens or arrives for a
/// workspace's records.
pub(crate) fn takes(event: &Event) -> bool {
    matches!(
        event,
        Event::HostReady { .. }
            | Event::Landed { .. }
            | Event::DocumentContent { .. }
            | Event::Refused { .. }
            | Event::DocumentOpened { .. }
    )
}

/// How a mount sends `document`, which it just opened: from the heads the workspace delivered it at
/// before any mount had it open, when it did.
fn first_sync(
    delivered: &mut HashMap<(String, String), Vec<ui::automerge::ChangeHash>>,
    merged: &mut HashMap<(String, String), Vec<[u8; 32]>>,
    document: &(String, String),
) -> DocumentSync {
    match delivered.remove(document) {
        // The workspace has the document as it delivered it, and the store the mount opened it
        // from holds it there.
        Some(heads) => {
            merged.insert(
                document.clone(),
                sorted_heads(&heads.iter().map(|head| head.0).collect::<Vec<_>>()),
            );
            DocumentSync::delivered(heads)
        }
        None => DocumentSync::new(),
    }
}

/// Whether this page asked the workspace for `document`, or asks for it in this pass (`asking`).
fn asked_for(
    asked: &HashSet<(String, String)>,
    asking: &[DocumentRoute],
    document: &(String, String),
) -> bool {
    asked.contains(document)
        || asking
            .iter()
            .any(|route| route.app_id == document.0 && route.document == document.1)
}

/// `heads` sorted, as a read of a record names the heads it starts from.
/// `heads` as the record's change hashes.
fn delivered_heads(heads: &[[u8; 32]]) -> Vec<ui::automerge::ChangeHash> {
    heads
        .iter()
        .copied()
        .map(ui::automerge::ChangeHash)
        .collect()
}

fn sorted_heads(heads: &[[u8; 32]]) -> Vec<[u8; 32]> {
    let mut heads = heads.to_vec();
    heads.sort_unstable();
    heads.dedup();
    heads
}

/// What a host's answer to an opened record tells the stores waiting for it, when it sent none
/// of the record: it holds none, or the record is another person's own. An answer that sent frames
/// tells them through the frames.
fn answered_without_frames(answer: DocumentAnswer) -> Option<WorkspaceAnswer> {
    match answer {
        DocumentAnswer::Sent { frames: 0 } => Some(WorkspaceAnswer::HoldsNone),
        DocumentAnswer::NotYours => Some(WorkspaceAnswer::NotYours),
        DocumentAnswer::Sent { .. } => None,
    }
}

/// The records that landed, in the order they are read: records first, so a desktop naming a new
/// tab's record finds it.
fn read_order(landed: BTreeSet<DocumentRoute>) -> Vec<DocumentRoute> {
    let mut landed: Vec<DocumentRoute> = landed.into_iter().collect();
    landed.compact_sort_by_key(|route| route.app_id == DESKTOP_APP);
    landed
}
