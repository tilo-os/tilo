//! Explicit host services. These are process-owned capabilities, never wire flags.
mod own_device;
mod task_execution;
pub use own_device::{OwnDeviceActions, OwnDeviceOutcome};
pub use task_execution::{NativeTaskActionRequirement, NativeTaskBinding, WorkspaceTaskExecution};

use std::sync::Arc;
use tilo_desktop_model::{DesktopTree, RoomId, Workspace, WorkspaceTaskStatus};

/// Current native seat identity, installed only by the authenticated host.
/// This Rust capability is never exposed through a wire request.
pub trait LocalPersonServices: Send + Sync {
    /// Derive the real local certificate for this person and current room.
    ///
    /// # Errors
    ///
    /// Refuses a missing, replaced or revoked identity and retired room custody.
    fn certificate(
        &self,
        room: &str,
        person: &str,
    ) -> Result<tilo_user_identity::DeviceCert, String>;
}

/// The normal desktop opening deadline, shared with placement retirement.
pub const APPLICATION_STARTUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

pub use crate::collab_broker_capnp::{
    AudioOutputFuture, AudioOutputServices, CertifiedPersonContext, DocumentBytes, HeldRevision,
    NativeDocumentRead, NativeDocumentReadContext, NativeDocumentReads, NativeMediaFuture,
    NativeMediaRequestContext, NativeMediaServices, PreparedElsewhere, PreparingElsewhere,
};

/// One read-only host view lookup. The original admitted channel and exact
/// source record are retained in the existing committed-read context.
pub type NativeResourceViewFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = Result<
                    tilo_actions::remote::CataloguePage,
                    tilo_actions::remote::ActionHostError,
                >,
            > + Send,
    >,
>;

/// Host-installed bridge to the room's authenticated resource publisher.
/// Implementations revalidate the original caller and exact record Read
/// before requesting metadata and before returning it. No action is invoked.
pub trait NativeResourceViews: Send + Sync {
    /// Read one existing resource's inert views and current host locator.
    fn query(
        &self,
        context: NativeDocumentReadContext,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> NativeResourceViewFuture;
}

/// Reads the host's committed authority in place.
/// Mutations go back to that host; this interface owns no replica or persistence.
///
/// Policy questions borrow the committed tree rather than copying it: a copy
/// carries every workspace's strips, tasks and effect-receipt history, and in
/// the compositor it is taken under the tree lock the render loop writes
/// through. One admission asks once and decides every check on that one view.
pub trait WorkspaceAuthority: Send + Sync {
    /// Run `read` once against the committed workspace tree, borrowed.
    ///
    /// `read` must be short: it must not block, await or serialize, because
    /// a host may hold its tree lock while it runs.
    ///
    /// # Errors
    ///
    /// Returns a message when the committed authority cannot be read; `read`
    /// is then not called.
    fn with_tree(&self, read: &mut dyn FnMut(&DesktopTree)) -> Result<(), String>;

    /// Run `read` once against the committed workspace of exactly `room`
    /// (`None` when this host has no such workspace). Same rules as
    /// [`Self::with_tree`]; a host whose rooms are not one tree overrides it
    /// to look up only the requested room.
    ///
    /// # Errors
    ///
    /// Returns a message when the committed authority cannot be read.
    fn with_room(
        &self,
        room: &str,
        read: &mut dyn FnMut(Option<&Workspace>),
    ) -> Result<(), String> {
        self.with_tree(&mut |tree| read(room_in(tree, room)))
    }

    /// An owned copy of the committed workspace tree, for the few callers
    /// that must keep one past the read. Never on an admission path.
    ///
    /// # Errors
    ///
    /// Returns a message when the committed authority cannot be read.
    fn snapshot(&self) -> Result<DesktopTree, String> {
        let mut copy = None;
        self.with_tree(&mut |tree| copy = Some(tree.clone()))?;
        copy.ok_or_else(|| UNANSWERED.to_owned())
    }

    fn legacy_journal_root(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// Whether every write to what the broker's membership and context
    /// checks read on this host is announced through
    /// [`tilo_identity_host::policy_change`]: this tree, the device fleet and
    /// identity registry behind the installed certificate authority, and the
    /// owner's task store. The broker then re-checks on those announcements
    /// and at the next capability or certificate expiry. `false` (the
    /// default) keeps the one-second re-check: a host that does not announce
    /// a revocation must not have it go unenforced.
    fn announces_policy_changes(&self) -> bool {
        false
    }

    /// The delegated agents on `room`'s roster whose person's root signed
    /// the delegation for exactly that device, as this host verified them:
    /// who an app's history names for the changes an agent's own key
    /// signed. Presentation only; nothing is admitted by it. The default
    /// knows none, so every such change stays an unknown participant.
    fn history_agents(&self, _room: &str) -> Vec<ui::history_source::RosterAgent> {
        Vec::new()
    }
}

const UNANSWERED: &str = "workspace authority did not answer";

/// The workspace of exactly `room` in `tree`.
#[must_use]
pub fn room_in<'a>(tree: &'a DesktopTree, room: &str) -> Option<&'a Workspace> {
    tree.workspaces
        .iter()
        .find(|workspace| workspace.room.to_string() == room)
}

impl dyn WorkspaceAuthority {
    /// One policy sample of the whole tree: `decide` runs once on the
    /// borrowed committed tree and its answer is returned.
    ///
    /// # Errors
    ///
    /// Returns a message when the committed authority cannot be read.
    pub fn decide_in_tree<R>(&self, decide: impl FnOnce(&DesktopTree) -> R) -> Result<R, String> {
        let mut decide = Some(decide);
        let mut answer = None;
        self.with_tree(&mut |tree| {
            if let Some(decide) = decide.take() {
                answer = Some(decide(tree));
            }
        })?;
        answer.ok_or_else(|| UNANSWERED.to_owned())
    }

