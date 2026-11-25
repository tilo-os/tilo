//! The host services this broker's own transport serves natively: launching applications,
//! workspace memberships, browser sessions, and the contexts a certified connection arrives with.
//! The rest lives in [`tilo_broker_core::services`] and is re-exported here.
pub use tilo_broker_core::services::*;

mod published;
pub use published::{Caller, PublishedResources, Reading, ResourceWatch};

use std::sync::Arc;
use tilo_desktop_model::WorkspaceId;

pub use crate::apps::collab_app_broker::{
    AudioOutputFuture, AudioOutputServices, AwaitingDocument, CertifiedPersonContext,
    ChainHeadAnswer, DocumentBytes, HeldRevision, NativeDocumentRead, NativeDocumentReadContext,
    NativeDocumentReads, NativeMediaFuture, NativeMediaRequestContext, NativeMediaServices,
    ResourceRead,
};

/// A workspace's status once it changed ([`WorkspaceMemberships::workspace_status_change`]).
pub type WorkspaceStatusFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = runtime_collab::WorkspaceStatus> + Send>>;

/// Native workspace membership owner. Public callers choose only a closed typed
/// command; this host service supplies the private governor signing ownership.
pub trait WorkspaceMemberships: Send + Sync {
    fn requires_workspace(&self, workspace: &str) -> bool;

    /// Where `workspace` stands for the apps on this device: ready, waiting for
    /// its join or its placement on its host, or refused, as when no host
    /// holds it and it has no documents to open. A join is held while it
    /// waits and refused with the reason, in place of a failure further in.
    fn workspace_status(&self, _workspace: &str) -> runtime_collab::WorkspaceStatus {
        runtime_collab::WorkspaceStatus::Ready
    }

    /// `workspace`'s status once it is no longer `seen`, woken by the change that
    /// ends it rather than by asking again.
    fn workspace_status_change(
        &self,
        workspace: &str,
        seen: runtime_collab::WorkspaceStatus,
    ) -> WorkspaceStatusFuture {
        let now = self.workspace_status(workspace);
        if now == seen {
            Box::pin(std::future::pending())
        } else {
            Box::pin(std::future::ready(now))
        }
    }

    /// Whether `workspace`'s membership actions are still to come here: this
    /// device holds the workspace, joins it or has it placed on its host, and its
    /// replica is not projected yet ([`Self::requires_workspace`] turns true
    /// once it is, which announces a policy change). An app opening in such a
    /// workspace waits for them; one opening in any other workspace
    /// unpublished is refused.
    fn membership_pending(&self, _workspace: &str) -> bool {
        false
    }

    /// Confirm that the canonical owner's durable store is still usable.
    fn health(&self) -> Result<(), String> {
        Err("workspace membership owner continuity is unavailable".into())
    }

    /// Write `command` into `workspace`'s governance as `caller`'s device.
    fn execute(
        &self,
        workspace: String,
        caller: tilo_user_identity::DeviceCert,
        command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
    ) -> WorkspaceMembershipFuture;

    /// The picture `person` (their root, bytes) set in their profile, as the image's own bytes:
    /// what a member row an app reads shows wherever it names them as an author. `None` before
    /// they set one, or where this host keeps no profile for them.
    fn person_photo(&self, person: &[u8; 32]) -> Option<Vec<u8>> {
        let _ = person;
        None
    }
}

/// A membership command on its way into the workspace it changes.
pub type WorkspaceMembershipFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;

/// One checked change to a workspace's saved shortcuts. The broker mints
/// `invocation` and has checked the change against what `actor` is offered.
pub struct ShortcutLibraryRequest {
    pub workspace: String,
    pub actor: tilo_action_policy::Actor,
    /// The person an agent `actor` acts for, by their root, hex: what the agent saves is
    /// theirs, signed with the agent's own key. `None` for a person saving for themselves.
    pub acting_for: Option<String>,
    pub change: crate::apps::automation::ShortcutChange,
    /// The provenance the change leaves in workspace activity.
    pub context: tilo_actions::ActionContext,
    pub invocation: tilo_actions::InvocationId,
}

/// A shortcut change on its way into its workspace: done once the workspace's
/// governor committed it and this device holds it, or failed with why.
pub type ShortcutStore =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;

