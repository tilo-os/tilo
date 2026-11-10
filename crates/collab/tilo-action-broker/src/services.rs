//! The host services this broker's own transport serves natively: launching applications,
//! workspace memberships, browser sessions, and the contexts a certified connection arrives with.
//! The rest lives in [`tilo_broker_core::services`] and is re-exported here.
pub use tilo_broker_core::services::*;

mod published;
pub use published::{Caller, Granted, PublishedResources, Reading, ResourceWatch};

/// A write on `route` was admitted to this device's channels. Where it is a
/// workspace's tracker, its readers (the broker's runs, the composer following
/// one) are told to read it again, as the owner's publication tells them in
/// a workspace it commits: a sealed workspace's tracker changes only here.
pub fn tracker_written(route: &tilo_broker_core::route::CrdtRoute) {
    if route.app_id == TRACKER_APP && route.record_id == TRACKER_RECORD {
        tilo_identity_host::policy_change::announce();
    }
}

use std::sync::Arc;
use tilo_desktop_model::{Workspace, WorkspaceId};

pub use crate::collab_app_broker::{
    AudioOutputFuture, AudioOutputServices, AwaitingHistory, CertifiedPersonContext, DocumentBytes,
    HeldRevision, NativeDocumentRead, NativeDocumentReadContext, NativeDocumentReads,
    NativeMediaFuture, NativeMediaRequestContext, NativeMediaServices, RecordRead,
};

/// A workspace's status once it changed ([`WorkspaceMemberships::workspace_status_change`]).
pub type WorkspaceStatusFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = runtime_collab::WorkspaceStatus> + Send>>;

/// Native workspace membership owner. Public callers choose only a closed typed
/// command; this host service supplies the private governor signing custody.
pub trait WorkspaceMemberships: Send + Sync {
    /// Recheck custody, retained integrity and all current source rights before
    /// disclosing a completed checkpoint invocation, including journal retries.
    fn checkpoint_result_access(
        &self,
        _workspace: &str,
        _caller: &tilo_user_identity::DeviceCert,
        _id: uuid::Uuid,
        _mutation: bool,
    ) -> Result<(), String> {
        Err("native checkpoint result access unavailable".into())
    }

    /// Execute an authenticated native checkpoint request with host-owned custody.
    fn checkpoint(
        &self,
        _request: tilo_peer_state::workspace_checkpoint::CheckpointRequest,
    ) -> tilo_peer_state::workspace_checkpoint::CheckpointFuture {
        Box::pin(async { Err("native checkpoint service unavailable".into()) })
    }

    fn requires_workspace(&self, workspace: &str) -> bool;

    /// Whether this device keeps `workspace`'s checkpoints. They are its
    /// governor's custody, so a workspace this device governs offers them
    /// whichever model its records follow, and a workspace governed elsewhere
    /// does not.
    fn keeps_checkpoints(&self, workspace: &str) -> bool {
        self.requires_workspace(workspace)
    }

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

    /// Whether an app opening `workspace` as `certificate`'s person first has
    /// the workspace let `app` author there: the workspace does not let it yet, and
    /// that person may grant it. Every opening of an app asks this, the
    /// launch of one and the join of a tab the session restored alike, and
    /// grants through [`WorkspaceMembershipCommand::GrantAppAuthorship`].
    ///
    /// [`WorkspaceMembershipCommand::GrantAppAuthorship`]: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::GrantAppAuthorship
    fn app_opens_by_granting(
        &self,
        _workspace: &str,
        _app: &str,
        _certificate: &tilo_user_identity::DeviceCert,
    ) -> bool {
        false
    }

    /// Whether `workspace` is held here but governed by the node that takes part
    /// in its channels: this device writes the workspace's membership changes
    /// there itself, decided by the workspace's governance as they are where
    /// this device governs the workspace itself.
    fn grants_authorship_at_host(&self, _workspace: &str) -> bool {
        false
    }