    /// One policy sample of one room: `decide` runs once on the borrowed
    /// committed workspace of exactly `room` and its answer is returned.
    ///
    /// # Errors
    ///
    /// Returns a message when the committed authority cannot be read.
    pub fn decide_in_room<R>(
        &self,
        room: &str,
        decide: impl FnOnce(Option<&Workspace>) -> R,
    ) -> Result<R, String> {
        let mut decide = Some(decide);
        let mut answer = None;
        self.with_room(room, &mut |workspace| {
            if let Some(decide) = decide.take() {
                answer = Some(decide(workspace));
            }
        })?;
        answer.ok_or_else(|| UNANSWERED.to_owned())
    }
}
/// Legacy native projection only; success is not an aggregate durability receipt.
pub trait LegacyWorkspaceProjection: Send + Sync {
    /// Records one effect receipt in the legacy projection.
    ///
    /// # Errors
    ///
    /// Returns a message when the projection cannot record the receipt.
    fn record_effect(
        &self,
        room: &str,
        invocation: tilo_actions::InvocationId,
        actor: &str,
        context: tilo_actions::ActionContext,
        status: tilo_actions::EffectReceiptStatus,
    ) -> Result<(), String>;
    /// Records one task status update in the legacy projection.
    ///
    /// # Errors
    ///
    /// Returns a message when the projection cannot record the update.
    fn record_task_update(
        &self,
        room: &str,
        task: &str,
        actor: &str,
        status: Option<WorkspaceTaskStatus>,
        summary: String,
    ) -> Result<(), String>;
}
pub type WorkspaceAuthorityHandle = Arc<dyn WorkspaceAuthority>;

/// Native room membership owner. Public callers choose only a closed typed
/// command; this host service supplies the private governor signing custody.
pub trait WorkspaceMemberships: Send + Sync {
    /// Recheck custody, retained integrity and all current source rights before
    /// disclosing a completed checkpoint invocation, including journal retries.
    /// # Errors
    /// Refuses stale or damaged results and current permission denial.
    fn checkpoint_result_access(
        &self,
        _room: &str,
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

    fn requires_room(&self, room: &str) -> bool;

    /// Confirm that the canonical owner's durable store is still usable.
    ///
    /// # Errors
    ///
    /// Returns a diagnostic while owner continuity is unavailable or fenced.
    fn health(&self) -> Result<(), String> {
        Err("workspace membership owner continuity is unavailable".into())
    }

    /// Read canonical coordination state directly from the admitted owner.
    /// # Errors
    /// Refuses unavailable or fenced owner state; never substitutes presentation data.
    fn task_snapshot(
        &self,
        room: &str,
    ) -> Result<tilo_peer_state::workspace_task::WorkspaceTaskSnapshot, String>;

    fn execute(
        &self,
        request: tilo_owner_commit::OwnerCommitRequest,
    ) -> tilo_owner_commit::CommitFuture;
}

/// Where the view of a launched app opens: next to where the actor that
/// needs it already is, so the person and every actor keep their place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchPlace {
    /// Where the person on this device is looking: their own invocation,
    /// from the palette, a chord or a click.
    Focus,
    /// Beside the view of `app_id` the actor works in, on whichever strip
    /// holds it, which need not be the one the person is looking at.
    Beside { app_id: String },
}

#[derive(Clone)]
pub struct LaunchableApplication {
    pub package_id: String,
    pub display_name: String,
    /// A host application Tilo did not package: it runs outside every Tilo
    /// sandbox, with the user's own files, and publishes no typed actions.
    pub unconfined: bool,
    /// What its signed manifest offers while it is not running
    /// (`[[offers]]`): listed in a workspace where it has published nothing
    /// yet. Invoking one launches it, and its published action admits the
    /// invocation.
    pub offers: Vec<tilo_app_package::manifest::OfferedAction>,
}
/// The application a launcher-authenticated connection belongs to, and the
/// workspace it was launched into. Launch content is only ever read through
/// this scope: holding a hash is not access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchScope {
    pub app_id: String,
    pub workspace_id: String,
}

/// What a launch opens: the lowercase BLAKE3 `hash` of content-addressed
/// bytes (empty for a plain launch) and the application that asked, whose
/// scope the bytes are read through.
#[derive(Clone, Copy, Debug)]
pub struct LaunchContent<'a> {
    pub hash: &'a str,
    pub caller: Option<&'a LaunchScope>,
    /// The person who opened the app, when they may change the workspace:
    /// the host records them granting it authorship, so it launches as an
    /// author of the space rather than a viewer. `None` launches a viewer.
    pub author: Option<&'a str>,
}

/// Original signed package evidence retained by the host for an actual launch.
/// This has no wire decoder and is never supplied by the joining application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationCreationEvidence {
    pub package_hash: String,
    pub declared_kinds: Vec<String>,
}

impl ApplicationCreationEvidence {
    #[must_use]
    pub fn valid(&self) -> bool {
        self.package_hash.len() == 64
            && self
                .package_hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            && !self.declared_kinds.is_empty()
            && self.declared_kinds.len() <= 256
            && self.declared_kinds.iter().all(|kind| {
                !kind.is_empty() && kind.len() <= 256 && !kind.chars().any(char::is_control)
            })
    }
}