/// A host that stores a workspace's shortcuts and runs them. The broker
/// publishes the shortcut library and each saved shortcut's `run` only
/// where this service is installed.
pub trait ShortcutLibraryServices: Send + Sync {
    /// The chords already taken on this device, which a shortcut's keyboard
    /// trigger may not take.
    fn keymap(&self) -> settings_store::hardware::ShortcutConfig;
    /// Store one checked change in workspace `request.workspace` and record its
    /// effect in workspace activity. The broker awaits it on [`Self::runtime`],
    /// never on the thread that admitted the change; it fails with why the
    /// workspace cannot take the change.
    fn store(&self, request: ShortcutLibraryRequest) -> ShortcutStore;
    /// The runtime shortcut runs execute on.
    fn runtime(&self) -> Result<tokio::runtime::Handle, String>;
    /// What the desktop calls its fixed shortcut `fixed`, in the person's
    /// language: named when a keyboard trigger would take its chord.
    fn fixed_shortcut_label(
        &self,
        fixed: &settings_store::keyboard_shortcuts::FixedShortcut,
    ) -> String {
        fixed.id.to_string()
    }
}

pub use tilo_broker_core::steps::ContinuationServices;

/// What a workspace control session's calls answer.
pub type ControlAnswer<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, String>> + 'a>>;

/// One open workspace control session, on the broker's `LocalSet`.
pub trait WorkspaceControlSession {
    /// The session's next event.
    fn read(&self) -> ControlAnswer<'_, runtime_collab::WorkspaceControlEvent>;

    /// Send the controlled device `input`.
    fn send_input(&self, input: runtime_collab::WorkspaceControlInput) -> ControlAnswer<'_, ()>;

    /// End the session.
    fn close(&self) -> ControlAnswer<'_, ()>;
}

/// Asked before each step of a workspace control session; an error ends it.
pub type ControlRevalidate = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

/// Opens workspace control sessions; called on the broker `LocalSet`.
pub trait WorkspaceControlServices: Send + Sync {
    /// Opens the workspace control session of `workspace` with `target`.
    fn open(
        &self,
        workspace: WorkspaceId,
        target: [u8; 32],
        revalidate: ControlRevalidate,
    ) -> Result<std::rc::Rc<dyn WorkspaceControlSession>, String>;
}