    /// Whether `workspace`'s membership actions are still to come here: this
    /// device holds the workspace, joins it or has it placed on its host, and its
    /// replica is not projected yet ([`Self::requires_workspace`] or
    /// [`Self::grants_authorship_at_host`] turns true once it is, which
    /// announces a policy change). An app opening in such a workspace waits for
    /// them; one opening in any other workspace unpublished is refused.
    fn membership_pending(&self, _workspace: &str) -> bool {
        false
    }

    /// Whether `workspace`'s host is this device's own, so the authorship an app
    /// opening there needs is recorded here rather than asked of another
    /// device.
    fn hosted_here(&self, _workspace: &str) -> bool {
        false
    }

    /// Confirm that the canonical owner's durable store is still usable.
    fn health(&self) -> Result<(), String> {
        Err("workspace membership owner continuity is unavailable".into())
    }

    fn execute(
        &self,
        request: tilo_peer_state::owner_commit::OwnerCommitRequest,
    ) -> tilo_peer_state::owner_commit::CommitFuture;
}

/// Host-retained original custody for one exact observed launch: no public constructor or wire
/// form, and it never grants rights.
#[derive(Clone)]
pub struct ObservedApplicationLaunch {
    workspace: String,
    app: String,
    package_root: Option<String>,
    check: Arc<dyn Fn() -> Result<tilo_user_identity::DeviceCert, String> + Send + Sync>,
    grant: Arc<ObservedAuthorshipGrant>,
}

type ObservedAuthorshipGrant = dyn Fn(&tilo_user_identity::DeviceCert) -> Result<crate::collab_app_broker::InvocationJob, String>
    + Send
    + Sync;

impl ObservedApplicationLaunch {
    pub(crate) fn new(
        workspace: String,
        app: String,
        package_root: Option<String>,
        check: Arc<dyn Fn() -> Result<tilo_user_identity::DeviceCert, String> + Send + Sync>,
        grant: Arc<ObservedAuthorshipGrant>,
    ) -> Self {
        Self {
            workspace,
            app,
            package_root,
            check,
            grant,
        }
    }

    pub fn matches(&self, app: &str, root: Option<&str>, workspace: &Workspace) -> bool {
        self.app == app
            && self.package_root.as_deref() == root
            && self.workspace == workspace.id().to_string()
    }

    /// Recheck the actual original caller, pending invocation and policy clock.
    pub fn certificate(&self) -> Result<tilo_user_identity::DeviceCert, String> {
        (self.check)()
    }

    /// Submit this bound app's authorship through the normal published door.
    pub fn delegate_authorship(&self) -> Result<crate::collab_app_broker::InvocationJob, String> {
        (self.grant)(&self.certificate()?)
    }
}

