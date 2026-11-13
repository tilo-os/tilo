//! The workspace a page replicates, as the start module reads it.
//!
//! [`crate::replication`] holds it: the records and the desk a workspace
//! keeps, merged from what the host sends and written as the person changes
//! them. Reading and writing them runs through the client's engine — the
//! workspace's channel keys, the frames they seal and the codec that reads
//! them — and that engine is part of the peer's code, which the page fetches
//! right after the start module (`web/host.mjs`). A desk, a shell and a mount
//! ask about the workspace while they draw, so the start module reaching the
//! replication directly would link the engine into itself, and every page
//! would wait for it before its first frame.
//!
//! They ask through [`Replicated`] instead: the peer's code fills the table in
//! when it starts (`replication::install`, from `peer_sync::register`), and
//! the splitter (`tools/web/wasm-split`) cannot follow a call through a
//! function pointer, so the replication loads with the peer's code rather
//! than with the page's. A page that never started a peer has no workspace to
//! replicate and reads the empty answer of every question below: no records,
//! no changes, nothing to publish.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use durable_store::MemoryStore;
use tilo_relay_protocol::workspaces::WorkspacesRequest;
use tilo_session::platform::records::Changes;
use ui::automerge::AutoCommit;
use ui::record::RecordStore;

/// The desk's own document, in the workspace's routes: what the desk of the
/// browser runtime is to every other device of the workspace.
pub(crate) const DESKTOP_APP: &str = "so.tilo.browser-desktop";
pub(crate) const DESKTOP_RECORD: &str = "desktop";

/// What a mount shares with the workspace.
pub(crate) struct Attachment {
    pub stores: Rc<RefCell<Vec<RecordStore>>>,
    pub desktop: Option<(tilo_desktop_surface::DesktopHandle, crate::desktop::Shared)>,
    /// Set when a peer's change reached one of this mount's records.
    pub changed: Rc<Cell<bool>>,
    /// Schedules a frame for the mount.
    pub notify: std::sync::Arc<dyn Fn() + Send + Sync>,
    /// Who the mount's person is: the desktop changes it publishes name
    /// them, so every page's Activity says whose they were.
    pub author: String,
    /// What the mount's apps may offer their person: set to the role the
    /// workspace answers they hold, so an app does not offer what the workspace
    /// would refuse.
    pub permissions: ui::permissions::SharedPermissions,
}

/// A request to the workspace's host, made for the workspace this page
/// replicates: the host's workspaces door reads the workspaces it governs by
/// their uuid.
pub(crate) type Ask = Box<dyn FnOnce(&str) -> WorkspacesRequest>;

/// Where the host's answer to an [`Ask`] goes.
pub(crate) type Answer =
    Box<dyn FnOnce(Result<tilo_relay_protocol::workspaces::WorkspacesReply, String>)>;

/// What a page without a peer answers when it is asked about the workspace it
/// replicates: it replicates none.
pub(crate) const NONE: &str = "this page joined no workspace";

/// The page's replication, as the peer's code hands it over.
#[derive(Clone, Copy)]
pub(crate) struct Replicated {
    /// Where the page keeps the workspace's documents: in the profile, or in
    /// the page when its peer is kept in no profile.
    pub(crate) page_store: fn(u32) -> Option<Rc<MemoryStore>>,
    /// A mount's records and desk, to replicate with the workspace.
    pub(crate) attach: fn(u32, u64, Attachment) -> bool,
    pub(crate) detach: fn(u32, u64),
    /// Publish what a mount wrote since the last pass.
    pub(crate) publish: fn(u32),
    /// Follow the answers the workspace sent since the last pass.
    pub(crate) follow_answers: fn(u32),
    /// Ask the host for the content of the records this page wants.
    pub(crate) ask_content: fn(u32),
    /// The certified device key (hex) the workspace's documents are kept as.
    pub(crate) device_of: fn(u32) -> Option<String>,
    /// A record as the workspace committed it, without what is still being typed.
    pub(crate) committed_record: fn(u32, &str, &str, &str) -> Option<Vec<u8>>,
    /// The one stream of record changes this page feeds.
    pub(crate) changes: fn() -> Changes,
    /// A record as the page holds it now.
    pub(crate) record_document: fn(u32, &str, &str) -> Option<AutoCommit>,
    /// The content store a mount lends its windows: its workspace's.
    pub(crate) content: fn(u32) -> Option<tilo_content::ContentStore>,
    /// The workspace's answer whether the person may change `node`.
    pub(crate) permit: fn(u32, &str, bool),
    /// Ask the workspace's host a request through its workspaces door.
    pub(crate) host_then: fn(u32, Ask, Answer) -> Result<(), String>,
    /// The workspace peer `id` replicates, by its uuid.
    pub(crate) workspace_uuid: fn(u32) -> Option<String>,
    /// The records of that workspace the page holds, as wire `(app, record)`.
    pub(crate) records: fn(u32) -> Vec<(String, String)>,
    /// The retained desk's frontier, without cloning its history.
    pub(crate) desktop_history_heads: fn(u32) -> Option<Vec<ui::automerge::ChangeHash>>,
    /// The workspace's desk document, as the profile keeps a copy of it.
    pub(crate) desk_bytes: fn(u32) -> Option<Vec<u8>>,
}