/// Original Browser connection, minted only from certified launcher ownership.
#[derive(Clone)]
pub struct BrowserServiceSessionRequestContext {
    peer_id: u64,
    person: String,
    read: crate::apps::collab_app_broker::NativeDocumentReadContext,
}
impl BrowserServiceSessionRequestContext {
    pub(crate) fn authenticated(
        peer_id: u64,
        person: String,
        read: crate::apps::collab_app_broker::NativeDocumentReadContext,
    ) -> Self {
        Self {
            peer_id,
            person,
            read,
        }
    }
    /// The context of a connection this device's broker admitted, as the seat answers a call
    /// the broker forwarded to it (`crate::apps::seat`): the broker checked the connection's read
    /// before it forwarded the call, and checks it again after.
    #[must_use]
    pub fn seat_forwarded(
        peer_id: u64,
        person: String,
        read: crate::apps::collab_app_broker::NativeDocumentReadContext,
    ) -> Self {
        Self::authenticated(peer_id, person, read)
    }
    pub fn peer_id(&self) -> u64 {
        self.peer_id
    }
    pub fn app_id(&self) -> &str {
        &self.read.route().app_id
    }
    pub fn workspace_id(&self) -> &str {
        &self.read.route().cluster_id
    }
    pub fn channel(&self) -> String {
        self.read.route().channel_key()
    }
    pub fn person(&self) -> &str {
        &self.person
    }
    pub fn identity_public_key(&self) -> [u8; 32] {
        self.read.certificate().user_public_key
    }
    pub fn read_context(&self) -> &crate::apps::collab_app_broker::NativeDocumentReadContext {
        &self.read
    }
    pub fn revalidate(&self) -> Result<(), String> {
        self.read.revalidate()
    }
}
/// Private credential storage, independent of replicated collaboration state.
pub trait BrowserServiceSessionServices: Send + Sync {
    /// Recheck current route Read permission and the session's own person.
    fn authorize(&self, context: &BrowserServiceSessionRequestContext) -> Result<(), String>;
    /// Mutations require a provider's explicit live Admin authorization.
    fn authorize_command(
        &self,
        context: &BrowserServiceSessionRequestContext,
        command: &runtime_collab::BrowserServiceSessionCommand,
    ) -> Result<(), String> {
        if matches!(
            command,
            runtime_collab::BrowserServiceSessionCommand::Forget { .. }
        ) {
            return Err("Website session Admin authorization unavailable".into());
        }
        self.authorize(context)
    }
    /// Answer an authorized command that needs nobody's word but the service's: whether this
    /// session lends vault items for a page
    /// ([`runtime_collab::BrowserServiceSessionCommand::LendsItems`]). Awaited, never waited for
    /// on a thread: the service may be the seat's, asked over a link the asking thread carries.
    fn command(
        self: Arc<Self>,
        context: &BrowserServiceSessionRequestContext,
        command: runtime_collab::BrowserServiceSessionCommand,
    ) -> ItemAnswering;
    /// Answer an item's use ([`runtime_collab::BrowserServiceSessionCommand::UseItem`]),
    /// later: whoever keeps the vault is asked outside this process, and the connection
    /// that asked is not kept waiting on its dispatch. Everything the service decides is an
    /// answer of its own, a refusal with its reason among them
    /// ([`runtime_collab::BrowserServiceSessionReply::ItemRefused`]); an error is for what
    /// is no item's use at all. A service that has no vault items used says so at once.
    fn item_use(
        &self,
        context: &BrowserServiceSessionRequestContext,
        command: runtime_collab::BrowserServiceSessionCommand,
    ) -> ItemAnswering {
        let _ = context;
        let answer = command
            .item_use()
            .map(|invocation| {
                runtime_collab::BrowserServiceSessionReply::item_refused(
                    invocation,
                    "This session has no vault items used for a page, so nothing was released.",
                )
            })
            .ok_or_else(|| "not an item's use".to_owned());
        Box::pin(std::future::ready(answer))
    }
    /// Keep or release an account's password
    /// ([`runtime_collab::BrowserServiceSessionCommand::account_secret`]), or list, resolve or
    /// forget one of the workspace's website sessions
    /// ([`runtime_collab::BrowserServiceSessionCommand::website_session`]), later: the
    /// workspace's vault and accounts are read and written on this device, and the connection
    /// that asked is not kept waiting on its dispatch. A service that keeps neither refuses.
    fn answered_later(
        &self,
        context: &BrowserServiceSessionRequestContext,
        command: runtime_collab::BrowserServiceSessionCommand,
    ) -> ItemAnswering {
        let _ = (context, command);
        Box::pin(std::future::ready(Err(
            crate::workspaces::account_logins::VAULT_NOT_HERE.to_owned(),
        )))
    }
    fn disconnect(&self, peer_id: u64);
}

/// The answer to an item's use, as [`BrowserServiceSessionServices::item_use`] hands it
/// back to be waited for.
pub type ItemAnswering = std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Result<runtime_collab::BrowserServiceSessionReply, String>>
            + Send
            + 'static,
    >,
>;

/// The answer to a Files clipboard command, as [`FileClipboardServices::command`] hands it back.
pub type ClipboardAnswering = std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Result<runtime_collab::file_clipboard::Reply, String>>
            + Send
            + 'static,
    >,
>;

/// A wait for a host service's next change, as each `changed` method returns it.
pub type ChangeWait = std::pin::Pin<Box<dyn std::future::Future<Output = u64> + Send + 'static>>;

/// A descriptor source sealed to the kernel-attested local process.
pub(crate) type SelectedDescriptorSource =
    Arc<dyn Fn(u32) -> Result<std::fs::File, String> + Send + Sync>;