pub trait LauncherServices: Send + Sync {
    /// Bind an optional diagnostic observation to the host's current exact
    /// launcher task. The returned ID is correlation only, never authority.
    fn begin_executing_binding_observation(&self, _launcher_pid: u32) -> Option<u64> {
        None
    }
    /// Optional host-side observation only. It grants no authority and receives
    /// neither challenge bytes nor credentials, document or application output.
    fn observe_executing_binding(&self, _observation: u64, _phase: ExecutingBindingPhase) {}
    fn applications(&self) -> Vec<LaunchableApplication>;
    /// The ids installed apps were published under before, as the installed catalogue believes
    /// them (`app_catalogue::apps::earlier_ids`): an application target naming one opens the app
    /// that took its place. None where no app was renamed.
    fn earlier_ids(&self) -> tilo_action_policy::EarlierIds {
        tilo_action_policy::EarlierIds::default()
    }
    /// The same installed entries as `applications`, narrowed to an exact ID.
    /// Hosts may avoid constructing unrelated entries. This is discovery only;
    /// the broker still decides which rows the caller may see or invoke.
    fn applications_for(&self, app_id: &str) -> Vec<LaunchableApplication> {
        self.applications()
            .into_iter()
            .filter(|app| app.package_id == app_id)
            .collect()
    }
    /// Whether the person's own input is behind what app `app_id` asks for
    /// now: a press of theirs (never an agent's) reached one of its windows
    /// within the activation window, and has not proved another open
    /// already. Answering uses it up. A host without that record answers
    /// `false`: an app then opens nothing on its own, only what an
    /// invocation someone else asked of it calls for.
    fn person_initiated(&self, _app_id: &str) -> bool {
        false
    }
    /// Launches `app` into `workspace`, its view of `record` (else the
    /// record it opens by default) opening at `place`.
    fn launch(
        &self,
        app: &str,
        workspace: &Workspace,
        record: Option<&str>,
        place: &LaunchPlace,
    ) -> Result<(), String>;
    /// Launch an exact installed package, or an ordinary host application,
    /// and observe its real startup in the authenticated workspace.
    fn begin_launch(
        &self,
        app: &str,
        package_root: Option<&str>,
        workspace: &Workspace,
        author: Option<&str>,
    ) -> Result<LaunchReadiness, String> {
        let _ = (app, package_root, workspace, author);
        Err("observed application startup is unavailable on this host".into())
    }
    /// Observe startup with host-retained original custody, when available.
    fn begin_launch_from_original(
        &self,
        app: &str,
        package_root: Option<&str>,
        workspace: &Workspace,
        author: Option<&str>,
        original: Option<ObservedApplicationLaunch>,
    ) -> Result<LaunchReadiness, String> {
        if let Some(original) = original {
            if !original.matches(app, package_root, workspace) {
                return Err("original launch target changed".into());
            }
            original.certificate()?;
        }
        self.begin_launch(app, package_root, workspace, author)
    }
    /// Launch `app` into `workspace` at `place`, opening the
    /// content-addressed bytes `content` names read-only, read through the
    /// calling application's own scope. An empty hash is a plain launch. A
    /// host that cannot stage content refuses one.
    ///
    /// Answers what it started: a ticket the started instance presents when
    /// it connects, where the host keeps one.
    fn launch_content(
        &self,
        app: &str,
        workspace: &Workspace,
        content: LaunchContent<'_>,
        place: &LaunchPlace,
    ) -> Result<Launched, String> {
        if content.hash.is_empty() {
            self.launch(app, workspace, None, place)
                .map(|()| Launched::Unconfirmed)
        } else {
            Err("opening launch content is unsupported on this host".into())
        }
    }
    fn attest_process(&self, pid: u32, app: &str, workspace: &str) -> bool;
    /// Have an instance of `app` with no window run in `workspace`, started now
    /// unless one runs there already, to take an invocation of the app's
    /// actions on its records: what a closed app gets in place of a window
    /// for an action that shows nothing. `Some` when this host runs one
    /// there: how its start ends, which the action waits on with its
    /// delivery; `None` when it runs none of `app` (its actions then launch
    /// it as before). It ends with the session.
    ///
    /// # Errors
    /// Why the instance this host runs of `app` there failed.
    fn windowless(&self, app: &str, workspace: &str) -> Result<Option<WindowlessStart>, String> {
        let _ = (app, workspace);
        Ok(None)
    }
    /// Whether this session runs `app`'s model itself, so it has no window process for a launch
    /// to bring up: a plain `open` of it places its root through the session's own `open` step
    /// (`docs/designs/app-language/one-app-model.md` §3) instead of waiting for a process that
    /// never publishes. `None` for a host that launches every app.
    fn served(&self, app: &str) -> Option<String> {
        let _ = app;
        None
    }
    /// Snapshot the original launch facts while admitting this connection.
    /// Unsupported launchers retain no private window-instance authority.
    /// `window_instance` is the window the connection says it was made for:
    /// a host that makes connections for its own windows keys their launch
    /// records by it; a launched process's record names its own.
    fn authenticated_launch(
        &self,
        pid: u32,
        app: &str,
        workspace: &str,
        window_instance: Option<u64>,
    ) -> Result<Option<AuthenticatedApplicationLaunch>, String> {
        let _ = window_instance;
        if !self.attest_process(pid, app, workspace) {
            return Ok(None);
        }
        Ok(Some(AuthenticatedApplicationLaunch {
            creation: self.application_creation_evidence(pid, app, workspace)?,
            continuation_instance: None,
            launch_ticket: None,
            windowless: false,
        }))
    }