/// One admitted launch's actual startup observation. Dropping an unanswered
/// observation retires its host-side subscription; enqueueing a worker is
/// never sufficient to produce this receipt.
pub struct LaunchReadiness {
    receiver: tokio::sync::oneshot::Receiver<Result<(), String>>,
    adoption: Option<tokio::sync::oneshot::Receiver<Option<LaunchAdoption>>>,
    retire: Option<Box<dyn FnOnce() + Send>>,
}

/// Host-observed public window identity, never an application-claimed PID.
/// Kernel process attribution is available only through device-owner control.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchAdoption {
    /// Host-issued identity of this exact Wayland resource lifetime; not a grant.
    pub adoption_id: uuid::Uuid,
    pub surface_id: u64,
    pub package_id: String,
    pub room: String,
}

impl LaunchReadiness {
    pub fn new(
        receiver: tokio::sync::oneshot::Receiver<Result<(), String>>,
        retire: impl FnOnce() + Send + 'static,
    ) -> Self {
        Self {
            receiver,
            adoption: None,
            retire: Some(Box::new(retire)),
        }
    }

    #[must_use]
    pub fn with_adoption(
        mut self,
        receiver: tokio::sync::oneshot::Receiver<Option<LaunchAdoption>>,
    ) -> Self {
        self.adoption = Some(receiver);
        self
    }

    /// Wait for an adopted window or a terminal startup failure.
    ///
    /// # Errors
    /// Returns the launcher failure, or that its observer disappeared.
    pub async fn wait(self) -> Result<(), String> {
        self.wait_with_adoption().await.map(|_| ())
    }

    /// Observe the adopted window association when the native host supplies custody.
    /// Legacy hosts still settle readiness without inventing an association.
    ///
    /// # Errors
    /// Returns the actual startup failure or a disconnected observation.
    pub async fn wait_with_adoption(mut self) -> Result<Option<LaunchAdoption>, String> {
        (&mut self.receiver)
            .await
            .map_err(|_| "the application startup observer stopped".to_string())??;
        match self.adoption.take() {
            Some(receiver) => receiver
                .await
                .map_err(|_| "the adopted window observer stopped".into()),
            None => Ok(None),
        }
    }
}

impl Drop for LaunchReadiness {
    fn drop(&mut self) {
        if let Some(retire) = self.retire.take() {
            retire();
        }
    }
}

/// Host-retained original custody for one exact observed application launch.
/// No public constructor or wire representation exists. It never grants rights:
/// the canonical membership action independently checks current administration.
#[derive(Clone)]
pub struct ObservedApplicationLaunch {
    room: String,
    app: String,
    package_root: Option<String>,
    check: Arc<dyn Fn() -> Result<tilo_user_identity::DeviceCert, String> + Send + Sync>,
    grant: Arc<ObservedAuthorshipGrant>,
}

type ObservedAuthorshipGrant = dyn Fn(&tilo_user_identity::DeviceCert) -> Result<crate::collab_broker_capnp::InvocationJob, String>
    + Send
    + Sync;

impl ObservedApplicationLaunch {
    pub(crate) fn new(
        room: String,
        app: String,
        package_root: Option<String>,
        check: Arc<dyn Fn() -> Result<tilo_user_identity::DeviceCert, String> + Send + Sync>,
        grant: Arc<ObservedAuthorshipGrant>,
    ) -> Self {
        Self {
            room,
            app,
            package_root,
            check,
            grant,
        }
    }

    pub fn matches(&self, app: &str, root: Option<&str>, workspace: &Workspace) -> bool {
        self.app == app
            && self.package_root.as_deref() == root
            && self.room == workspace.room.to_string()
    }

    /// Recheck the actual original caller, pending invocation and policy clock.
    /// # Errors
    /// Refuses retired, cancelled, replaced or no-longer-authorized custody.
    pub fn certificate(&self) -> Result<tilo_user_identity::DeviceCert, String> {
        (self.check)()
    }

    /// Submit this bound app's authorship through the normal published door.
    /// # Errors
    /// Refuses missing custody or unavailable/ambiguous membership publication.
    pub fn delegate_authorship(&self) -> Result<crate::collab_broker_capnp::InvocationJob, String> {
        (self.grant)(&self.certificate()?)
    }
}

/// Original host-retained facts for one authenticated live launch.
#[derive(Clone, Debug)]
pub struct AuthenticatedApplicationLaunch {
    pub creation: Option<ApplicationCreationEvidence>,
    pub continuation_instance: Option<u64>,
    /// The ticket [`LauncherServices::launch_content`] answered for the
    /// launch that started this process ([`Launched::Started`]), read from
    /// the host's own launch record. Correlation only, never authority.
    pub launch_ticket: Option<u64>,
}

/// What [`LauncherServices::launch_content`] started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Launched {
    /// A new instance, whose collaboration connection presents `ticket`
    /// when it is authenticated
    /// ([`AuthenticatedApplicationLaunch::launch_ticket`]): the launch can
    /// name the instance it started without guessing.
    Started { ticket: u64 },
    /// Something the host cannot tie to a connection: an app another device
    /// opens (a governed room), a host application, a launcher keeping no
    /// tickets.
    Unconfirmed,
}