/// Sealed local Files admission: its callback rechecks the original caller, live connection and
/// workspace policy, never a JSON-claimed actor.
#[derive(Clone)]
pub struct FileClipboardContext {
    pub(crate) peer: u64,
    pub(crate) window: String,
    /// The app whose window asked: the one the broker admitted as Files.
    pub(crate) app_id: String,
    pub(crate) duplicate: SelectedDescriptorSource,
    pub(crate) workspace: String,
    pub(crate) owner: tilo_action_policy::Actor,
    pub(crate) check: Arc<dyn Fn(tilo_actions::ActionKind) -> Result<(), String> + Send + Sync>,
}
impl FileClipboardContext {
    /// The context of a Files window this device's broker admitted, as the seat answers a
    /// clipboard command the broker forwarded to it (`crate::apps::seat`): the broker checked the
    /// caller for what the command needs before it forwarded it, and checks again after; the
    /// objects Files selected came with the call, in `selected` by the numbers the command names
    /// them by.
    #[must_use]
    pub fn seat_forwarded(
        (peer, window, app_id): (u64, String, String),
        (workspace, owner): (String, tilo_action_policy::Actor),
        selected: std::collections::HashMap<u32, std::fs::File>,
    ) -> Self {
        Self {
            peer,
            window,
            app_id,
            duplicate: Arc::new(move |descriptor| {
                selected
                    .get(&descriptor)
                    .ok_or("Files did not hand over this selected descriptor")?
                    .try_clone()
                    .map_err(|error| error.to_string())
            }),
            workspace,
            owner,
            check: Arc::new(|_| Ok(())),
        }
    }
    pub fn window(&self) -> &str {
        &self.window
    }
    /// Duplicate an object retained by this exact launcher-attested Files process.
    pub fn duplicate_selected(&self, descriptor: u32) -> Result<std::fs::File, String> {
        self.authorize(tilo_actions::ActionKind::Read)?;
        let file = (self.duplicate)(descriptor)?;
        self.authorize(tilo_actions::ActionKind::Read)?;
        Ok(file)
    }
    pub fn peer(&self) -> u64 {
        self.peer
    }
    /// The app whose window asked.
    pub fn app_id(&self) -> &str {
        &self.app_id
    }
    pub fn workspace(&self) -> &str {
        &self.workspace
    }
    pub fn owner(&self) -> &tilo_action_policy::Actor {
        &self.owner
    }
    /// Recheck the exact caller against live ownership and workspace policy.
    pub fn authorize(&self, permission: tilo_actions::ActionKind) -> Result<(), String> {
        (self.check)(permission)
    }
}
pub trait FileClipboardServices: Send + Sync {
    /// Perform a bounded command under the sealed local admission. Awaited, never waited for on
    /// a thread: the clipboard may be the seat's, asked over a link the asking thread carries.
    fn command(
        self: Arc<Self>,
        context: &FileClipboardContext,
        command: runtime_collab::file_clipboard::Command,
    ) -> ClipboardAnswering;
    fn disconnect(&self, peer: u64);
}

/// Where a host spends a grant given once in a workspace's grants, as the call it admits is
/// decided. The host owns the workspace records it is in.
pub trait WorkspaceGrants: Send + Sync {
    /// Spend `grant`, given once in `workspace`, for the call being admitted now: true for
    /// exactly one call, which may go on; false when another call spent it first, and this one
    /// is refused. The grant is then taken back from the workspace.
    fn spend(&self, workspace: &str, grant: &str) -> bool;
}

/// The identity of an exact private Settings worker, derived by the broker. It crosses to the seat
/// (`crate::apps::seat`) as the broker derived it, and only the broker reaches the seat.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct DisplayControlContext {
    pub(crate) workspace: String,
    pub(crate) root: [u8; 32],
    pub(crate) device: [u8; 32],
}
impl DisplayControlContext {
    pub fn workspace(&self) -> &str {
        &self.workspace
    }
    pub fn root(&self) -> [u8; 32] {
        self.root
    }
    pub fn device(&self) -> [u8; 32] {
        self.device
    }
}
/// Where the records of a workspace another device hosts run their actions.
pub trait HostedWorkspaces: Send + Sync {
    /// The actions of `workspace`'s host, when a host other than this device
    /// holds it and runs its records' actions in its own workers: what the
    /// person's invocations of those actions are sent to, decided and run.
    /// `None` for a workspace this device hosts itself or holds alone.
    fn host_actions(
        &self,
        workspace: &str,
    ) -> Option<Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>>;