    /// Admit kernel-attested executing custody only beneath this exact launch; it grants no new
    /// identity or filesystem permissions.
    #[cfg(target_os = "linux")]
    fn attest_executing_process(
        &self,
        launcher_pid: u32,
        peer: &runtime_portal::process_binding::ExecutingPeer,
        app: &str,
        workspace: &str,
    ) -> bool {
        let _ = (launcher_pid, peer, app, workspace);
        false
    }
    /// Original declarations of this exact live launch, not the currently
    /// installed replacement package. Unsupported hosts grant no creation.
    fn application_creation_evidence(
        &self,
        pid: u32,
        app: &str,
        workspace: &str,
    ) -> Result<Option<ApplicationCreationEvidence>, String> {
        let _ = (pid, app, workspace);
        Ok(None)
    }
    /// An action invocation reached `app` in `workspace`. A person, an agent or
    /// another device drives an app through its actions as much as through
    /// keys and clicks, so a window the app closes after one was closed on
    /// request, not lost.
    fn note_acted_on(&self, app: &str, workspace: &str) {
        let _ = (app, workspace);
    }
    /// Whether [`Self::close`] can close applications on this host.
    fn closes_applications(&self) -> bool {
        false
    }
    /// Ask every window of `app` in `workspace` to close the way a person
    /// closing it would. The next launch starts a fresh process, which picks
    /// up permissions granted since the last one.
    fn close(&self, app: &str, workspace: &Workspace) -> Result<(), String> {
        let _ = (app, workspace);
        Err("closing applications is unsupported on this host".into())
    }
}

/// One checked change to a workspace's saved shortcuts. The broker mints
/// `invocation` and has checked the change against what `actor` (a policy
/// id) is offered.
pub struct ShortcutLibraryRequest {
    pub workspace: String,
    pub actor: String,
    pub change: crate::automation::ShortcutChange,
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

pub use tilo_broker_core::steps::{
    ContinuationServices, HostStepOutcome, HostStepRequest, HostStepServices, LiveRead, LiveView,
    TaskRun,
};

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

/// The broker-authenticated identity passed to native realtime continuity.
/// The service never accepts caller-supplied routing or peer ownership.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_field_names,
    reason = "peer, app and workspace ids are the three distinct identities this context carries"
)]
pub struct RealtimeRequestContext {
    peer_id: u64,
    app_id: String,
    workspace_id: String,
}

impl RealtimeRequestContext {
    pub(crate) fn authenticated(
        peer_id: u64,
        app_id: impl Into<String>,
        workspace_id: impl Into<String>,
    ) -> Self {
        Self {
            peer_id,
            app_id: app_id.into(),
            workspace_id: workspace_id.into(),
        }
    }

    #[must_use]
    pub const fn peer_id(&self) -> u64 {
        self.peer_id
    }

    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    #[must_use]
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }
}

/// Native realtime continuity: the host binds portal capture leases and certified device-fabric
/// state, while the broker owns admission, route binding, wire bounds and disconnect cleanup.
pub trait RealtimeServices: Send + Sync {
    /// Runs one realtime continuity command.
    fn command(
        &self,
        context: &RealtimeRequestContext,
        command_json: &[u8],
    ) -> Result<Vec<u8>, String>;

    /// Subscribes to realtime continuity events for this context.
    fn subscribe(
        &self,
        context: &RealtimeRequestContext,
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>, String>;

    fn disconnect(&self, peer_id: u64);
}
/// The host's speaker groups: the broker admits only the certified Settings route, then hands it
/// the validated command.
pub trait SpeakerServices: Send + Sync {
    /// Runs one validated speaker-group command.
    fn command(&self, command: runtime_collab::SpeakerCommand) -> runtime_collab::SpeakerReply;

