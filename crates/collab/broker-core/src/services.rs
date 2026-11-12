//! What a host serves the broker, on every host: the services a role installs, the data that
//! travels with them, and what the broker answers an app's call from.
//!
//! Which process answers a service is that host's own — this desktop's broker over its app
//! sockets, the page over the apps it runs in process. Every trait here names only what a role
//! installs and reads, never how a host carries it out.

use std::sync::Arc;

use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceId};

mod own_device;
mod task_execution;
mod workspace_nodes;

pub use own_device::{OwnDeviceActions, OwnDeviceOutcome};
pub use task_execution::{
    AgentRoster, DOOR_RUN_PREFIX, NativeTaskActionRequirement, NativeTaskBinding, Preview,
    PreviewSide, TRACKER_APP, TRACKER_ISSUE_PREFIX, TRACKER_RECORD, TRACKER_RESOURCE,
    TrackerAnswer, TrackerDirection, TrackerQuestion, TrackerRun, TrackerRuns,
    confirmation_preview, is_conversation_turn, is_door_run, is_tracker_issue, issue_answer_scope,
};
pub use workspace_nodes::{WorkspaceNodeActions, WorkspaceNodeAsk, WorkspaceNodeOutcome};

/// Current native seat identity, installed only by the authenticated host.
/// This Rust capability is never exposed through a wire request.
pub trait LocalPersonServices: Send + Sync {
    /// Derive the real local certificate for this person and current workspace.
    fn certificate(
        &self,
        workspace: &str,
        person: &str,
    ) -> Result<tilo_user_identity::DeviceCert, String>;

    /// [`Self::certificate`] for an action the person at this seat starts, which the host answers
    /// once its identity names them: a login reads the identity directory as its files land, and an
    /// action asked in that moment waits here for it, woken by the identity store's own
    /// announcement, instead of being refused for an identity that is on its way. A host whose
    /// identity is not on its way answers as [`Self::certificate`] does.
    fn certificate_when_named(
        &self,
        workspace: &str,
        person: &str,
    ) -> Result<tilo_user_identity::DeviceCert, String> {
        self.certificate(workspace, person)
    }
}

/// The origin of a workspace's records on this device: it applies an agent task's change the broker
/// admitted, running the action against the record as it is then.
pub trait RecordChangeServices: Send + Sync {
    /// Apply `change` in `workspace` as `invoker`'s step: the origin's answer.
    fn apply(
        &self,
        workspace: &str,
        change: &tilo_actions::remote::RecordChange,
        invoker: &tilo_actions::ActionInvoker,
    ) -> Result<serde_json::Value, String>;
}

/// The normal desktop opening deadline, shared with placement retirement.
pub const APPLICATION_STARTUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Reads the host's committed authority in place; mutations go back to that host, and this owns
/// no replica or persistence.
///
/// Policy questions borrow the committed tree rather than copying it: in the compositor a copy is
/// taken under the tree lock the render loop writes through, so one admission asks once.
pub trait WorkspaceAuthority: Send + Sync {
    /// Run `read` once against the committed workspace tree, borrowed. `read` must not block,
    /// await or serialize: a host may hold its tree lock while it runs.
    ///
    /// # Errors
    ///
    /// A message when the committed authority cannot be read; `read` is then not called.
    fn with_tree(&self, read: &mut dyn FnMut(&DesktopTree)) -> Result<(), String>;

    /// As [`Self::with_tree`], for exactly `workspace` (`None` when this host has none). A host
    /// whose workspaces are not one tree overrides it to look up only that one.
    fn with_workspace(
        &self,
        workspace: &str,
        read: &mut dyn FnMut(Option<&Workspace>),
    ) -> Result<(), String> {
        self.with_tree(&mut |tree| read(workspace_in(tree, workspace)))
    }

    /// An owned copy of the committed workspace tree, for the few callers
    /// that must keep one past the read. Never on an admission path.
    fn snapshot(&self) -> Result<DesktopTree, String> {
        let mut copy = None;
        self.with_tree(&mut |tree| copy = Some(tree.clone()))?;
        copy.ok_or_else(|| UNANSWERED.to_owned())
    }