    /// Where a call to `app`'s actions in `workspace` runs, when the app keeps a long-lived
    /// connection ([`crate::work::connection_call`]): here, or the actions of the device that
    /// has the connection's lease, which runs the app's process. Where no device has it, why;
    /// nothing starts here for the call.
    fn connection_actions<'a>(
        &'a self,
        workspace: &'a str,
        app: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ConnectionActions> + Send + 'a>> {
        let _ = (workspace, app);
        Box::pin(std::future::ready(ConnectionActions::Here))
    }

    /// The actions of `device`, a device of `workspace`, the way a call to an app's connection
    /// reaches the device that runs it ([`Self::connection_actions`]). `None` where this device
    /// cannot reach it.
    fn reach(
        &self,
        workspace: &str,
        device: [u8; 32],
    ) -> Option<tilo_peer_state::actions::RemoteDevice> {
        let _ = (workspace, device);
        None
    }
}

/// Where a call to an app that keeps a long-lived connection runs
/// ([`HostedWorkspaces::connection_actions`]).
pub enum ConnectionActions {
    /// Here, as any app's call.
    Here,
    /// On the device that has the connection's lease, through its actions.
    On(Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>),
    /// Nowhere yet, and why.
    Unplaced(String),
}

/// A resource command's certified route and caller, supplied by the broker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceRequestContext {
    app_id: String,
    workspace_id: String,
    caller: tilo_actions::remote::Caller,
    /// The invocations the workspace service delivered to this route's app that it is still carrying out,
    /// each with who invoked it: a write that serves one is that invoker's, an agent acting for
    /// the caller included.
    serving: Vec<(tilo_actions::InvocationId, tilo_actions::Acting)>,
    /// The caller the device opened this route's view for, when it opened one for a caller of
    /// its dispatch: a write the app makes outside an invocation it serves is theirs, limited
    /// to what they may do, and a read answers only what they may read.
    opened_for: Option<tilo_actions::remote::Caller>,
    /// The record the route's channel opened, whose changes `RecordChanges` reads and
    /// `RecordChange` makes.
    record: Option<String>,
    /// Why the caller may not found the route's record in a workspace this device governs.
    founding_refusal: Option<String>,
    /// The kind of the record the route's channel opened, as the app joined it.
    kind: Option<String>,
    /// Whether the route serves the app's own process, its connection to the device: what it
    /// writes outside an invocation it serves is the app's own. The device's own callers (the
    /// shell, a runtime of the device's) write for the device's person.
    app_process: bool,
}

impl ResourceRequestContext {
    pub fn authenticated(
        app_id: impl Into<String>,
        workspace_id: impl Into<String>,
        caller: tilo_actions::remote::Caller,
    ) -> Self {
        Self {
            app_id: app_id.into(),
            workspace_id: workspace_id.into(),
            caller,
            serving: Vec::new(),
            opened_for: None,
            record: None,
            founding_refusal: None,
            kind: None,
            app_process: false,
        }
    }

    /// The context of a route that serves the app's own process, its connection to the device.
    #[must_use]
    pub const fn of_app_process(mut self) -> Self {
        self.app_process = true;
        self
    }

    /// Whether the route serves the app's own process ([`Self::of_app_process`]).
    #[must_use]
    pub const fn serves_app_process(&self) -> bool {
        self.app_process
    }

    /// The context of a route whose channel opened a record of `kind`.
    #[must_use]
    pub fn of_kind(mut self, kind: Option<String>) -> Self {
        self.kind = kind;
        self
    }

    /// The kind of the record the route's channel opened, when the app named it.
    #[must_use]
    pub fn kind(&self) -> Option<&str> {
        self.kind.as_deref()
    }

    /// The context of a route whose caller may not found its record where this device governs
    /// the workspace, and why.
    #[must_use]
    pub fn refusing_founding(mut self, refusal: Option<String>) -> Self {
        self.founding_refusal = refusal;
        self
    }

    /// Why the caller may not found the route's record where this device governs the workspace.
    #[must_use]
    pub fn founding_refusal(&self) -> Option<&str> {
        self.founding_refusal.as_deref()
    }