    /// Resolves once what a [`runtime_collab::SpeakerCommand::Snapshot`]
    /// would show may have changed since `after` (0 at first, then the value
    /// the previous wait returned), with the value to wait from next. It
    /// names nothing: Settings asks for a snapshot, which the host checks as
    /// it checks every command. `None` when the host cannot say when its
    /// speaker groups change; the broker then answers unimplemented.
    fn changed(&self, after: u64) -> Option<SpeakerChange> {
        let _ = after;
        None
    }
}

/// A wait for the speaker groups' next change, as [`SpeakerServices::changed`]
/// returns it.
pub type SpeakerChange = NearbyTransferChange;

/// The host's drives, as Files lists them: the broker admits only the certified Files route to
/// wait on them.
pub trait DriveServices: Send + Sync {
    /// Resolves once the drive list may have changed since `after` (0 at
    /// first, then the value the previous wait returned), with the value to
    /// wait from next. It names nothing: Files asks for the list again.
    /// `None` when the host cannot say when its drives change; the broker
    /// then answers unimplemented.
    fn changed(&self, after: u64) -> Option<DriveChange>;
}

/// A wait for the drive list's next change, as [`DriveServices::changed`]
/// returns it.
pub type DriveChange = NearbyTransferChange;

/// The person's apps, as Files lists them: the broker admits only the certified Files route to
/// wait on them.
pub trait ApplicationServices: Send + Sync {
    /// Resolves once the app list may have changed since `after` (0 at first,
    /// then the value the previous wait returned), with the value to wait
    /// from next. It names nothing: Files asks for the list again. `None`
    /// when the host cannot say when its apps change; the broker then
    /// answers unimplemented.
    fn changed(&self, after: u64) -> Option<ApplicationChange>;
}

/// A wait for the app list's next change, as
/// [`ApplicationServices::changed`] returns it.
pub type ApplicationChange = NearbyTransferChange;
/// The host's Wi-Fi credential continuity across the person's devices: the broker admits only the
/// certified Settings route. A host without radios installs none.
pub trait WifiContinuityServices: Send + Sync {
    /// Runs one validated Wi-Fi continuity command.
    fn command(
        &self,
        command: runtime_collab::WifiContinuityCommand,
    ) -> runtime_collab::WifiContinuityReply;
}

/// The broker-authenticated connection a Nearby Send command came from: every handle the host
/// issues is bound to this peer and channel, never to what the caller claims.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NearbyTransferRequestContext {
    peer_id: u64,
    app_id: String,
    workspace_id: String,
    channel: String,
    /// Sealed admission identity, never supplied by the command JSON. An
    /// agent is absent here: a local person must answer external trust sheets.
    person: Option<String>,
}

impl NearbyTransferRequestContext {
    pub(crate) fn authenticated(
        peer_id: u64,
        app_id: impl Into<String>,
        workspace_id: impl Into<String>,
        channel: impl Into<String>,
        person: Option<String>,
    ) -> Self {
        Self {
            peer_id,
            app_id: app_id.into(),
            workspace_id: workspace_id.into(),
            channel: channel.into(),
            person,
        }
    }

    /// The context of a certified connection, for a test that drives the
    /// service without the broker admitting a real app on it.
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn for_test(
        peer_id: u64,
        app_id: &str,
        workspace_id: &str,
        channel: &str,
        person: &str,
    ) -> Self {
        Self::authenticated(
            peer_id,
            app_id,
            workspace_id,
            channel,
            Some(person.to_owned()),
        )
    }

    #[must_use]
    pub const fn peer_id(&self) -> u64 {
        self.peer_id
    }

    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    #[must_use]
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// The exact certified channel (app, workspace and record) of the
    /// connection.
    #[must_use]
    pub fn channel(&self) -> &str {
        &self.channel
    }

    #[must_use]
    pub fn person(&self) -> Option<&str> {
        self.person.as_deref()
    }
}