/// Content-free progress of the existing executing-process custody handshake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutingBindingPhase {
    Started,
    Challenge,
    Answer,
    Admitted,
    Refused,
    /// The launch exited before the app answered. The binding waits for the
    /// answer, not for a clock, so this is how an unanswered binding ends.
    Abandoned,
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
    /// Launches `app` into `workspace`, its view of `record` (else the
    /// record it opens by default) opening at `place`.
    ///
    /// # Errors
    ///
    /// Returns a message when the host cannot launch the application.
    fn launch(
        &self,
        app: &str,
        workspace: &Workspace,
        record: Option<&str>,
        place: &LaunchPlace,
    ) -> Result<(), String>;
    /// Launch an exact installed package, or an ordinary host application,
    /// and observe its real startup in the authenticated workspace.
    ///
    /// # Errors
    /// Refuses when this host cannot provide startup observations. An empty
    /// package root selects the ordinary installed application identity.
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
    /// # Errors
    /// Refuses stale original custody or startup failures.
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
    ///
    /// # Errors
    ///
    /// Returns a message when the launch fails or content staging is unsupported.
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
    fn attest_process(&self, pid: u32, app: &str, room: &str) -> bool;
    /// Snapshot the original launch facts while admitting this connection.
    /// Unsupported launchers retain no private window-instance authority.
    ///
    /// # Errors
    /// The original launch facts changed or cannot be read.
    fn authenticated_launch(
        &self,
        pid: u32,
        app: &str,
        room: &str,
    ) -> Result<Option<AuthenticatedApplicationLaunch>, String> {
        if !self.attest_process(pid, app, room) {
            return Ok(None);
        }
        Ok(Some(AuthenticatedApplicationLaunch {
            creation: self.application_creation_evidence(pid, app, room)?,
            continuation_instance: None,
            launch_ticket: None,
        }))
    }

    /// Admit kernel-attested executing custody only beneath this exact launch.
    /// This grants no new identity or filesystem permissions.
    #[cfg(target_os = "linux")]
    fn attest_executing_process(
        &self,
        launcher_pid: u32,
        peer: &runtime_portal::process_binding::ExecutingPeer,
        app: &str,
        room: &str,
    ) -> bool {
        let _ = (launcher_pid, peer, app, room);
        false
    }
    /// Original declarations of this exact live launch, not the currently
    /// installed replacement package. Unsupported hosts grant no creation.
    ///
    /// # Errors
    ///
    /// The launch record, process start time or signed package custody changed.
    fn application_creation_evidence(
        &self,
        pid: u32,
        app: &str,
        room: &str,
    ) -> Result<Option<ApplicationCreationEvidence>, String> {
        let _ = (pid, app, room);
        Ok(None)
    }
    /// An action invocation reached `app` in `room`. A person, an agent or
    /// another device drives an app through its actions as much as through
    /// keys and clicks, so a window the app closes after one was closed on
    /// request, not lost.
    fn note_acted_on(&self, app: &str, room: &str) {
        let _ = (app, room);
    }
    /// Whether [`Self::close`] can close applications on this host.
    fn closes_applications(&self) -> bool {
        false
    }
    /// Ask every window of `app` in `workspace` to close the way a person
    /// closing it would. The next launch starts a fresh process, which picks
    /// up permissions granted since the last one.
    ///
    /// # Errors
    ///
    /// Returns a message when the host cannot close applications.
    fn close(&self, app: &str, workspace: &Workspace) -> Result<(), String> {
        let _ = (app, workspace);
        Err("closing applications is unsupported on this host".into())
    }
}

/// One host-owned workspace governance action, already admitted to the
/// remote-action vocabulary. The broker mints `invocation`; the service
/// validates the caller's authority and applies the action.
pub struct WorkspaceGovernanceRequest<'a> {
    pub room: &'a str,
    pub actor: &'a crate::collab_broker_capnp::BrokerActor,
    pub resource: &'a str,
    pub action: &'a str,
    pub argument: Option<&'a str>,
    /// Legacy JSON confirmation evidence (`{"confirm": true}` or
    /// `{"confirmation_phrase": ...}`).
    pub confirmation: &'a serde_json::Value,
    pub invocation: tilo_actions::InvocationId,
}

/// Workspace governance (permission profiles) owned by a host that holds the
/// workspace's capability authority. Its actions are offered through the
/// broker's remote-action catalogue under the workspace provider.
pub trait WorkspaceGovernanceServices: Send + Sync {
    /// The application id governance actions are checked against.
    fn app_id(&self) -> &str;
    /// Whether `action` is one of this service's governance actions.
    fn handles(&self, action: &str) -> bool;
    /// Every governance action of `workspace`, before the caller's
    /// permission check.
    fn descriptors(&self, workspace: &Workspace) -> Vec<tilo_actions::ActionDescriptor>;
    /// Validate and apply one governance action, recording its effect.
    ///
    /// # Errors
    ///
    /// Returns a refusal when the caller may not act, confirmation is
    /// missing, or the action cannot be applied.
    fn invoke(&self, request: &WorkspaceGovernanceRequest<'_>) -> Result<(), String>;
}
/// One checked change to a workspace's saved shortcuts. The broker mints
/// `invocation` and has checked the change against what `actor` is offered.
pub struct ShortcutLibraryRequest<'a> {
    pub room: &'a str,
    pub actor: &'a crate::collab_broker_capnp::BrokerActor,
    pub change: &'a crate::automation::ShortcutChange,
    /// The provenance the change leaves in workspace activity.
    pub context: tilo_actions::ActionContext,
    pub invocation: tilo_actions::InvocationId,
}