    /// The context of a route whose channel opened the record `record`.
    #[must_use]
    pub fn on_record(mut self, record: impl Into<String>) -> Self {
        self.record = Some(record.into());
        self
    }

    /// The record the route's channel opened, when it opened one.
    #[must_use]
    pub fn record(&self) -> Option<&str> {
        self.record.as_deref()
    }

    /// The context of a view the device opened for `opened_for`, when it opened it for a caller
    /// of its dispatch rather than for the person at a window of its own.
    #[must_use]
    pub fn opened_for(mut self, opened_for: Option<tilo_actions::remote::Caller>) -> Self {
        self.opened_for = opened_for;
        self
    }

    /// The caller this route's view was opened for, when it was opened for one.
    #[must_use]
    pub const fn viewer(&self) -> Option<&tilo_actions::remote::Caller> {
        self.opened_for.as_ref()
    }

    /// The caller this route reads as: the one its view was opened for, else the caller it
    /// serves. Never the person whose device serves it, unless that is who called.
    #[must_use]
    pub fn reader(&self) -> &tilo_actions::remote::Caller {
        self.opened_for.as_ref().unwrap_or(&self.caller)
    }

    /// The context, with the invocations `serving` the workspace service delivered to its app and is
    /// waiting on, each with who invoked it.
    /// A route's context as the broker certifies it, for a test that stands in for the broker.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn for_tests(
        app_id: impl Into<String>,
        workspace_id: impl Into<String>,
        caller: tilo_actions::remote::Caller,
    ) -> Self {
        Self::authenticated(app_id, workspace_id, caller)
    }

    #[must_use]
    pub fn serving<A: Into<tilo_actions::Acting>>(
        mut self,
        serving: impl IntoIterator<Item = (tilo_actions::InvocationId, A)>,
    ) -> Self {
        self.serving = serving
            .into_iter()
            .map(|(invocation, acting)| (invocation, acting.into()))
            .collect();
        self
    }

    /// Who invoked `invocation`, and how they acted, when it is one the workspace service
    /// delivered to this route's app and the app is still carrying out.
    #[must_use]
    pub fn invoker_of(&self, invocation: &str) -> Option<&tilo_actions::Acting> {
        self.serving
            .iter()
            .find(|(id, _)| id.0.to_string() == invocation)
            .map(|(_, acting)| acting)
    }

    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    #[must_use]
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    #[must_use]
    pub fn caller(&self) -> &tilo_actions::remote::Caller {
        &self.caller
    }
}

/// The store's answer to one resource command, once it has it.
pub type ResourceAnswer = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<collab_types::store::wire::Reply, String>> + Send>,
>;

/// The device's one store, as apps write and read their resources in it: an app holds no store
/// of its own.
pub trait ResourceServices: Send + Sync {
    /// Answers `command` of the app `context` names, in its workspace: a write as the person the
    /// app acts for, a read of only what they may read.
    fn command(
        &self,
        context: &ResourceRequestContext,
        command: collab_types::store::wire::Command,
    ) -> ResourceAnswer;

    /// Holds `query` open for the app `context` names: its first page, then each page a change
    /// makes different, until the app lets go of the receiver.
    fn open(
        &self,
        context: &ResourceRequestContext,
        query: collab_types::store::Query,
    ) -> ResourcePages;
}

/// The pages of a query an app holds open, once the store opened it.
pub type ResourcePages = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = Result<
                    tokio::sync::mpsc::UnboundedReceiver<collab_types::store::Page>,
                    String,
                >,
            >,
    >,
>;

/// The native host's separately authorized display transaction service.
pub trait DisplayControlServices: Send + Sync {
    fn command(&self, context: &DisplayControlContext, command: &[u8]) -> Result<Vec<u8>, String>;
}

/// Whether `app` may found a record of the resource kind `kind`.
pub type RecordFounding = Arc<dyn Fn(&str, &str) -> bool + Send + Sync>;

/// The shell's link to the device's daemon, or why it has
/// none. Only a Unix desktop session has a shell.
#[cfg(unix)]
pub type DeviceDaemon = Result<runtime_collab::ShellClient, String>;