/// The host's Nearby Send between the person's devices: the broker admits only the certified Files
/// route and drops a connection's handles when it goes away.
pub trait NearbyTransferServices: Send + Sync {
    /// Runs one validated Nearby Send command for `context`.
    fn command(
        &self,
        context: &NearbyTransferRequestContext,
        command: runtime_collab::NearbyTransferCommand,
    ) -> Result<runtime_collab::NearbyTransferReply, String>;

    /// Forgets every handle issued to `peer_id`.
    fn disconnect(&self, peer_id: u64);

    /// Resolves once what a [`runtime_collab::NearbyTransferCommand::Snapshot`]
    /// would show `context` may have changed since `after` (0 at first, then
    /// the value the previous wait returned), with the value to wait from
    /// next. It names nothing: the endpoint asks for a snapshot, which the
    /// host checks as it checks every command. `None` when the host cannot
    /// say when its state changes; the broker then answers unimplemented.
    fn changed(
        &self,
        context: &NearbyTransferRequestContext,
        after: u64,
    ) -> Option<NearbyTransferChange> {
        let _ = (context, after);
        None
    }
}

/// Original Browser connection, minted only from certified launcher custody.
#[derive(Clone)]
pub struct BrowserServiceSessionRequestContext {
    peer_id: u64,
    person: String,
    read: crate::collab_app_broker::NativeDocumentReadContext,
}
impl BrowserServiceSessionRequestContext {
    pub(crate) fn authenticated(
        peer_id: u64,
        person: String,
        read: crate::collab_app_broker::NativeDocumentReadContext,
    ) -> Self {
        Self {
            peer_id,
            person,
            read,
        }
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
    pub fn read_context(&self) -> &crate::collab_app_broker::NativeDocumentReadContext {
        &self.read
    }
    pub fn revalidate(&self) -> Result<(), String> {
        self.read.revalidate()
    }
}
/// Private credential custody, independent of replicated collaboration state.
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
        // An account secret needs the custodian's own decision for the app
        // asking; a host that makes none releases and seals nothing.
        if !command.browser_only() {
            return Err("Account custody unavailable".into());
        }
        self.authorize(context)
    }
    /// Execute an authorized website session command.
    fn command(
        &self,
        context: &BrowserServiceSessionRequestContext,
        command: runtime_collab::BrowserServiceSessionCommand,
    ) -> Result<runtime_collab::BrowserServiceSessionReply, String>;
    fn disconnect(&self, peer_id: u64);
}

/// A wait for Nearby Send's next change, as [`NearbyTransferServices::changed`]
/// returns it.
pub type NearbyTransferChange =
    std::pin::Pin<Box<dyn std::future::Future<Output = u64> + Send + 'static>>;

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
    pub(crate) owner: String,
    /// The agent behind this command, when the broker's custody of its
    /// original invocation names an agent acting for the person whose
    /// session Files runs in (`agent:<id>`). Presentation only.
    pub(crate) attributed_agent: Option<String>,
    pub(crate) check: Arc<dyn Fn(tilo_actions::ActionKind) -> Result<(), String> + Send + Sync>,
}
impl FileClipboardContext {
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
    pub fn owner(&self) -> &str {
        &self.owner
    }
    /// The agent (`agent:<id>`) whose invocation this command continues,
    /// acting for the person whose session Files runs in, as the broker's
    /// custody of that invocation names it; `None` for anyone else. What the
    /// transfer writes is recorded as that agent's, for its person. It grants
    /// nothing.
    pub fn attributed_agent(&self) -> Option<&str> {
        self.attributed_agent.as_deref()
    }
    /// Recheck the exact caller against live custody and workspace policy.
    pub fn authorize(&self, permission: tilo_actions::ActionKind) -> Result<(), String> {
        (self.check)(permission)
    }
}
pub trait FileClipboardServices: Send + Sync {
    /// Perform a bounded command under the sealed local admission.
    fn command(
        &self,
        context: &FileClipboardContext,
        command: runtime_collab::file_clipboard::Command,
    ) -> Result<runtime_collab::file_clipboard::Reply, String>;
    fn disconnect(&self, peer: u64);