thread_local! {
    /// What the peer's code handed over, once it started.
    static INSTALLED: RefCell<Option<Replicated>> = const { RefCell::new(None) };
}

/// Hand the start module the page's replication.
pub(crate) fn install(replicated: Replicated) {
    INSTALLED.with_borrow_mut(|installed| *installed = Some(replicated));
}

/// `ask` of the installed replication, or `empty` while the peer's code is not in.
fn installed<T>(empty: T, ask: impl FnOnce(&Replicated) -> T) -> T {
    INSTALLED.with_borrow(|installed| installed.as_ref().map_or(empty, ask))
}

pub(crate) fn page_store(id: u32) -> Option<Rc<MemoryStore>> {
    installed(None, |replicated| (replicated.page_store)(id))
}

pub(crate) fn attach(id: u32, mount: u64, attachment: Attachment) -> bool {
    installed(false, |replicated| {
        (replicated.attach)(id, mount, attachment)
    })
}

pub(crate) fn detach(id: u32, mount: u64) {
    installed((), |replicated| (replicated.detach)(id, mount));
}

pub(crate) fn publish(id: u32) {
    installed((), |replicated| (replicated.publish)(id));
}

pub(crate) fn follow_answers(id: u32) {
    installed((), |replicated| (replicated.follow_answers)(id));
}

pub(crate) fn ask_content(id: u32) {
    installed((), |replicated| (replicated.ask_content)(id));
}

pub(crate) fn device_of(id: u32) -> Option<String> {
    installed(None, |replicated| (replicated.device_of)(id))
}

pub(crate) fn committed_record(
    id: u32,
    workspace: &str,
    app: &str,
    record: &str,
) -> Option<Vec<u8>> {
    installed(None, |replicated| {
        (replicated.committed_record)(id, workspace, app, record)
    })
}

pub(crate) fn changes() -> Changes {
    installed(Changes::default(), |replicated| (replicated.changes)())
}

pub(crate) fn record_document(id: u32, app: &str, record: &str) -> Option<AutoCommit> {
    installed(None, |replicated| {
        (replicated.record_document)(id, app, record)
    })
}

pub(crate) fn content(id: u32) -> Option<tilo_content::ContentStore> {
    installed(None, |replicated| (replicated.content)(id))
}

pub(crate) fn permit(id: u32, node: &str, may: bool) {
    installed((), |replicated| (replicated.permit)(id, node, may));
}

pub(crate) fn host_then(id: u32, request: Ask, done: Answer) -> Result<(), String> {
    installed(Err(NONE.to_owned()), |replicated| {
        (replicated.host_then)(id, request, done)
    })
}

pub(crate) fn workspace_uuid(id: u32) -> Option<String> {
    installed(None, |replicated| (replicated.workspace_uuid)(id))
}

pub(crate) fn records(id: u32) -> Vec<(String, String)> {
    installed(Vec::new(), |replicated| (replicated.records)(id))
}

pub(crate) fn desktop_history_heads(id: u32) -> Option<Vec<ui::automerge::ChangeHash>> {
    installed(None, |replicated| (replicated.desktop_history_heads)(id))
}

pub(crate) fn desk_bytes(id: u32) -> Option<Vec<u8>> {
    installed(None, |replicated| (replicated.desk_bytes)(id))
}