/// The workspace's own folder, as the dispatch reaches it: where a `files#open` given content
/// puts its files. A host that keeps no tree of its workspaces serves none, and that open is
/// refused there.
pub trait FilesTreePuts: Send + Sync {
    /// Put what `arguments` name into `workspace`'s tree, answering where each file is or how
    /// many went in.
    ///
    /// `confirm` is asked once every file is named and before the tree is touched: whether the
    /// workspace still lets the caller write its files, as a grant taken back while the content
    /// was read takes the change away.
    ///
    /// # Errors
    /// Why the tree did not take the files, in words a person reads.
    fn put(
        &self,
        workspace: tilo_server_protocol::WorkspaceId,
        arguments: &serde_json::Map<String, serde_json::Value>,
        acting: Option<tilo_actions::ActionContext>,
        confirm: &dyn Fn() -> bool,
    ) -> Result<serde_json::Value, String>;
}

#[cfg(all(feature = "files", not(target_arch = "wasm32")))]
impl FilesTreePuts for crate::files::OpenedFiles {
    fn put(
        &self,
        workspace: tilo_server_protocol::WorkspaceId,
        arguments: &serde_json::Map<String, serde_json::Value>,
        acting: Option<tilo_actions::ActionContext>,
        confirm: &dyn Fn() -> bool,
    ) -> Result<serde_json::Value, String> {
        crate::files::OpenedFiles::put(self, workspace, arguments, acting, confirm)
    }
}