    fn legacy_journal_root(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// Whether every write the broker's membership and context checks read here (this tree, the
    /// device fleet, the identity registry, the owner's task store) is announced, so the broker
    /// re-checks on the announcement instead of every second. The default keeps that re-check: a
    /// host that announces nothing must not have a revocation go unenforced.
    fn announces_policy_changes(&self) -> bool {
        false
    }

    /// The delegated agents on `workspace`'s roster, as this host verified their person's root
    /// signed the delegation for this device. Presentation only; the default knows none.
    fn history_agents(&self, _workspace: &str) -> Vec<collab_types::history::RosterAgent> {
        Vec::new()
    }

    /// What each app installed on this device is called, in every language its package names it
    /// in: what a window names another app's changes by, sent with the workspace's people's
    /// names. Presentation only; the default names none.
    fn installed_apps(&self) -> Vec<collab_types::history::NamedApp> {
        Vec::new()
    }

    /// The device revocations `workspace`'s document records, against which who saved a shortcut
    /// is proven. `None` when this host holds no document of the workspace.
    fn workspace_revocations(
        &self,
        _workspace: &str,
    ) -> Option<tilo_user_identity::RevocationList> {
        None
    }
}

const UNANSWERED: &str = "workspace authority did not answer";

#[must_use]
pub fn workspace_in<'a>(tree: &'a DesktopTree, workspace_id: &str) -> Option<&'a Workspace> {
    tree.workspaces
        .iter()
        .find(|workspace| workspace.id().to_string() == workspace_id)
}

impl dyn WorkspaceAuthority {
    /// One policy sample of the whole tree: `decide` runs once on the
    /// borrowed committed tree and its answer is returned.
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

    /// One policy sample of one workspace: `decide` runs once on the borrowed
    /// committed workspace of exactly `workspace` and its answer is returned.
    pub fn decide_in_workspace<R>(
        &self,
        workspace: &str,
        decide: impl FnOnce(Option<&Workspace>) -> R,
    ) -> Result<R, String> {
        let mut decide = Some(decide);
        let mut answer = None;
        self.with_workspace(workspace, &mut |workspace| {
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
    fn record_effect(
        &self,
        workspace: &str,
        invocation: tilo_actions::InvocationId,
        actor: &str,
        context: tilo_actions::ActionContext,
        status: tilo_actions::EffectReceiptStatus,
    ) -> Result<(), String>;
}
pub type WorkspaceAuthorityHandle = Arc<dyn WorkspaceAuthority>;

/// Where the view of a launched app opens: next to where the actor that
/// needs it already is, so the person and every actor keep their place. It
/// is where a window the launch starts stands, never a request for another
/// one: a launch of an app the host already shows brings that window
/// forward, and the place has nothing to say about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchPlace {
    /// Where the person on this device is looking: their own invocation,
    /// from the palette, a chord or a click.
    Focus,
    /// Beside the view of `app_id` the actor works in, wherever it is.
    Beside { app_id: String },
    /// Beside where the person is looking, without taking their focus: a view with none of its
    /// own to open beside.
    Aside,
}

#[derive(Clone)]
pub struct LaunchableApplication {
    pub package_id: String,
    pub display_name: String,
    /// A host application Tilo did not package: it runs outside every Tilo
    /// sandbox, with the user's own files, and publishes no typed actions.
    pub unconfined: bool,
    /// What its signed manifest offers while it is not running (`[[offers]]`); invoking one
    /// launches it and its published action admits the invocation.
    pub offers: Vec<tilo_app_package::manifest::OfferedAction>,
    /// The host capabilities its signed manifest declares (`[capabilities]`) that the host
    /// decides grants on (`settings_store::app_scope::SYSTEM_SETTINGS_CAPABILITY`).
    pub capabilities: Vec<String>,
}
/// The application a launcher-authenticated connection belongs to, and its workspace: launch
/// content is read only through this scope, since holding a hash is not access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchScope {
    pub app_id: String,
    pub workspace_id: String,
}

/// What a launch opens: the lowercase BLAKE3 `hash` of content-addressed bytes (empty for a
/// plain launch) and the application that asked, whose scope the bytes are read through.
#[derive(Clone, Copy, Debug)]
pub struct LaunchContent<'a> {
    pub hash: &'a str,
    pub caller: Option<&'a LaunchScope>,
    /// The person who opened the app, when they may change the workspace: the host records them
    /// granting it authorship. `None` launches a viewer.
    pub author: Option<&'a str>,
}

/// One admitted launch's actual startup observation. Dropping an unanswered one retires its
/// host-side subscription; enqueueing a worker is never sufficient for this receipt.
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
    pub workspace: String,
}

/// Distinguishes a reported startup failure from a lost startup observation.
/// Losing observation does not establish that the launched process stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchReadinessError {
    Failed(String),
    ObserverStopped,
    AdoptionObserverStopped,
    TimedOut,
}

impl LaunchReadinessError {
    #[must_use]
    pub fn is_uncertain(&self) -> bool {
        !matches!(self, Self::Failed(_))
    }
}