    /// Name `agent`, an `agent:<id>` principal the broker took from its own
    /// custody of a live Files invocation and that acts for the person whose
    /// session Files runs in, as the cause of this device's next writes to
    /// `paths` (absolute paths under the workspace mount; a folder covers
    /// what is under it). Presentation only: it grants nothing.
    fn attribute_writes(&self, agent: &str, paths: Vec<String>) -> Result<(), String> {
        let _ = (agent, paths);
        Err("write attribution is unavailable".into())
    }
}
/// A grant being recorded ([`AppActionGrants::record`]): its id once it is
/// in force, or why it is not.
pub type GrantRecording =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>;
/// Where grants a person gives an application for another app's action are
/// recorded. The broker decides whether to ask and builds the grant; the
/// host owns the workspace records it lands in.
pub trait AppActionGrants: Send + Sync {
    /// Whether this machine governs `workspace` and `person` may grant in it,
    /// so a grant they give can be recorded here.
    fn may_grant(&self, workspace: &str, person: &str) -> bool;
    /// Record `grant` in `request.workspace` as given by `request.person`, and
    /// note `answer` in the workspace's history. Resolves to the recorded
    /// grant's id once the grant is in force here, so the call it was given
    /// for is admitted with it in place. Otherwise it resolves to why
    /// nothing was granted, worded for the person.
    fn record(
        &self,
        request: &crate::app_actions::AppActionConsentRequest,
        grant: tilo_action_policy::Grant,
        answer: crate::app_actions::AppActionConsent,
    ) -> GrantRecording;
    /// Remove the grant `grant` from `workspace`: a grant given once, now that
    /// its call was decided.
    fn revoke(&self, workspace: &str, grant: &str);
}
/// The identity of an exact private Settings worker, derived by the broker.
#[derive(Clone, Debug)]
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
/// The native host's separately authorized display transaction service.
pub trait DisplayControlServices: Send + Sync {
    fn command(&self, context: &DisplayControlContext, command: &[u8]) -> Result<Vec<u8>, String>;
}