#[derive(Clone, Default)]
pub struct ProcessServices {
    /// Read-only existing-resource discovery, absent unless the host can carry it.
    pub committed_documents: Option<Arc<dyn NativeDocumentReads>>,
    pub native_media: Option<Arc<dyn NativeMediaServices>>,
    pub audio_output: Option<Arc<dyn AudioOutputServices>>,
    pub workspace_memberships: Option<Arc<dyn WorkspaceMemberships>>,
    /// The workspaces' agents, as their records name them; without it no agent joins.
    pub agents: Option<Arc<dyn AgentRoster>>,
    /// The devices each person is present on, as this device hears them; without it who is here
    /// names no device.
    pub present_devices: Option<Arc<dyn crate::workspaces::PresentDevices>>,
    /// The agent sessions this host serves: the MCP connections a person admitted, each with what
    /// its person allowed it.
    pub agent_sessions: Option<Arc<dyn AgentSessions>>,
    pub native_certificates:
        Option<Arc<dyn crate::apps::certificate_authority::CertificateAuthority>>,
    pub local_person: Option<Arc<dyn LocalPersonServices>>,
    pub resource_changes: Option<Arc<dyn DocumentChangeServices>>,
    /// Where a host with no [`NativePeerBrokerConfig`](crate::apps::NativePeerBrokerConfig)
    /// keeps the invocation journal; without it the broker keeps none.
    pub journal_root: Option<std::path::PathBuf>,
    pub launcher: Option<Arc<dyn LauncherServices>>,
    /// The resources this session's services publish, which its apps read, watch and invoke;
    /// without it those calls are refused.
    pub published: Option<Arc<dyn PublishedResources>>,
    /// The workspace's own folder, where a `files#open` given content puts its files; without it
    /// that open is refused, and Files' open of nothing still starts the app.
    pub files_open: Option<Arc<dyn FilesTreePuts>>,
    pub workspace_governance: Option<Arc<dyn WorkspaceGovernanceServices>>,
    pub shortcut_library: Option<Arc<dyn ShortcutLibraryServices>>,
    /// The device's keyboard shortcuts on the action api; without it they are not offered.
    pub keyboard_shortcuts: Option<Arc<dyn KeyboardShortcutServices>>,
    /// The providers registered on this device, the steps the host runs itself among them
    /// ([`crate::workspaces::providers`]): each answers under its own provider id.
    pub providers: Option<crate::workspaces::providers::Providers>,
    pub continuations: Option<Arc<dyn ContinuationServices>>,
    /// The session's persistent search index; without it apps' content is findable only while
    /// they run.
    pub search_index: Option<Arc<dyn SearchIndexServices>>,
    /// The device content store content puts go to; without it that api is not offered.
    pub content_store: Option<Arc<dyn ContentStoreServices>>,
    pub workspace_control: Option<Arc<dyn WorkspaceControlServices>>,
    /// The session's settings, which apps reach through their channel; without it apps have none beyond
    /// their own process.
    pub settings: Option<Arc<dyn settings_store::SettingsSource>>,
    pub display_control: Option<Arc<dyn DisplayControlServices>>,
    /// Every app's live activity, as the host presents it; without it no app shows anything
    /// outside its windows.
    pub live_activities: Option<Arc<runtime_collab::LiveActivities>>,
    pub browser_service_sessions: Option<Arc<dyn BrowserServiceSessionServices>>,
    pub file_clipboard: Option<Arc<dyn FileClipboardServices>>,
    /// What each application declared it may do on the API; without it none declared anything.
    pub app_action_access: Option<crate::apps::app_actions::AppActionAccessResolver>,
    /// The resource kinds each application declared; without it no app's reach is known.
    pub app_resource_kinds: Option<crate::apps::app_actions::AppResourceKindsResolver>,
    /// Which kinds an app may found records in, in a workspace this device governs; without it
    /// an app founds records in any kind.
    pub record_founding: Option<RecordFounding>,
    /// Where this host records the answers a person gave for its workspaces'
    /// grants.
    pub workspace_grants: Option<Arc<dyn WorkspaceGrants>>,
    /// Where the launchable action catalogue survives a restart; without it it lives as long as
    /// this broker.
    pub launchable_catalogue: Option<std::path::PathBuf>,
    /// Content records reference, kept here and asked of its route; without it apps' content stays
    /// in their own process.
    pub document_content: Option<Arc<crate::content::RouteContent>>,
    /// This device's channel encryption service, which tells each route's endpoint when its
    /// channel keys changed (`groupKeysChanged`).
    pub group_keys: Option<crate::apps::workspace_keys::KeyProvider>,
    /// Where an admitted offloadable action runs (`tilo_actions::offload`); without it every one
    /// runs here.
    pub action_offload: Option<Arc<dyn tilo_actions::offload::ActionOffload>>,
    /// Carries an application's action to another of the person's devices; without it none does.
    pub own_device: Option<Arc<dyn OwnDeviceActions>>,
    /// Asks the workspace a node of its tree is in to share it, end a share of
    /// it, or say what the person may do there. Without it no app's share
    /// reaches a workspace, and the broker refuses it rather than let a share
    /// promise access no grant backs.
    pub workspace_nodes: Option<Arc<dyn WorkspaceNodeActions>>,
    /// The hosts of the workspaces another device hosts, where the person's
    /// invocations of those workspaces' record actions run. Without it every
    /// action runs here.
    pub hosted_workspaces: Option<Arc<dyn HostedWorkspaces>>,
    /// The device's one store, which apps write and read their resources in; without it those
    /// commands are refused.
    pub resources: Option<Arc<dyn ResourceServices>>,
    /// What the person's shell is answered with, on a connection admitted as that shell by the
    /// link the daemon handed it ([`crate::apps::shell`]); without it no connection is the shell.
    pub shell: Option<Arc<crate::apps::shell::ShellServices>>,
}

impl ProcessServices {
    /// The names of these services, in the order the struct declares them: a field added
    /// above belongs here too, and a host's own test compares its decisions to this list.
    pub const NAMES: &[&str] = &[
        "committed_documents",
        "native_media",
        "audio_output",
        "workspace_memberships",
        "agents",
        "present_devices",
        "agent_sessions",
        "native_certificates",
        "local_person",
        "resource_changes",
        "journal_root",
        "launcher",
        "published",
        "files_open",
        "workspace_governance",
        "shortcut_library",
        "keyboard_shortcuts",
        "providers",
        "continuations",
        "search_index",
        "content_store",
        "workspace_control",
        "settings",
        "display_control",
        "live_activities",
        "browser_service_sessions",
        "file_clipboard",
        "app_action_access",
        "app_resource_kinds",
        "record_founding",
        "workspace_grants",
        "launchable_catalogue",
        "document_content",
        "group_keys",
        "action_offload",
        "own_device",
        "workspace_nodes",
        "hosted_workspaces",
        "resources",
        "shell",
    ];
}