/// A host that stores a workspace's shortcuts and runs them. The broker
/// publishes the shortcut library and each saved shortcut's `run` only
/// where this service is installed.
pub trait ShortcutLibraryServices: Send + Sync {
    /// The chords already taken on this device, which a shortcut's keyboard
    /// trigger may not take.
    fn keymap(&self) -> settings_store::hardware::ShortcutConfig;
    /// Store one checked change in workspace `request.room` and record its
    /// effect in workspace activity.
    ///
    /// # Errors
    ///
    /// Returns why the workspace cannot take the change.
    fn store(&self, request: &ShortcutLibraryRequest<'_>) -> Result<(), String>;
    /// The runtime shortcut runs execute on.
    ///
    /// # Errors
    ///
    /// Returns why no runtime is available.
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

/// The device's own keyboard shortcuts, described and changed by the host
/// that owns their labels. The broker loads and saves the keymap, offers
/// the catalogue only to callers on this machine, and checks each action
/// against the caller's policy under [`Self::app_id`].
pub trait KeyboardShortcutServices: Send + Sync {
    /// The application id keyboard shortcut actions are checked against.
    fn app_id(&self) -> &str;
    /// Whether `resource` is the shortcut catalogue or one of its entries.
    fn is_resource(&self, resource: &str) -> bool;
    /// The catalogue rows under workspace provider `provider` for `keymap`.
    fn catalogue_rows(
        &self,
        provider: &str,
        keymap: &settings_store::hardware::ShortcutConfig,
    ) -> Vec<tilo_actions::remote::CatalogueRow>;
    /// The action `address` names in `keymap`, when there is one.
    fn descriptor(
        &self,
        keymap: &settings_store::hardware::ShortcutConfig,
        address: &tilo_actions::ActionAddress,
    ) -> Option<tilo_actions::ActionDescriptor>;
    /// Decode `argument` for `address` and apply it to `keymap`: its output,
    /// and whether `keymap` changed and must be saved.
    ///
    /// # Errors
    ///
    /// Why the arguments are malformed or the change cannot be made, such
    /// as the chord another shortcut holds.
    fn apply(
        &self,
        keymap: &mut settings_store::hardware::ShortcutConfig,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> Result<(tilo_actions::ActionOutput, bool), Box<tilo_actions::ActionFailure>>;
}

/// One pairing step the broker admitted: action `address` (on
/// [`tilo_actions::remote::device_pairing::RESOURCE`]) for `actor` in
/// `room`, with `argument` in its slot form.
pub struct DevicePairingRequest<'a> {
    pub room: &'a str,
    pub actor: &'a crate::collab_broker_capnp::BrokerActor,
    pub address: &'a tilo_actions::ActionAddress,
    pub argument: Option<&'a str>,
}

/// How a pairing step ended.
pub type DevicePairingOutcome =
    Result<tilo_actions::ActionOutput, Box<tilo_actions::ActionFailure>>;

/// Pairing a nearby device with the person's identity, run by the host:
/// LAN discovery, and the approval an administrator authenticates through
/// the host's polkit helper. The broker answers it under
/// [`tilo_actions::remote::device_pairing::PROVIDER`], only to a caller on
/// this machine confined to a room, and checks each step against the
/// caller's policy under [`Self::app_id`] before the host decides whether
/// the actor may pair at all.
pub trait DevicePairingServices: Send + Sync {
    /// The application whose controls pair devices. It reaches the steps as
    /// the person using it, as it does its own actions.
    fn app_id(&self) -> &str;
    /// Every step, described.
    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor>;
    /// Start one admitted step. Its outcome arrives on the returned answer:
    /// at once, or once the LAN window or the administrator's
    /// authentication ends, completed from another thread.
    ///
    /// # Errors
    ///
    /// Why the step may not start: the actor does not administer the
    /// room's identity, the arguments are malformed, or another step runs.
    fn start(
        &self,
        request: &DevicePairingRequest<'_>,
    ) -> Result<tilo_actions::remote::Pending<DevicePairingOutcome>, String>;
}

/// One identity-administration step the broker admitted: action `address`
/// (on [`tilo_actions::remote::identity_admin::RESOURCE`]) for `actor` in
/// `room`, with `argument` in its slot form.
pub struct IdentityAdminRequest<'a> {
    pub room: &'a str,
    pub actor: &'a crate::collab_broker_capnp::BrokerActor,
    pub address: &'a tilo_actions::ActionAddress,
    pub argument: Option<&'a str>,
}

/// How an identity-administration step ended.
pub type IdentityAdminOutcome = DevicePairingOutcome;

/// The person's identities and the devices trusted under them, administered
/// by the host that keeps the identity store: listing, activating, removing
/// and protecting an identity, listing and revoking its devices, and
/// removing a home server. The root secret stays on the host, which signs
/// every revocation. The broker answers it under
/// [`tilo_actions::remote::identity_admin::PROVIDER`], only to a caller on
/// this machine confined to a room, and checks each step against the
/// caller's policy under [`Self::app_id`] before the host decides whether
/// the actor may administer the identity at all.
pub trait IdentityAdminServices: Send + Sync {
    /// The application whose controls administer identities. It reaches
    /// the steps as the person using it, as it does its own actions.
    fn app_id(&self) -> &str;
    /// Every step, described.
    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor>;
    /// Start one admitted step. Its outcome arrives on the returned answer:
    /// at once, or once a step that reaches another device ends.
    ///
    /// # Errors
    ///
    /// Why the step may not start: the actor does not administer the
    /// room's identity, or the arguments are malformed.
    fn start(
        &self,
        request: &IdentityAdminRequest<'_>,
    ) -> Result<tilo_actions::remote::Pending<IdentityAdminOutcome>, String>;
}

pub trait ContinuationServices: Send + Sync {
    /// Reports one application instance's continuation metadata.
    ///
    /// # Errors
    ///
    /// Returns a message when the host cannot retain the metadata.
    fn report(
        &self,
        room: RoomId,
        app: String,
        instance: u64,
        schema: u16,
        metadata: Vec<u8>,
    ) -> Result<(), String>;
    /// Clears one application instance's continuation metadata.
    ///
    /// # Errors
    ///
    /// Returns a message when the host cannot clear the metadata.
    fn clear(&self, room: RoomId, app: &str, instance: u64) -> Result<(), String>;
    /// One application instance answering a handoff request, or reporting
    /// that the document it shows arrived retired. `message` is an encoded
    /// `ui::collaboration::handoff::HandoffReport`.
    ///
    /// # Errors
    ///
    /// Returns a message when the host does not take handoff reports.
    fn handoff(
        &self,
        room: RoomId,
        app: String,
        instance: u64,
        message: Vec<u8>,
    ) -> Result<(), String>;
}
/// Called on the broker `LocalSet`; returned Cap'n Proto clients never cross threads.
pub trait WorkspaceControlServices: Send + Sync {
    /// Opens the workspace control handle of `room` for `target`.
    ///
    /// # Errors
    ///
    /// Returns a Cap'n Proto error when the handle cannot be opened.
    fn open(
        &self,
        room: RoomId,
        target: [u8; 32],
        revalidate: Arc<dyn Fn() -> Result<(), capnp::Error> + Send + Sync>,
    ) -> Result<runtime_collab::raw::workspace_control_handle::Client, capnp::Error>;
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

/// Native realtime continuity remains host-owned because it binds portal
/// capture leases and certified device-fabric state. The shared broker owns
/// admission, exact route binding, wire bounds, and disconnect cleanup.
pub trait RealtimeServices: Send + Sync {
    /// Runs one realtime continuity command.
    ///
    /// # Errors
    ///
    /// Returns a message when the host refuses or cannot run the command.
    fn command(
        &self,
        context: &RealtimeRequestContext,
        command_json: &[u8],
    ) -> Result<Vec<u8>, String>;