#[derive(Clone, Default)]
pub struct HostServices {
    /// Fixed project-host registrations, unavailable unless actual enforcement is configured.
    pub approved_checks: Option<Arc<dyn crate::collab_app_broker::ApprovedCheckServices>>,
    /// Read-only existing-resource discovery, absent unless the host can carry it.
    pub committed_documents: Option<Arc<dyn NativeDocumentReads>>,
    pub native_media: Option<Arc<dyn NativeMediaServices>>,
    pub audio_output: Option<Arc<dyn AudioOutputServices>>,
    pub workspace_memberships: Option<Arc<dyn WorkspaceMemberships>>,
    /// The Tasks app's tracker, whose issues assigned to an agent are its runs; without it agents
    /// run no tasks.
    pub tracker_runs: Option<Arc<dyn TrackerRuns>>,
    /// The workspaces' agents, as their records name them; without it no agent joins.
    pub agents: Option<Arc<dyn AgentRoster>>,
    pub native_certificates: Option<Arc<dyn crate::certificate_authority::CertificateAuthority>>,
    pub local_person: Option<Arc<dyn LocalPersonServices>>,
    pub record_changes: Option<Arc<dyn RecordChangeServices>>,
    pub legacy_workspace: Option<Arc<dyn LegacyWorkspaceProjection>>,
    pub launcher: Option<Arc<dyn LauncherServices>>,
    /// The resources this session's services publish, which its apps read, watch and invoke;
    /// without it those calls are refused.
    pub published: Option<Arc<dyn PublishedResources>>,
    pub workspace_governance: Option<Arc<dyn WorkspaceGovernanceServices>>,
    pub shortcut_library: Option<Arc<dyn ShortcutLibraryServices>>,
    /// The device's keyboard shortcuts on the action door; without it they are not offered.
    pub keyboard_shortcuts: Option<Arc<dyn KeyboardShortcutServices>>,
    /// The steps the host runs itself, each under its own provider.
    pub host_steps: Vec<Arc<dyn HostStepServices>>,
    pub continuations: Option<Arc<dyn ContinuationServices>>,
    /// The session's persistent search index; without it apps' content is findable only while
    /// they run.
    pub search_index: Option<Arc<dyn SearchIndexServices>>,
    /// What the session hears of a record changing, so search reads it again.
    pub record_index_changes: Option<Arc<dyn RecordIndexChanges>>,
    /// The device content store content puts go to; without it that door is not offered.
    pub content_store: Option<Arc<dyn ContentStoreServices>>,
    pub workspace_control: Option<Arc<dyn WorkspaceControlServices>>,
    pub realtime: Option<Arc<dyn RealtimeServices>>,
    pub speakers: Option<Arc<dyn SpeakerServices>>,
    /// The drives Files lists, which tell its certified route when they change
    /// (`drivesChanged`); without it Files lists none.
    pub drives: Option<Arc<dyn DriveServices>>,
    /// The person's apps Files lists, which tell its certified route when they change
    /// (`applicationsChanged`); without it its Applications place asks on a timer.
    pub applications: Option<Arc<dyn ApplicationServices>>,
    /// The session's settings (`settings_store::app_scope`); without it apps have none beyond
    /// their own process.
    pub settings: Option<Arc<dyn settings_store::SettingsSource>>,
    pub display_control: Option<Arc<dyn DisplayControlServices>>,
    /// Every app's live activity, as the host presents it; without it no app shows anything
    /// outside its windows.
    pub live_activities: Option<Arc<runtime_collab::LiveActivities>>,
    pub wifi_continuity: Option<Arc<dyn WifiContinuityServices>>,
    pub browser_service_sessions: Option<Arc<dyn BrowserServiceSessionServices>>,
    pub nearby_transfer: Option<Arc<dyn NearbyTransferServices>>,
    pub file_clipboard: Option<Arc<dyn FileClipboardServices>>,
    /// What each application declared it may do on the door; without it none declared anything.
    pub app_action_access: Option<crate::app_actions::AppActionAccessResolver>,
    /// Records grants people give apps for other apps' actions; without it no grant is asked.
    pub app_action_grants: Option<Arc<dyn AppActionGrants>>,
    /// Where the launchable action catalogue survives a restart; without it it lives as long as
    /// this broker.
    pub launchable_catalogue: Option<std::path::PathBuf>,
    /// Content records reference, kept here and asked of its route; without it apps' content stays
    /// in their own process.
    pub record_content: Option<Arc<crate::record_content::RouteContent>>,
    /// This device's channel encryption service, which tells each route's endpoint when its
    /// channel keys changed (`channelKeysChanged`).
    pub channel_keys: Option<crate::channel_encryption::ChannelKeyProvider>,
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
    /// The person's own store, which carries the records of their own scope
    /// to their other devices. Without it those records stay on this device.
    pub personal: Option<tilo_peer_state::PersonalSync>,
}

impl HostServices {
    /// The seams of these services, by name, in the order the struct declares them: a field added
    /// above belongs here too, and a host's own test compares its decisions to this list.
    pub const SEAMS: &[&str] = &[
        "approved_checks",
        "committed_documents",
        "native_media",
        "audio_output",
        "workspace_memberships",
        "tracker_runs",
        "agents",
        "native_certificates",
        "local_person",
        "record_changes",
        "legacy_workspace",
        "launcher",
        "published",
        "workspace_governance",
        "shortcut_library",
        "keyboard_shortcuts",
        "host_steps",
        "continuations",
        "search_index",
        "record_index_changes",
        "content_store",
        "workspace_control",
        "realtime",
        "speakers",
        "drives",
        "applications",
        "settings",
        "display_control",
        "live_activities",
        "wifi_continuity",
        "browser_service_sessions",
        "nearby_transfer",
        "file_clipboard",
        "app_action_access",
        "app_action_grants",
        "launchable_catalogue",
        "record_content",
        "channel_keys",
        "action_offload",
        "own_device",
        "workspace_nodes",
        "personal",
    ];
}