impl std::fmt::Display for LaunchReadinessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed(message) => f.write_str(message),
            Self::ObserverStopped => f.write_str("the application startup observer stopped"),
            Self::AdoptionObserverStopped => f.write_str("the adopted window observer stopped"),
            Self::TimedOut => f.write_str("the application did not open a window in time"),
        }
    }
}

impl std::error::Error for LaunchReadinessError {}

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
    pub async fn wait(self) -> Result<(), String> {
        self.wait_with_adoption()
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Observe the adopted window association when the native host supplies custody.
    /// Legacy hosts still settle readiness without inventing an association.
    pub async fn wait_with_adoption(
        mut self,
    ) -> Result<Option<LaunchAdoption>, LaunchReadinessError> {
        (&mut self.receiver)
            .await
            .map_err(|_| LaunchReadinessError::ObserverStopped)?
            .map_err(LaunchReadinessError::Failed)?;
        match self.adoption.take() {
            Some(receiver) => receiver
                .await
                .map_err(|_| LaunchReadinessError::AdoptionObserverStopped),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod launch_readiness_tests {
    use super::{LaunchReadiness, LaunchReadinessError};

    #[tokio::test]
    async fn lost_observers_are_uncertain_but_reported_failures_are_not() {
        for scenario in ["startup_lost", "adoption_lost", "reported_failure"] {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let (adopt, adopted) = tokio::sync::oneshot::channel();
            let ready = LaunchReadiness::new(receiver, || {}).with_adoption(adopted);
            match scenario {
                "startup_lost" => drop(sender),
                "adoption_lost" => sender.send(Ok(())).unwrap(),
                _ => sender
                    .send(Err("the application startup observer stopped".into()))
                    .unwrap(),
            }
            drop(adopt);
            let error = ready.wait_with_adoption().await.unwrap_err();
            let expected = match scenario {
                "startup_lost" => LaunchReadinessError::ObserverStopped,
                "adoption_lost" => LaunchReadinessError::AdoptionObserverStopped,
                _ => {
                    LaunchReadinessError::Failed("the application startup observer stopped".into())
                }
            };
            assert_eq!(error, expected);
            assert_eq!(error.is_uncertain(), scenario != "reported_failure");
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

/// Original host-retained facts for one authenticated live launch.
#[derive(Clone, Copy, Debug)]
pub struct AuthenticatedApplicationLaunch {
    pub continuation_instance: Option<u64>,
    /// The ticket that launch answered ([`Launched::Started`]), from the host's own launch
    /// record. Correlation only, never authority.
    pub launch_ticket: Option<u64>,
    /// The host made this connection for an instance with no window: it takes the app's actions
    /// on its records, never an `open`, which shows something.
    pub windowless: bool,
}

/// What [`LauncherServices::launch_content`] started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Launched {
    /// A new instance whose connection presents `ticket` when authenticated, so the launch names
    /// the instance it started without guessing.
    Started { ticket: u64 },
    /// Something no connection names: an app another device opens, a host application, a launcher
    /// keeping no tickets.
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

/// How the start of a windowless instance ends: `Ok` once it runs, why not otherwise; a sender
/// dropped unsent is a host that stopped starting it.
pub type WindowlessStart = tokio::sync::oneshot::Receiver<Result<(), String>>;

/// One host-owned workspace governance action, already admitted to the remote-action vocabulary:
/// the broker mints `invocation`, the service checks the caller's authority and applies it.
pub struct WorkspaceGovernanceRequest<'a> {
    /// Direct local origin, excluding a Shortcut or an on-behalf-of run.
    /// Installed by the broker, never decoded from action arguments.
    pub local_device: bool,
    /// The original run's live custody, when the caller belongs to a run.
    pub execution_lease: Option<tilo_actions::remote::ExecutionLease>,
    pub workspace: &'a str,
    pub actor: &'a crate::actor::BrokerActor,
    pub resource: &'a str,
    pub action: &'a str,
    pub argument: Option<&'a str>,
    /// Legacy JSON confirmation evidence (`{"confirm": true}` or
    /// `{"confirmation_phrase": ...}`).
    pub confirmation: &'a serde_json::Value,
    pub invocation: tilo_actions::InvocationId,
}

/// The terminal answer of an asynchronous workspace owner action.
pub type WorkspaceGovernanceOutcome =
    Result<tilo_actions::ActionOutput, Box<tilo_actions::ActionFailure>>;

/// The workspace governance (permission profiles) of a host that holds the workspace's
/// capability authority, offered under the workspace provider.
pub trait WorkspaceGovernanceServices: Send + Sync {
    /// The application id governance actions are checked against.
    fn app_id(&self) -> &str;
    /// Whether `action` is one of this service's governance actions.
    fn handles(&self, action: &str) -> bool;
    /// Every governance action of `workspace`, before the caller's
    /// permission check.
    fn descriptors(&self, workspace: &Workspace) -> Vec<tilo_actions::ActionDescriptor>;
    /// Whether this owner action takes canonical descriptor admission and broker-checked
    /// confirmation; a service keeps its own semantics unless it opts in.
    fn requires_descriptor_admission(&self, _action: &str) -> bool {
        false
    }
    /// Validate and apply one governance action, recording its effect.
    fn invoke(&self, _request: &WorkspaceGovernanceRequest<'_>) -> Result<(), String> {
        Err("this owner action requires asynchronous execution".into())
    }

    /// Start a potentially asynchronous owner action without blocking the door.
    /// The default preserves synchronous governance implementations.
    fn start(
        &self,
        request: &WorkspaceGovernanceRequest<'_>,
    ) -> Result<tilo_actions::remote::Pending<WorkspaceGovernanceOutcome>, String> {
        self.invoke(request)?;
        Ok(tilo_actions::remote::Pending::ready(Ok(
            tilo_actions::ActionOutput::default(),
        )))
    }
}
/// The device's own keyboard shortcuts, as the host that owns their labels describes them. The
/// broker loads the keymap, offers it only on this machine, and checks each action under
/// [`Self::app_id`].
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
    /// Decode `argument` for `address` and answer it from `keymap`.
    fn apply(
        &self,
        keymap: &settings_store::hardware::ShortcutConfig,
        address: &tilo_actions::ActionAddress,
        argument: Option<&str>,
    ) -> Result<tilo_actions::ActionOutput, Box<tilo_actions::ActionFailure>>;
}

/// Encoded bytes one application's search index change may take: the bound
/// `ui::search::index_publication::MAX_DELTA_BYTES` publishers chunk to.
pub const MAX_SEARCH_INDEX_DELTA_BYTES: usize = 512 * 1024;

/// The session's persistent search index of applications' content, kept after an app closes so
/// what it holds stays findable whether or not it runs.
pub trait SearchIndexServices: Send + Sync {
    /// Apply one change to the entries `app` published in `workspace` from `record`. `delta` is an
    /// encoded `ui::search::index_publication::IndexDelta` of at most
    /// [`MAX_SEARCH_INDEX_DELTA_BYTES`], which the host decodes and bounds.
    fn publish(
        &self,
        workspace: WorkspaceId,
        app: &str,
        record: &str,
        delta: &[u8],
    ) -> Result<(), String>;
}

/// What the session hears of a workspace's record changing: an app of the
/// workspace wrote it here, or a peer's change of it was taken in. The content
/// the record holds may have moved, and the search index that reads it without
/// the app's window is out of date.
pub trait RecordIndexChanges: Send + Sync {
    /// The record `record` of `app` in `workspace` changed.
    fn changed(&self, workspace: &str, app: &str, record: &str);
}

/// The scope component content puts are kept under in a workspace's content
/// store: neutral, no application's. The store reads by workspace, so every app
/// of the workspace reads what was put there once an action hands it the
/// reference.
pub const CONTENT_PUT_SCOPE: &str = "tilo.content";

/// The device content store, as the content put door writes to it
/// (`tilo_actions::remote::ContentPuts`). Actions carry intent and
/// references; bytes travel by session transport into the content store.
pub trait ContentStoreServices: Send + Sync {
    /// The most bytes one piece of content may have.
    fn max_bytes(&self) -> u64;
    /// Open an upload kept for `workspace` under [`CONTENT_PUT_SCOPE`].
    fn begin(&self, workspace: &str) -> Result<Box<dyn ContentSink>, String>;

    /// Whether `workspace` still holds `hash`: content it put there that no one
    /// has dropped since. A compaction of the content put attributions keeps
    /// the ones this answers `true` for and drops the rest, so the log stays
    /// as long as the content it describes is worth attributing. A store that
    /// cannot tell keeps the attribution.
    fn holds(&self, workspace: &str, hash: &str) -> bool {
        let _ = (workspace, hash);
        true
    }
}

/// One upload into the content store. Dropped unfinished, it stores
/// nothing.
pub trait ContentSink: Send {
    /// Hand the store the next bytes.
    fn append(&mut self, bytes: &[u8]) -> Result<(), String>;
    /// Store what was appended, recording the workspace's reference to it, and
    /// answer its hash (lowercase BLAKE3 hex) and length.
    fn finish(self: Box<Self>) -> Result<(String, u64), String>;
}