    /// Subscribes to realtime continuity events for this context.
    ///
    /// # Errors
    ///
    /// Returns a message when the host refuses the subscription.
    fn subscribe(
        &self,
        context: &RealtimeRequestContext,
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>, String>;

    fn disconnect(&self, peer_id: u64);
}
/// The host's speaker groups: the broker admits only the certified Settings
/// route, bounds and validates the command, and hands it here.
pub trait SpeakerServices: Send + Sync {
    /// Runs one validated speaker-group command.
    fn command(&self, command: runtime_collab::SpeakerCommand) -> runtime_collab::SpeakerReply;
}
/// The host's Wi-Fi credential continuity across the person's devices: the
/// broker admits only the certified Settings route, bounds and validates the
/// command, and hands it here. A host without Wi-Fi radios installs none.
pub trait WifiContinuityServices: Send + Sync {
    /// Runs one validated Wi-Fi continuity command.
    fn command(
        &self,
        command: runtime_collab::WifiContinuityCommand,
    ) -> runtime_collab::WifiContinuityReply;
}

/// The broker-authenticated connection a Nearby Send command came from.
/// Every handle the host issues is bound to this peer and channel, never to
/// anything the caller claims.
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

/// The host's Nearby Send between the person's devices: the broker admits
/// only the certified Files route, bounds and validates the command and the
/// reply, and drops a connection's handles when it goes away. A host without
/// nearby radios installs none.
pub trait NearbyTransferServices: Send + Sync {
    /// Runs one validated Nearby Send command for `context`.
    ///
    /// # Errors
    ///
    /// Returns a message when the host refuses the command outright, such as
    /// when the person has turned file transfer off.
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
    read: crate::collab_broker_capnp::NativeDocumentReadContext,
}
impl BrowserServiceSessionRequestContext {
    pub(crate) fn authenticated(
        peer_id: u64,
        person: String,
        read: crate::collab_broker_capnp::NativeDocumentReadContext,
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
    pub fn read_context(&self) -> &crate::collab_broker_capnp::NativeDocumentReadContext {
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
        self.authorize(context)
    }
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

/// Sealed local Files admission. Its callback rechecks the original caller,
/// live connection and existing workspace policy, never a JSON-claimed actor.
#[derive(Clone)]
pub struct FileClipboardContext {
    pub(crate) peer: u64,
    pub(crate) window: String,
    pub(crate) duplicate: SelectedDescriptorSource,
    pub(crate) workspace: String,
    pub(crate) owner: String,
    pub(crate) history_compensation: bool,
    pub(crate) check: Arc<dyn Fn(tilo_actions::ActionKind) -> Result<(), String> + Send + Sync>,
}
impl FileClipboardContext {
    pub fn window(&self) -> &str {
        &self.window
    }
    /// Duplicate an object retained by this exact launcher-attested Files process.
    ///
    /// # Errors
    /// Refuses retired authority or unavailable peer descriptors.
    pub fn duplicate_selected(&self, descriptor: u32) -> Result<std::fs::File, String> {
        self.authorize(tilo_actions::ActionKind::Read)?;
        let file = (self.duplicate)(descriptor)?;
        self.authorize(tilo_actions::ActionKind::Read)?;
        Ok(file)
    }
    pub fn peer(&self) -> u64 {
        self.peer
    }
    pub fn workspace(&self) -> &str {
        &self.workspace
    }
    pub fn owner(&self) -> &str {
        &self.owner
    }
    /// Whether the broker admitted this command through a declared document Undo.
    /// This is minted from original invocation custody, never caller parameters.
    pub fn is_history_compensation(&self) -> bool {
        self.history_compensation
    }
    /// Recheck the exact caller against live custody and workspace policy.
    ///
    /// # Errors
    /// Refuses retired, revoked, expired or insufficient authority.
    pub fn authorize(&self, permission: tilo_actions::ActionKind) -> Result<(), String> {
        (self.check)(permission)
    }
}
#[cfg(target_os = "linux")]
#[derive(Clone)]
pub struct OpenSelectedContext {
    pub(crate) launch: ObservedApplicationLaunch,
    pub(crate) workspace: String,
    pub(crate) duplicate: SelectedDescriptorSource,
    pub(crate) executing: Arc<runtime_portal::process_binding::ExecutingPeer>,
    pub(crate) check: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
}
#[cfg(target_os = "linux")]
impl OpenSelectedContext {
    pub fn observed_launch(&self) -> &ObservedApplicationLaunch {
        &self.launch
    }
    pub fn workspace(&self) -> &str {
        &self.workspace
    }
    pub fn executing(&self) -> &Arc<runtime_portal::process_binding::ExecutingPeer> {
        &self.executing
    }
    /// # Errors
    /// Refuses cancelled, revoked or retired OpenWith custody.
    pub fn authorize(&self) -> Result<(), String> {
        (self.check)()?;
        if !self.executing.current() {
            return Err("Files process ended".into());
        }
        Ok(())
    }
    /// # Errors
    /// Refuses stale authority or unavailable actual peer descriptors.
    pub fn duplicate_selected(&self, descriptor: u32) -> Result<std::fs::File, String> {
        self.authorize()?;
        let file = (self.duplicate)(descriptor)?;
        self.authorize()?;
        Ok(file)
    }
}
#[cfg(target_os = "linux")]
pub type OpenSelectedFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = Result<runtime_collab::open_selected::ObservedOpenReply, String>,
            > + Send,
    >,
>;
#[cfg(target_os = "linux")]
pub type OpenSelectedV2Future = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = Result<runtime_collab::open_selected::v2::ObservedReply, String>,
            > + Send,
    >,
>;
/// Read-only discovery context sealed to the same admitted native Files task.
/// The context cannot admit a selection or stand in for an original OpenWith.
#[cfg(target_os = "linux")]
#[derive(Clone)]
pub struct SelectedDiscoveryContext {
    pub(crate) clipboard: FileClipboardContext,
    pub(crate) executing: Arc<runtime_portal::process_binding::ExecutingPeer>,
    pub(crate) certificate: tilo_user_identity::DeviceCert,
    pub(crate) original: Option<ObservedApplicationLaunch>,
    pub(crate) needed: tilo_actions::ActionKind,
}
#[cfg(target_os = "linux")]
impl SelectedDiscoveryContext {
    pub fn workspace(&self) -> &str {
        self.clipboard.workspace()
    }
    pub fn owner(&self) -> &str {
        self.clipboard.owner()
    }
    pub fn executing(&self) -> &Arc<runtime_portal::process_binding::ExecutingPeer> {
        &self.executing
    }
    pub fn certificate(&self) -> &tilo_user_identity::DeviceCert {
        &self.certificate
    }
    pub fn authorize(&self) -> Result<(), String> {
        self.clipboard.authorize(self.needed)?;
        if let Some(original) = &self.original
            && original.certificate()? != self.certificate
        {
            return Err("selected discovery original certificate changed".into());
        }
        if !self.executing.current() {
            return Err("Files discovery process ended".into());
        }
        Ok(())
    }
}
#[cfg(target_os = "linux")]
pub type SelectedDiscoveryFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = Result<runtime_collab::open_selected::discovery::Reply, String>,
            > + Send,
    >,
>;
pub trait FileClipboardServices: Send + Sync {
    #[cfg(target_os = "linux")]
    fn discover_selected(
        &self,
        _context: SelectedDiscoveryContext,
        _request: runtime_collab::open_selected::discovery::Request,
    ) -> SelectedDiscoveryFuture {
        Box::pin(async { Err("selected discovery is unavailable".into()) })
    }

    #[cfg(target_os = "linux")]
    fn open_selected_v2(
        &self,
        _context: OpenSelectedContext,
        _request: runtime_collab::open_selected::v2::Request,
    ) -> OpenSelectedV2Future {
        Box::pin(async { Err("selected version two access unsupported".into()) })
    }
    #[cfg(target_os = "linux")]
    fn open_selected(
        &self,
        _context: OpenSelectedContext,
        _request: runtime_collab::open_selected::Request,
    ) -> OpenSelectedFuture {
        Box::pin(async { Err("retained selected-open transport unavailable".into()) })
    }

    /// Perform a bounded command under the sealed local admission.
    ///
    /// # Errors
    /// Refuses invalid, unavailable or unauthorized clipboard operations.
    fn command(
        &self,
        context: &FileClipboardContext,
        command: runtime_collab::file_clipboard::Command,
    ) -> Result<runtime_collab::file_clipboard::Reply, String>;
    fn disconnect(&self, peer: u64);
}
/// Where grants a person gives an application for another app's action are
/// recorded. The broker decides whether to ask and builds the grant; the
/// host owns the workspace records it lands in.
pub trait AppActionGrants: Send + Sync {
    /// Whether this machine governs `room` and `person` may grant in it,
    /// so a grant they give can be recorded here.
    fn may_grant(&self, room: &str, person: &str) -> bool;
    /// Record `grant` in `request.room` as given by `request.person`, and
    /// note `answer` in the workspace's history. The recorded grant's id, or
    /// `None` when it could not be recorded.
    fn record(
        &self,
        request: &crate::app_actions::AppActionConsentRequest,
        grant: tilo_action_policy::Grant,
        answer: crate::app_actions::AppActionConsent,
    ) -> Option<String>;
    /// Remove the grant `grant` from `room`: a grant given once, now that
    /// its call was decided.
    fn revoke(&self, room: &str, grant: &str);
}
/// Identity of an exact private Settings worker, derived by the broker.
#[derive(Clone, Debug)]
pub struct DisplayControlContext {
    pub(crate) room: String,
    pub(crate) root: [u8; 32],
    pub(crate) device: [u8; 32],
}
impl DisplayControlContext {
    pub fn room(&self) -> &str {
        &self.room
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
    /// # Errors
    /// Refuses stale owner authority, invalid intent or failed persistence.
    fn command(&self, context: &DisplayControlContext, command: &[u8]) -> Result<Vec<u8>, String>;
}

#[derive(Clone, Default)]
pub struct HostServices {
    /// Fixed project-host registrations, unavailable unless actual enforcement is configured.
    pub approved_checks: Option<Arc<dyn crate::collab_broker_capnp::ApprovedCheckServices>>,
    /// Read-only existing-resource discovery, absent unless the native host
    /// can carry it over its current authenticated room connection.
    pub resource_views: Option<Arc<dyn NativeResourceViews>>,
    pub committed_documents: Option<Arc<dyn NativeDocumentReads>>,
    pub native_media: Option<Arc<dyn NativeMediaServices>>,
    pub audio_output: Option<Arc<dyn AudioOutputServices>>,
    pub workspace_memberships: Option<Arc<dyn WorkspaceMemberships>>,
    pub task_execution: Option<Arc<dyn WorkspaceTaskExecution>>,
    pub native_certificates: Option<Arc<dyn crate::peer_actions::CertificateAuthority>>,
    pub local_person: Option<Arc<dyn LocalPersonServices>>,
    pub legacy_workspace: Option<Arc<dyn LegacyWorkspaceProjection>>,
    pub launcher: Option<Arc<dyn LauncherServices>>,
    pub workspace_governance: Option<Arc<dyn WorkspaceGovernanceServices>>,
    pub shortcut_library: Option<Arc<dyn ShortcutLibraryServices>>,
    /// The device's keyboard shortcuts on the action door. Without it
    /// they are not offered.
    pub keyboard_shortcuts: Option<Arc<dyn KeyboardShortcutServices>>,
    /// Pairing a nearby device, run by the host. Without it no door offers
    /// it.
    pub device_pairing: Option<Arc<dyn DevicePairingServices>>,
    /// The person's identities and their devices, administered by the
    /// host. Without it no door offers them.
    pub identity_admin: Option<Arc<dyn IdentityAdminServices>>,
    pub continuations: Option<Arc<dyn ContinuationServices>>,
    pub workspace_control: Option<Arc<dyn WorkspaceControlServices>>,
    pub realtime: Option<Arc<dyn RealtimeServices>>,
    pub speakers: Option<Arc<dyn SpeakerServices>>,
    /// The session's settings, of which each app reaches its own entries
    /// and the shared sections (`settings_store::app_scope`). Without it
    /// apps have no settings beyond their own process.
    pub settings: Option<Arc<dyn settings_store::SettingsSource>>,
    pub display_control: Option<Arc<dyn DisplayControlServices>>,
    /// Every app's live activity, as the host presents it. Without it no
    /// app shows anything outside its windows.
    pub live_activities: Option<Arc<runtime_collab::LiveActivities>>,
    pub wifi_continuity: Option<Arc<dyn WifiContinuityServices>>,
    pub browser_service_sessions: Option<Arc<dyn BrowserServiceSessionServices>>,
    pub nearby_transfer: Option<Arc<dyn NearbyTransferServices>>,
    pub file_clipboard: Option<Arc<dyn FileClipboardServices>>,
    /// What each application declared it may do on the action door under
    /// its own identity. Without it no application declared anything.
    pub app_action_access: Option<crate::app_actions::AppActionAccessResolver>,
    /// Records grants people give applications for other apps' actions.
    /// Without it an app is never offered a grant question.
    pub app_action_grants: Option<Arc<dyn AppActionGrants>>,
    /// Where the launchable action catalogue survives a restart. Without it
    /// closed apps' actions are retained for this broker's lifetime only.
    pub launchable_catalogue: Option<std::path::PathBuf>,
    /// Content records reference, kept on this device and asked of each
    /// record's route (`offerBlob`, `wantBlob`, `subscribeBlobs`). Without
    /// it apps' content stays in their own process.
    pub record_content: Option<Arc<crate::record_content::RouteContent>>,
    /// This device's channel encryption service, which tells each route's
    /// endpoint when its channel keys changed (`channelKeysChanged`).
    /// Without it endpoints ask the identity service on a timer.
    pub channel_keys: Option<crate::channel_encryption::ChannelKeyProvider>,
    /// Where an admitted invocation of an offloadable action runs: on the
    /// person's best-placed device when another is
    /// (`tilo_actions::offload`). Without it every one runs here, through
    /// the app's own handler.
    pub action_offload: Option<Arc<dyn tilo_actions::offload::ActionOffload>>,
    /// Carries an application's action to another of the person's own
    /// devices. Without it no app reaches another device's actions.
    pub own_device: Option<Arc<dyn OwnDeviceActions>>,
}
