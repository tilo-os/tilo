//! Explicit host services. These are process-owned capabilities, never wire flags.
use std::sync::Arc;
use tilo_desktop_model::{DesktopTree, RoomId, Workspace, WorkspaceTaskStatus};

pub use crate::collab_broker_capnp::{
    DocumentBytes, NativeDocumentRead, NativeDocumentReadContext, NativeDocumentReads,
    NativeMediaFuture, NativeMediaRequestContext, NativeMediaServices,
};

/// Reads a fresh immutable projection from the host's committed authority.
/// Mutations go back to that host; this interface owns no replica or persistence.
pub trait WorkspaceAuthority: Send + Sync {
    /// A fresh immutable projection of the committed workspace tree.
    ///
    /// # Errors
    ///
    /// Returns a message when the committed authority cannot be read.
    fn snapshot(&self) -> Result<DesktopTree, String>;
    fn legacy_journal_root(&self) -> Option<std::path::PathBuf> {
        None
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

pub type DesktopCommandFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = Result<
                    tilo_peer_state::owner_commit::OwnerCommitReceipt,
                    tilo_peer_state::desktop_command::DesktopCommandError,
                >,
            > + Send,
    >,
>;
/// An explicitly installed native semantic owner, never a compositor session.
/// The broker constructs requests from authenticated durable invocations.
pub trait DesktopCommands: Send + Sync {
    fn room(&self) -> &str;
    fn execute(
        &self,
        request: tilo_peer_state::owner_commit::OwnerCommitRequest,
        direct_delivery: bool,
    ) -> DesktopCommandFuture;
    /// Applications this owner can open. Each is published as an ordinary
    /// `desktop:app:<id>#open` action, the same for people (command palette)
    /// and agents.
    fn catalogue(&self) -> &dyn tilo_desktop_model::apps::AppCatalogue {
        &()
    }
    /// The owner's kept receipt of a committed desktop command (its command
    /// id and the windows it opened), when the owner keeps one. Read after
    /// a successful execute; the default keeps none.
    fn receipt(
        &self,
        request: &tilo_peer_state::owner_commit::OwnerCommitRequest,
    ) -> Option<tilo_peer_state::desktop_command::DesktopCommandReceipt> {
        let _ = request;
        None
    }
}

/// The room's owner for documents proposed into the room
/// ([`tilo_peer_state::document_import`]). The broker constructs each
/// request from an authenticated durable invocation; the owner decides it
/// in its serialized turn.
pub trait DocumentImports: Send + Sync {
    fn room(&self) -> &str;
    fn execute(
        &self,
        request: tilo_peer_state::owner_commit::OwnerCommitRequest,
    ) -> DesktopCommandFuture;
}

#[derive(Clone)]
pub struct LaunchableApplication {
    pub package_id: String,
    pub display_name: String,
    /// A host application Tilo did not package: it runs outside every Tilo
    /// sandbox, with the user's own files, and publishes no typed actions.
    pub unconfined: bool,
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

pub trait LauncherServices: Send + Sync {
    fn applications(&self) -> Vec<LaunchableApplication>;
    /// Launches `app` into `workspace`.
    ///
    /// # Errors
    ///
    /// Returns a message when the host cannot launch the application.
    fn launch(&self, app: &str, workspace: &Workspace) -> Result<(), String>;
    /// Launch `app` into `workspace`, opening the content-addressed bytes
    /// `content` names read-only, read through the calling application's own
    /// scope. An empty hash is a plain launch. A host that cannot stage
    /// content refuses one.
    ///
    /// # Errors
    ///
    /// Returns a message when the launch fails or content staging is unsupported.
    fn launch_content(
        &self,
        app: &str,
        workspace: &Workspace,
        content: LaunchContent<'_>,
    ) -> Result<(), String> {
        if content.hash.is_empty() {
            self.launch(app, workspace)
        } else {
            Err("opening launch content is unsupported on this host".into())
        }
    }
    fn attest_process(&self, pid: u32, app: &str, room: &str) -> bool;
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
}

impl NearbyTransferRequestContext {
    pub(crate) fn authenticated(
        peer_id: u64,
        app_id: impl Into<String>,
        workspace_id: impl Into<String>,
        channel: impl Into<String>,
    ) -> Self {
        Self {
            peer_id,
            app_id: app_id.into(),
            workspace_id: workspace_id.into(),
            channel: channel.into(),
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
}
/// Where grants a person gives an application for another app's action are
/// recorded. The broker decides whether to ask and builds the grant; the
/// host owns the workspace records it lands in.
pub trait AppActionGrants: Send + Sync {
    /// Whether this machine governs `room` and `person` may administer it,
    /// so a grant they give can be recorded here.
    fn may_grant(&self, room: &str, person: &str) -> bool;
    /// Record `capability` in `request.room` as granted by
    /// `request.person`, and note `answer` in the workspace's history.
    /// The recorded capability's id, or `None` when it could not be
    /// recorded.
    fn record(
        &self,
        request: &crate::app_actions::AppActionConsentRequest,
        capability: collab_types::caps::CapOverride,
        answer: crate::app_actions::AppActionConsent,
    ) -> Option<String>;
    /// Remove the grant `capability` from `room`: a grant given once, now
    /// that its call was decided.
    fn revoke(&self, room: &str, capability: &str);
}
#[derive(Clone, Default)]
pub struct HostServices {
    pub committed_documents: Option<Arc<dyn NativeDocumentReads>>,
    pub native_media: Option<Arc<dyn NativeMediaServices>>,
    pub desktop_commands: Option<Arc<dyn DesktopCommands>>,
    /// Documents proposed into the room and the owner's answers. Without it
    /// nothing can be proposed.
    pub document_imports: Option<Arc<dyn DocumentImports>>,
    pub native_certificates: Option<Arc<dyn crate::peer_actions::CertificateAuthority>>,
    pub legacy_workspace: Option<Arc<dyn LegacyWorkspaceProjection>>,
    pub launcher: Option<Arc<dyn LauncherServices>>,
    pub workspace_governance: Option<Arc<dyn WorkspaceGovernanceServices>>,
    pub shortcut_library: Option<Arc<dyn ShortcutLibraryServices>>,
    /// The device's keyboard shortcuts on the action door. Without it
    /// they are not offered.
    pub keyboard_shortcuts: Option<Arc<dyn KeyboardShortcutServices>>,
    pub continuations: Option<Arc<dyn ContinuationServices>>,
    pub workspace_control: Option<Arc<dyn WorkspaceControlServices>>,
    pub realtime: Option<Arc<dyn RealtimeServices>>,
    pub speakers: Option<Arc<dyn SpeakerServices>>,
    /// The session's settings, of which each app reaches its own entries
    /// and the shared sections (`settings_store::app_scope`). Without it
    /// apps have no settings beyond their own process.
    pub settings: Option<Arc<dyn settings_store::SettingsSource>>,
    pub wifi_continuity: Option<Arc<dyn WifiContinuityServices>>,
    pub nearby_transfer: Option<Arc<dyn NearbyTransferServices>>,
    /// What each application declared it may do on the action door under
    /// its own identity. Without it no application declared anything.
    pub app_action_access: Option<crate::app_actions::AppActionAccessResolver>,
    /// Records grants people give applications for other apps' actions.
    /// Without it an app is never offered a grant question.
    pub app_action_grants: Option<Arc<dyn AppActionGrants>>,
    pub collaboration_admin: Option<Arc<dyn crate::collaboration_admin::CollaborationAdmin>>,
    /// Where the launchable action catalogue survives a restart. Without it
    /// closed apps' actions are retained for this broker's lifetime only.
    pub launchable_catalogue: Option<std::path::PathBuf>,
}
