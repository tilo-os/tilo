//! `xdg-desktop-portal` backend over zbus.
//!
//! `xdg-desktop-portal` is the freedesktop.org component that sandboxed
//! apps talk to when they need privileged operations the sandbox blocks —
//! file open/save, screencast, camera access. The host portal proxies
//! every request to a *backend* implementation that owns the
//! decision-and-UI side; this is that backend for the Tilo compositor.
//!
//! We expose these D-Bus interfaces under
//! `org.freedesktop.impl.portal.desktop.tilo`:
//!
//! - `org.freedesktop.impl.portal.FileChooser` — `OpenFile` / `SaveFile` / `SaveFiles`.
//! - `org.freedesktop.impl.portal.ScreenCast` — `CreateSession` /
//!   `SelectSources` / `Start` + properties. The frontend alone owns
//!   `OpenPipeWireRemote` so it can provide a node-restricted connection.
//! - `org.freedesktop.impl.portal.RemoteDesktop` — `CreateSession` /
//!   `SelectDevices` / `Start` / `Notify*` seat injection.
//! - `org.freedesktop.impl.portal.Access` — `AccessDialog`, also used by
//!   `Camera.AccessCamera` (`app_id="camera"`), since `xdg-desktop-portal`
//!   has no separate `impl.portal.Camera` interface.
//! - `org.freedesktop.impl.portal.Notification` — sandboxed notification
//!   delivery into the compositor-owned notification chrome.
//!
//! Consent-bearing methods share one shape — see [`flow::resolve`]: look up
//! the caller's workspace + app id, fast-path `Allow` on an `Always`-scoped
//! grant, otherwise enqueue a `PendingGrant` and await the user's prompt
//! choice. Prompt choices stay in private local portal state rather than the
//! collaborative workspace document. Notification delivery is intentionally prompt-free and feeds the
//! compositor's revocable notification queue directly.
//!
//! Tests run a `PromptQueueHandle` in [`PromptResolver::AutoAllow`] mode
//! so round trips never block on a real dialog; `tests/portal_test.rs`
//! exercises each interface plus the "Always-grant skips the prompt"
//! path over an in-process p2p bus.

#![allow(
    clippy::style,
    clippy::complexity,
    clippy::perf,
    clippy::pedantic,
    clippy::nursery,
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    clippy::string_slice,
    clippy::unnecessary_get_then_check,
    reason = "portal implementation methods mirror fixed freedesktop D-Bus signatures and decode bounded wire/state formats; protocol and adversarial behavior is covered by focused tests"
)]
#![deny(clippy::too_many_arguments)]

#[cfg(any(test, target_os = "linux"))]
pub mod account;
#[cfg(any(test, target_os = "linux"))]
pub mod app_chooser;
pub mod appearance;
#[cfg(target_os = "linux")]
pub mod auth;
#[cfg(any(test, target_os = "linux"))]
pub mod background;
#[cfg(any(test, target_os = "linux"))]
pub mod camera;
pub mod capture_broker;
pub mod clipboard;
pub mod desktop_services;
pub mod doc_store;
#[cfg(any(test, target_os = "linux"))]
pub mod dynamic_launcher;
#[cfg(any(test, target_os = "linux"))]
pub mod email;
pub mod file_chooser;
pub mod file_picker_spawner;
pub mod global_shortcuts;
#[cfg(any(test, target_os = "linux"))]
pub mod inhibit;
#[cfg(any(test, target_os = "linux"))]
pub mod input_capture;
#[cfg(target_os = "linux")]
pub(crate) mod interface_dispatch;
pub mod lifecycle;
// Native media observes this policy on every supported shell host.
pub mod lockdown;
#[cfg(any(test, target_os = "linux"))]
pub mod notification;
#[cfg(any(test, target_os = "linux"))]
pub mod permission_store;
pub mod picker_queue;
pub mod print;
#[cfg(any(test, target_os = "linux"))]
pub mod remote_desktop;
#[cfg(any(test, target_os = "linux"))]
pub mod screencast;
pub mod screencast_pipewire;
#[cfg(target_os = "linux")]
pub mod screensaver;
#[cfg(any(test, target_os = "linux"))]
pub mod screenshot;
#[cfg(any(test, target_os = "linux"))]
pub mod secret;
pub mod settings;
#[cfg(any(test, target_os = "linux"))]
pub mod usb;
#[cfg(any(test, target_os = "linux"))]
pub mod wallpaper;

// Re-exported so integration tests can construct the backend types
// directly and register them on a custom `ConnectionBuilder` (see `tests/portal_test.rs`).
#[cfg(target_os = "linux")]
pub use account::AccountBackend;
#[cfg(target_os = "linux")]
pub use app_chooser::AppChooserBackend;
#[cfg(target_os = "linux")]
pub use auth::{AuthenticatedCall, PortalAuthenticator};
#[cfg(target_os = "linux")]
pub use background::BackgroundBackend;
#[cfg(target_os = "linux")]
pub use camera::AccessBackend;
pub use capture_broker::{
    CaptureAuthorizationId, CaptureBroker, CaptureKind, CaptureLeaseGrant, CaptureLeaseId,
    CapturePrivacyEvent, CaptureRevocation, CaptureSessionBinding, CaptureSourceView,
    LocalCaptureHandle,
};
#[cfg(target_os = "linux")]
pub use clipboard::ClipboardBackend;
pub use desktop_services::DesktopPortalServices;
#[cfg(target_os = "linux")]
pub use doc_store::{DocumentStore, DocumentStoreHandle, HostPathStore, PublishedDoc};
#[cfg(target_os = "linux")]
pub use dynamic_launcher::DynamicLauncherBackend;
#[cfg(target_os = "linux")]
pub use email::EmailBackend;
#[cfg(target_os = "linux")]
pub use file_chooser::FileChooserBackend;
pub use file_picker_spawner::{
    FilePickerOptions, FilePickerRequest, FilePickerSpawner, FilePickerSpawnerHandle,
    MockFilePickerSpawner, NullFilePickerSpawner, PickerFilter,
};
#[cfg(target_os = "linux")]
pub use global_shortcuts::GlobalShortcutsBackend;
#[cfg(target_os = "linux")]
pub use inhibit::InhibitBackend;
#[cfg(target_os = "linux")]
pub use input_capture::{
    CaptureZone, CapturedInput, InputCaptureBackend, InputCaptureCompositorHandle,
};
#[cfg(target_os = "linux")]
pub use lockdown::LockdownBackend;
#[cfg(target_os = "linux")]
pub use notification::NotificationBackend;
#[cfg(target_os = "linux")]
pub use permission_store::PermissionStoreBackend;
pub use picker_queue::{FilePickerQueue, PickerScope};
#[cfg(target_os = "linux")]
pub use print::{
    LpSpooler, NullPrintSpooler, PrintBackend, PrintJobRequest, PrintSpooler, PrintSpoolerHandle,
    RecordingSpooler,
};
#[cfg(target_os = "linux")]
pub use remote_desktop::{
    ChannelInjector, RecordingInjector, RemoteDesktopBackend, RemoteInputInjector,
    RemoteInputInjectorHandle,
};
#[cfg(target_os = "linux")]
pub use screencast::ScreenCastBackend;
#[cfg(target_os = "linux")]
pub use screencast_pipewire::{
    MockScreenCastProvisioner, PipewireStreamInfo, ProvisioningError, RealPipewireProvisioner,
    ScreenCastStreamProvisioner, ScreenCastStreamProvisionerHandle,
    UnavailableScreenCastProvisioner,
};
#[cfg(target_os = "linux")]
pub use screenshot::ScreenshotBackend;
#[cfg(target_os = "linux")]
pub use secret::SecretBackend;
#[cfg(target_os = "linux")]
pub use settings::SettingsBackend;
#[cfg(target_os = "linux")]
pub use usb::UsbBackend;
#[cfg(target_os = "linux")]
pub use wallpaper::WallpaperBackend;

use crate::shell::state::WorkspaceAuthority as _;
use std::collections::HashMap;

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

use crate::shell::state::{
    AppId, CapOverride, Capability, Expiry, FsMode, GRANT_ALWAYS, GRANT_ONCE, GRANT_SESSION,
    GrantScope, ResourceRef, Verb, Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle,
};

/// Identity of the sandboxed app that issued a portal request.
///
/// Production fills this in from the bridge tag + per-workspace PID→app
/// map; tests construct a `PortalCaller` directly for a deterministic round-trip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortalCaller {
    /// Workspace the caller was spawned into.
    pub workspace_id: WorkspaceId,
    /// Wayland-style app id, used to bind policy and private portal consent.
    pub app_id: AppId,
}

impl PortalCaller {
    pub fn new(workspace_id: WorkspaceId, app_id: impl Into<AppId>) -> Self {
        Self {
            workspace_id,
            app_id: app_id.into(),
        }
    }
}

/// Capability *kind* — `grants` stores full [`Capability`] enums (scope,
/// path, hosts, etc.), but the portal only ever asks "is there any grant
/// of *kind* X for this app?". This is that filter's kind-side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilityKind {
    FilesystemRead,
    FilesystemReadWrite,
    Camera,
    Microphone,
    Screencast,
    RemoteDesktop,
    InputCapture,
    Print,
    Network,
}

impl CapabilityKind {
    const fn name(self) -> &'static str {
        match self {
            Self::FilesystemRead => "filesystem-read",
            Self::FilesystemReadWrite => "filesystem-read-write",
            Self::Camera => "camera",
            Self::Microphone => "microphone",
            Self::Screencast => "screencast",
            Self::RemoteDesktop => "remote-desktop",
            Self::InputCapture => "input-capture",
            Self::Print => "print",
            Self::Network => "network",
        }
    }

    /// Does this capability variant satisfy a request for `self`?
    pub fn matches(self, cap: &Capability) -> bool {
        match (self, cap) {
            (CapabilityKind::FilesystemRead, Capability::Filesystem { mode, .. }) => {
                matches!(mode, FsMode::Read | FsMode::ReadWrite)
            }
            (CapabilityKind::FilesystemReadWrite, Capability::Filesystem { mode, .. }) => {
                matches!(mode, FsMode::ReadWrite)
            }
            (CapabilityKind::Camera, Capability::Camera { .. })
            | (CapabilityKind::Microphone, Capability::Microphone { .. })
            | (CapabilityKind::Screencast, Capability::Screencast { .. })
            | (CapabilityKind::RemoteDesktop, Capability::RemoteDesktop { .. })
            | (CapabilityKind::Print, Capability::Print { .. })
            | (CapabilityKind::Network, Capability::Network { .. }) => true,
            _ => false,
        }
    }
}

/// Handle shared by portal interfaces for policy grants and private portal
/// consent. Workspace capabilities remain collaborative policy; choices made
/// in a local privacy prompt are deliberately kept in a separate local store.
#[derive(Clone)]
pub struct GrantStoreHandle {
    tree: SharedTree,
    portal_consents: Arc<Mutex<PortalConsentState>>,
    portal_consent_path: Option<Arc<PathBuf>>,
}

/// The workspace tree as the portal interfaces hold it.
///
/// zbus implements every portal interface method through `async_trait`, whose
/// `Self: 'async_trait` bounds keep rustc from reusing auto-trait proofs
/// between methods. Each generated method would otherwise prove the whole
/// workspace tree `Send + Sync` again. Behind this trait object the proof
/// stops at the object's bounds; the tree itself is checked once, where the
/// handle is built.
#[derive(Clone)]
struct SharedTree(Arc<dyn TreeSource + Send + Sync>);

trait TreeSource {
    fn handle(&self) -> &WorkspaceTreeHandle;
}

impl TreeSource for WorkspaceTreeHandle {
    fn handle(&self) -> &WorkspaceTreeHandle {
        self
    }
}

impl std::ops::Deref for SharedTree {
    type Target = WorkspaceTreeHandle;
    fn deref(&self) -> &WorkspaceTreeHandle {
        self.0.handle()
    }
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct PortalConsentFile {
    always: BTreeSet<String>,
}

#[derive(Default)]
struct PortalConsentState {
    session: BTreeSet<String>,
    always: BTreeSet<String>,
}

impl GrantStoreHandle {
    /// Construct a grant store over the shared workspace-tree handle. In
    /// production the same handle is passed to `ChromeState` and `WorkspaceTreeReplica` so every subsystem mutates the same tree.
    pub fn new(tree: WorkspaceTreeHandle) -> Self {
        Self::with_consent_path(tree, default_portal_consent_path())
    }

    fn with_consent_path(tree: WorkspaceTreeHandle, path: Option<PathBuf>) -> Self {
        let always = path
            .as_deref()
            .and_then(|path| fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<PortalConsentFile>(&bytes).ok())
            .map(|file| file.always)
            .unwrap_or_default();
        Self {
            tree: SharedTree(Arc::new(tree)),
            portal_consents: Arc::new(Mutex::new(PortalConsentState {
                session: BTreeSet::new(),
                always,
            })),
            portal_consent_path: path.map(Arc::new),
        }
    }

    /// Build a single-workspace store for unit tests / examples.
    pub fn test_with_workspace(ws: Workspace) -> (Self, WorkspaceId) {
        let ws_id = ws.id;
        // Type-agnostic over the desktop model backing `WorkspaceTree::desktop`.
        let mut tree = WorkspaceTree::default();
        tree.desktop.active = ws_id;
        tree.desktop.workspaces.push(ws);
        (Self::with_consent_path(tree.shared(), None), ws_id)
    }

    /// Lookup the strongest existing grant (`Always` > `Session` > `Once`)
    /// of `kind` for `caller`. `None` means "no grant" → the portal will enqueue a prompt.
    ///
    /// Acquires a read lock; returns `None` if the workspace doesn't exist (drained workspace, racy portal request) — defaults safely to deny.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the read guard protects `ws`, which the closure inside `.collect()` needs \
                  through the inner block's final statement — already dropped at the earliest \
                  possible point"
    )]
    pub fn lookup(&self, caller: &PortalCaller, kind: CapabilityKind) -> Option<GrantScope> {
        let consent_key = self.portal_consent_key(caller, kind)?;
        if let Ok(consents) = self.portal_consents.lock() {
            if consents.always.contains(&consent_key) {
                return Some(GRANT_ALWAYS);
            }
            if consents.session.contains(&consent_key) {
                return Some(GRANT_SESSION);
            }
        }
        // Collect the matching expiries while the lock is held, then fold
        // them outside it — nothing past this point touches the tree.
        let matching_expiries: Vec<GrantScope> = {
            let tree = self.tree.read();
            let ws = tree
                .desktop
                .workspaces
                .iter()
                .find(|w| w.id == caller.workspace_id)?;
            ws.app_capabilities(&caller.app_id)
                .into_iter()
                .filter_map(|cap| {
                    let desc = Capability::from_capability(cap)?;
                    kind.matches(&desc).then_some(cap.expiry)
                })
                .collect()
        };
        let mut best: Option<GrantScope> = None;
        for expiry in matching_expiries {
            best = Some(match best {
                Some(prev) => stronger_scope(prev, expiry),
                None => expiry,
            });
        }
        best
    }

    /// The authoritative "may this app do this?" check.
    ///
    /// Unlike [`Self::lookup`] (which answers "is there a grant of this
    /// shape" for the prompt logic), this asks the capability model directly
    /// about a concrete resource, so a filesystem grant on a directory
    /// authorises a file inside it and a network grant on one host doesn't
    /// authorise another. Fail-closed on every unknown: a missing workspace, an unknown app, an expired capability all deny.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the read guard protects `ws`, which `ws.allows` needs through the final \
                  call — already dropped at the earliest possible point, the function's end"
    )]
    pub fn permits(&self, caller: &PortalCaller, resource: &ResourceRef, verb: Verb) -> bool {
        let tree = self.tree.read();
        let Some(ws) = tree
            .desktop
            .workspaces
            .iter()
            .find(|w| w.id == caller.workspace_id)
        else {
            return false;
        };
        let actor = ws.actor_for_app(&caller.app_id);
        ws.allows(&actor, resource, verb)
    }

    /// Persist `capability` under `caller.app_id` in `caller.workspace_id` —
    /// used after the user clicks `Session` or `Always` on the prompt.
    ///
    /// Acquires the write lock briefly, mutates both the cache map and the
    /// automerge doc; peer-sync picks up the change on the next `export_changes` tick.
    pub fn insert(&self, caller: &PortalCaller, capability: &Capability) -> bool {
        let mut tree = self.tree.write();
        let Some(ws) = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|w| w.id == caller.workspace_id)
        else {
            return false;
        };
        let ids = ws.grant_app(&caller.app_id, capability);
        // Mirror into the automerge doc so peer-sync ships it and a restart reloads it; tolerant of the doc not being seeded yet.
        let written: Vec<CapOverride> = ws
            .capabilities
            .iter()
            .filter(|c| ids.contains(&c.id))
            .cloned()
            .collect();
        for cap in written {
            tree.doc_insert_capability(caller.workspace_id, &cap);
        }
        true
    }

    /// Record a local portal prompt choice without adding it to the shared
    /// workspace document. `Always` choices are atomically persisted in the
    /// user's private state directory; session choices disappear with the
    /// compositor process.
    pub fn insert_portal_consent(&self, caller: &PortalCaller, capability: &Capability) -> bool {
        let Some((kind, scope)) = capability_kind_and_scope(capability) else {
            return false;
        };
        self.insert_portal_consent_kind(caller, kind, scope)
    }

    /// Persist a local-only portal decision whose security meaning has no
    /// workspace `Capability` representation (currently InputCapture).
    pub fn insert_portal_consent_kind(
        &self,
        caller: &PortalCaller,
        kind: CapabilityKind,
        scope: GrantScope,
    ) -> bool {
        let Some(key) = self.portal_consent_key(caller, kind) else {
            return false;
        };
        self.insert_portal_consent_key(key, scope)
    }

    fn insert_portal_consent_key(&self, key: String, scope: GrantScope) -> bool {
        let Ok(mut state) = self.portal_consents.lock() else {
            return false;
        };
        if matches!(scope, GRANT_ALWAYS) {
            state.always.insert(key.clone());
            if let Some(path) = self.portal_consent_path.as_deref()
                && persist_portal_consents(path, &state.always).is_err()
            {
                // Preserve availability for this compositor session without
                // pretending the user's durable choice was committed.
                state.always.remove(&key);
                state.session.insert(key);
            }
        } else if matches!(scope, GRANT_SESSION) {
            state.session.insert(key);
        }
        true
    }

    fn portal_consent_key(&self, caller: &PortalCaller, kind: CapabilityKind) -> Option<String> {
        let tree = self.tree.read();
        tree.desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.id == caller.workspace_id)
            .filter(|workspace| !workspace.room.is_nil())
            .map(|workspace| {
                // A local slot survives joining a different room. Consent
                // belongs to the durable room, never to that reusable slot.
                // The prefix also leaves legacy numeric-slot records inert:
                // their original room cannot be recovered safely.
                format!(
                    "room:{}\u{1f}{}\u{1f}{}",
                    workspace.room,
                    caller.app_id,
                    kind.name()
                )
            })
    }

    /// True if `caller` already holds a filesystem grant whose path covers
    /// `path` — an exact match or an ancestor directory. The file-drop
    /// router calls this before recording a drop grant so a second drop of
    /// the same file doesn't append a duplicate `Capability::Filesystem`.
    ///
    /// Acquires a read lock; returns `false` if the workspace or app has no grants (default-deny, so the caller records the grant).
    pub fn filesystem_path_granted(&self, caller: &PortalCaller, path: &std::path::Path) -> bool {
        self.filesystem_path_allows(caller, path, Verb::Read)
    }

    /// True if `caller`'s grants let it `verb` `path` (read it, or with
    /// [`Verb::Write`] replace it).
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the read guard protects `ws`, which `ws.allows` needs through the final call"
    )]
    pub fn filesystem_path_allows(
        &self,
        caller: &PortalCaller,
        path: &std::path::Path,
        verb: Verb,
    ) -> bool {
        let tree = self.tree.read();
        let Some(ws) = tree
            .desktop
            .workspaces
            .iter()
            .find(|w| w.id == caller.workspace_id)
        else {
            return false;
        };
        // Asks the capability model rather than re-deriving prefix logic:
        // `Scope::FsPath` already covers its subtree on component boundaries, the same rule the filesystem itself enforces.
        let actor = ws.actor_for_app(&caller.app_id);
        ws.allows(
            &actor,
            &ResourceRef::FsPath(path.to_string_lossy().into_owned()),
            verb,
        )
    }

    /// The filesystem paths `caller`'s workspace grants it, in grant order.
    pub fn granted_paths(&self, caller: &PortalCaller) -> Vec<PathBuf> {
        let tree = self.tree.read();
        tree.desktop
            .workspaces
            .iter()
            .find(|w| w.id == caller.workspace_id)
            .map(|ws| {
                ws.app_capabilities(&caller.app_id)
                    .into_iter()
                    .filter_map(|capability| match &capability.scope {
                        collab_types::caps::Scope::FsPath(path) => Some(PathBuf::from(path)),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `app_id` in the workspace whose room the launcher named `room`.
    pub fn caller_in_room(&self, room: &str, app_id: &str) -> Option<PortalCaller> {
        let tree = self.tree.read();
        tree.desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == room)
            .map(|workspace| PortalCaller::new(workspace.id, app_id))
    }

    /// Direct tree-handle accessor for tests needing to assert on multiple
    /// fields at once. Production callers go through [`Self::lookup`] / [`Self::insert`].
    pub fn tree_handle(&self) -> WorkspaceTreeHandle {
        WorkspaceTreeHandle::clone(&self.tree)
    }

    /// Back-compat alias for [`Self::tree_handle`]. A handful of older
    /// tests/portals still reference `.tree()`; both now return the shared `WorkspaceTreeHandle`.
    // NOT cfg-gated: reachable from the shots harness, which is a release build.
    pub fn tree(&self) -> WorkspaceTreeHandle {
        WorkspaceTreeHandle::clone(&self.tree)
    }

    /// Resolve a launcher-verified live application to one concrete local
    /// workspace. Multiple live rooms are intentionally ambiguous unless a
    /// future parent-window resolver identifies the exact surface.
    pub fn resolve_live_app(&self, app_id: &str) -> Option<PortalCaller> {
        let rooms = crate::shell::launcher::verified_workspaces_for_app(app_id);
        let tree = self.tree.read();
        let mut matches = tree
            .desktop
            .workspaces
            .iter()
            .filter(|workspace| rooms.iter().any(|room| room == &workspace.room.to_string()));
        let workspace = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        Some(PortalCaller::new(workspace.id, app_id))
    }
}

/// The longer-lived of two expiries.
///
/// Delegates to [`Expiry::longest`] rather than re-deriving the order —
/// two implementations of "strongest wins" would eventually disagree about whether a permanent grant is permanent.
fn stronger_scope(a: GrantScope, b: GrantScope) -> GrantScope {
    Expiry::longest(a, b)
}

fn capability_kind_and_scope(capability: &Capability) -> Option<(CapabilityKind, GrantScope)> {
    Some(match capability {
        Capability::Filesystem { mode, scope, .. } => (
            if matches!(mode, FsMode::ReadWrite) {
                CapabilityKind::FilesystemReadWrite
            } else {
                CapabilityKind::FilesystemRead
            },
            *scope,
        ),
        Capability::Camera { scope } => (CapabilityKind::Camera, *scope),
        Capability::Microphone { scope } => (CapabilityKind::Microphone, *scope),
        Capability::Screencast { scope } => (CapabilityKind::Screencast, *scope),
        Capability::RemoteDesktop { scope } => (CapabilityKind::RemoteDesktop, *scope),
        Capability::Print { scope } => (CapabilityKind::Print, *scope),
        Capability::Network { scope, .. } => (CapabilityKind::Network, *scope),
        // The shell runtime keeps this decision; no portal asks for it.
        Capability::HostShell { .. } => return None,
    })
}

/// A user-private persistent root.  Never fall back to a predictable shared
/// `/tmp` directory for portal authority, secrets, or user choices: without a
/// per-user state/runtime directory persistence fails closed instead.
/// Both variables follow the XDG rule: an empty or relative value is ignored
/// rather than resolved against the compositor's working directory.
pub(crate) fn private_state_root() -> Option<PathBuf> {
    tilo_paths::state_dir().or_else(tilo_paths::runtime_dir)
}

fn default_portal_consent_path() -> Option<PathBuf> {
    private_state_root().map(|root| root.join("tilo/portal/consents.json"))
}

fn persist_portal_consents(path: &Path, always: &BTreeSet<String>) -> std::io::Result<()> {
    tilo_files::native::atomic::json::write_private(
        path,
        &PortalConsentFile {
            always: always.clone(),
        },
    )
}

/// The user-prompt the portal needs to surface, waiting on a choice.
pub struct PendingGrant {
    pub caller: PortalCaller,
    pub capability: Capability,
    /// Portal request handle that owns this prompt, when it has one. This is
    /// not app supplied data; it is used solely to remove exactly this queued
    /// sheet when its authenticated Request object is closed.
    request_key: Option<String>,
    /// One-shot reply channel: the shell's permission-prompt pump fulfils
    /// this on a user click; test mode fulfils it synchronously from [`PromptResolver`].
    pub reply: oneshot::Sender<PromptOutcome>,
}

/// Outcome of a permission prompt — mirrors [`ui::widget::PromptChoice`]
/// but keeps the portal module dependency-free from `quick_ui` for the lookup path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptOutcome {
    AllowOnce,
    AllowSession,
    AllowAlways,
    Deny,
}

impl PromptOutcome {
    pub fn is_allowed(self) -> bool {
        !matches!(self, PromptOutcome::Deny)
    }
    pub fn to_scope(self) -> Option<GrantScope> {
        match self {
            PromptOutcome::AllowOnce => Some(GRANT_ONCE),
            PromptOutcome::AllowSession => Some(GRANT_SESSION),
            PromptOutcome::AllowAlways => Some(GRANT_ALWAYS),
            PromptOutcome::Deny => None,
        }
    }
}

/// How the prompt queue resolves pending grants. Production uses
/// [`PromptResolver::Channel`] (buffered, drained by the shell's
/// permission-prompt pump); tests use `AutoAllow` / `AutoDeny` so the round-trip completes without a GUI thread.
#[derive(Clone)]
pub enum PromptResolver {
    /// Push onto the queue and let the prompt-pump fulfil the oneshot.
    Channel,
    /// Reply with `AllowOnce` immediately — for round-trip tests.
    AutoAllow,
    /// Reply with `Deny` immediately — for deny-path tests.
    AutoDeny,
    /// Reply with a fixed outcome — for scope-specific tests.
    AutoOutcome(PromptOutcome),
}

/// Handle the portal interfaces hold to push new prompts onto the queue; cloneable so each interface impl gets its own clone.
#[derive(Clone)]
pub struct PromptQueueHandle {
    resolver: PromptResolver,
    state: Arc<Mutex<PromptQueueState>>,
}

#[derive(Default)]
struct PromptQueueState {
    pending: Vec<PendingGrant>,
    /// Keys cancelled before their async resolver reached `enqueue`. Keeping
    /// this beside the queue closes the close/enqueue race without ever
    /// cancelling an unrelated prompt.
    cancelled: HashSet<String>,
}

impl PromptQueueHandle {
    pub fn new(resolver: PromptResolver) -> Self {
        Self {
            resolver,
            state: Arc::new(Mutex::new(PromptQueueState::default())),
        }
    }

    /// Enqueue a new request and return the reply receiver; the caller
    /// `.await`s it while the prompt path / auto-resolver fulfils it.
    pub fn enqueue(
        &self,
        caller: PortalCaller,
        capability: Capability,
    ) -> oneshot::Receiver<PromptOutcome> {
        self.enqueue_inner(caller, capability, None)
    }

    /// Like [`Self::enqueue`], but binds the queued sheet to a concrete
    /// request lifecycle object so `Request.Close` can safely remove it.
    pub fn enqueue_for_request(
        &self,
        caller: PortalCaller,
        capability: Capability,
        request_key: String,
    ) -> oneshot::Receiver<PromptOutcome> {
        self.enqueue_inner(caller, capability, Some(request_key))
    }

    fn enqueue_inner(
        &self,
        caller: PortalCaller,
        capability: Capability,
        request_key: Option<String>,
    ) -> oneshot::Receiver<PromptOutcome> {
        let (tx, rx) = oneshot::channel();
        let pending = PendingGrant {
            caller,
            capability,
            request_key: request_key.clone(),
            reply: tx,
        };
        match self.resolver {
            PromptResolver::Channel => {
                if let Ok(mut state) = self.state.lock() {
                    if request_key.is_some_and(|key| state.cancelled.remove(&key)) {
                        let _ = pending.reply.send(PromptOutcome::Deny);
                    } else {
                        state.pending.push(pending);
                    }
                }
            }
            PromptResolver::AutoAllow => {
                let _ = pending.reply.send(PromptOutcome::AllowOnce);
            }
            PromptResolver::AutoDeny => {
                let _ = pending.reply.send(PromptOutcome::Deny);
            }
            PromptResolver::AutoOutcome(o) => {
                let _ = pending.reply.send(o);
            }
        }
        rx
    }

    /// Cancel precisely one request-owned prompt. The removed receiver is
    /// resolved as Deny so the D-Bus method returns promptly; unkeyed prompts
    /// and other request handles remain untouched.
    pub fn cancel_request(&self, request_key: &str) -> bool {
        let removed = self.state.lock().ok().and_then(|mut state| {
            state.cancelled.insert(request_key.into());
            state
                .pending
                .iter()
                .position(|pending| pending.request_key.as_deref() == Some(request_key))
                .map(|index| state.pending.remove(index))
        });
        if let Some(pending) = removed {
            let _ = pending.reply.send(PromptOutcome::Deny);
            true
        } else {
            // The request may be between lifecycle registration and prompt
            // enqueue; retain the key so that race resolves as Deny instead.
            true
        }
    }

    /// Cancel a request that this in-process owner already enqueued. Unlike a
    /// D-Bus close-before-enqueue race, it needs no retained cancellation key.
    pub(crate) fn cancel_enqueued_request(&self, request_key: &str) {
        let removed = self.state.lock().ok().and_then(|mut state| {
            state
                .pending
                .iter()
                .position(|pending| pending.request_key.as_deref() == Some(request_key))
                .map(|index| state.pending.remove(index))
        });
        if let Some(pending) = removed {
            let _ = pending.reply.send(PromptOutcome::Deny);
        }
    }

    /// Drain pending prompts for the shell's per-frame pump to convert into
    /// `ChromeState::pending_grants` entries; each drained entry still owns its reply channel for the caller to fulfil.
    pub fn drain(&self) -> Vec<PendingGrant> {
        match self.state.lock() {
            Ok(mut state) => std::mem::take(&mut state.pending),
            Err(_) => Vec::new(),
        }
    }

    /// Number of queued prompts — for tests asserting queue length before the pump drains it.
    pub fn queued_len(&self) -> usize {
        self.state.lock().map_or(0, |state| state.pending.len())
    }
}

/// Configuration handed to [`serve`] / interface constructors.
#[derive(Clone)]
#[cfg(target_os = "linux")]
pub struct PortalContext {
    pub grants: GrantStoreHandle,
    pub prompts: PromptQueueHandle,
    /// Optional D-Bus unique-name → `PortalCaller` lookup used by focused
    /// tests. Production authenticates the portal frontend's current bus owner
    /// and resolves its asserted app id against the compositor's live,
    /// Wayland-authenticated application instances.
    pub caller_map: CallerMap,
    /// The `FileChooser` backend calls [`FilePickerSpawner::spawn`] on
    /// `Allow`; tests pass a [`MockFilePickerSpawner`] with canned paths.
    /// Defaults to [`NullFilePickerSpawner`], which cancels every request (`response=1`).
    pub file_picker: FilePickerSpawnerHandle,
    /// The `ScreenCast` backend calls
    /// [`ScreenCastStreamProvisioner::provision_stream`] on `Start` Allow; tests use [`MockScreenCastProvisioner`] instead of the system Pipewire daemon.
    pub screencast_provisioner: ScreenCastStreamProvisionerHandle,
    /// The `RemoteDesktop` backend hands each granted `Notify*` action here;
    /// production wires a [`ChannelInjector`] the compositor loop drains, and tests use the null handle.
    pub remote_input_injector: RemoteInputInjectorHandle,
    /// Compositor authority for InputCapture.  Native mode supplies live
    /// output zones; hosted/headless contexts deliberately keep it unavailable.
    pub input_capture: InputCaptureCompositorHandle,
    /// How a picked path is made reachable by the app that asked for it —
    /// see [`doc_store`] for why recording the grant alone isn't enough for an app already running.
    pub documents: DocumentStoreHandle,
    /// The Print backend hands each consented job here; production runs
    /// `lp`, tests inject a [`RecordingSpooler`] — the only component in the print path that touches CUPS, living on *this* side of the sandbox.
    pub print_spooler: PrintSpoolerHandle,
    /// Shared mailbox into the compositor-owned notification chrome. Portal
    /// notifications and the freedesktop notification service deliberately
    /// use the same id allocator and queue.
    pub notifications: crate::notifications::NotificationBridge,
    /// Shell-owned adapters for portals whose work is performed by compositor
    /// UI or another privileged desktop service.
    pub desktop_services: DesktopPortalServices,
    /// Fixed authenticator in p2p tests. Production leaves this empty and
    /// [`serve`] binds authentication to its actual session-bus connection.
    pub authenticator: Option<PortalAuthenticator>,
}

#[cfg(target_os = "linux")]
impl PortalContext {
    /// Build a context wired with null spawners — `FileChooser` cancels every
    /// request and `ScreenCast` explicitly reports that no capture producer is
    /// available. Used by tests that only care about the response-code path.
    pub fn new_with_null_spawners(
        grants: GrantStoreHandle,
        prompts: PromptQueueHandle,
        caller_map: CallerMap,
    ) -> Self {
        Self {
            grants,
            prompts,
            caller_map,
            file_picker: FilePickerSpawnerHandle::new(Arc::new(NullFilePickerSpawner::cancel())),
            screencast_provisioner: ScreenCastStreamProvisionerHandle::new(Arc::new(
                UnavailableScreenCastProvisioner::new(
                    "this Tilo session has no PipeWire capture producer",
                ),
            )),
            remote_input_injector: RemoteInputInjectorHandle::null(),
            input_capture: InputCaptureCompositorHandle::unavailable(),
            documents: DocumentStoreHandle::host_paths(),
            // The spooler is a process invocation; a headless context without
            // CUPS reports no destinations.
            print_spooler: PrintSpoolerHandle::cups(),
            notifications: crate::notifications::NotificationBridge::new(),
            desktop_services: DesktopPortalServices::default(),
            authenticator: None,
        }
    }
}

/// In-memory D-Bus unique-name → `PortalCaller` lookup. `Arc<Mutex<…>>`
/// so it's cheap to clone across interface impls and pushable from a different thread than the D-Bus reader.
#[derive(Clone, Default)]
#[cfg(any(test, target_os = "linux"))]
pub struct CallerMap {
    inner: Arc<Mutex<HashMap<String, PortalCaller>>>,
    /// Test-only forced override: every `lookup` returns this caller
    /// regardless of key, for transports (p2p socket pairs) with no unique-name on the request header.
    forced: Arc<Mutex<Option<PortalCaller>>>,
}

#[cfg(any(test, target_os = "linux"))]
impl CallerMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, dbus_unique_name: impl Into<String>, caller: PortalCaller) {
        if let Ok(mut m) = self.inner.lock() {
            m.insert(dbus_unique_name.into(), caller);
        }
    }

    pub fn lookup(&self, dbus_unique_name: &str) -> Option<PortalCaller> {
        if let Ok(forced) = self.forced.lock()
            && let Some(c) = forced.as_ref()
        {
            return Some(c.clone());
        }
        let m = self.inner.lock().ok()?;
        m.get(dbus_unique_name).cloned()
    }

    /// Test-only knob: every subsequent `lookup` returns `caller` regardless of key, for transports that never surface one.
    pub fn force_caller_for_tests(&self, caller: PortalCaller) {
        if let Ok(mut f) = self.forced.lock() {
            *f = Some(caller);
        }
    }
}

/// Shared sub-flow every interface uses: lookup or prompt, return the
/// outcome. Kept here so "Always-grant skips the prompt" is asserted by one test all three interfaces share.
/// Grant and prompt authority used by the shared consent flow.
pub trait ConsentContext {
    fn grants(&self) -> &GrantStoreHandle;
    fn prompts(&self) -> &PromptQueueHandle;
}
impl ConsentContext for (GrantStoreHandle, PromptQueueHandle) {
    fn grants(&self) -> &GrantStoreHandle {
        &self.0
    }
    fn prompts(&self) -> &PromptQueueHandle {
        &self.1
    }
}
#[cfg(target_os = "linux")]
impl ConsentContext for PortalContext {
    fn grants(&self) -> &GrantStoreHandle {
        &self.grants
    }
    fn prompts(&self) -> &PromptQueueHandle {
        &self.prompts
    }
}

pub mod flow {
    use super::{
        Capability, CapabilityKind, ConsentContext, GRANT_ALWAYS, GRANT_ONCE, PortalCaller,
        PromptOutcome,
    };

    pub async fn resolve(
        ctx: &impl ConsentContext,
        caller: PortalCaller,
        kind: CapabilityKind,
        cap_template: Capability,
    ) -> PromptOutcome {
        resolve_inner(ctx, caller, kind, cap_template, None).await
    }

    /// Request-owned variant of [`resolve`]. It lets a portal close remove
    /// the matching visible sheet rather than merely discarding its result.
    pub async fn resolve_for_request(
        ctx: &impl ConsentContext,
        caller: PortalCaller,
        kind: CapabilityKind,
        cap_template: Capability,
        request_key: String,
    ) -> PromptOutcome {
        resolve_inner(ctx, caller, kind, cap_template, Some(request_key)).await
    }

    async fn resolve_inner(
        ctx: &impl ConsentContext,
        caller: PortalCaller,
        kind: CapabilityKind,
        cap_template: Capability,
        request_key: Option<String>,
    ) -> PromptOutcome {
        let Some(consent_key) = ctx.grants().portal_consent_key(&caller, kind) else {
            return PromptOutcome::Deny;
        };
        // Fast path — `Always`-scoped grant already present.
        if matches!(ctx.grants().lookup(&caller, kind), Some(GRANT_ALWAYS)) {
            return PromptOutcome::AllowAlways;
        }
        let rx = ctx
            .prompts()
            .enqueue_inner(caller.clone(), cap_template, request_key);
        match rx.await {
            Ok(outcome) => finish(ctx, &caller, kind, consent_key, outcome),
            // A dead channel means the prompt pump shut down mid-flight — deny rather than hang the caller forever.
            Err(_) => PromptOutcome::Deny,
        }
    }

    fn finish(
        ctx: &impl ConsentContext,
        caller: &PortalCaller,
        kind: CapabilityKind,
        consent_key: String,
        outcome: PromptOutcome,
    ) -> PromptOutcome {
        if ctx.grants().portal_consent_key(caller, kind).as_ref() != Some(&consent_key) {
            return PromptOutcome::Deny;
        }
        if let Some(scope) = outcome.to_scope()
            && !matches!(scope, GRANT_ONCE)
        {
            // Persist the identity approved by this prompt. Recomputing it
            // here could bind an old approval to a newly joined room.
            ctx.grants().insert_portal_consent_key(consent_key, scope);
        }
        outcome
    }
}

/// The document store this session should use — one place to make the choice. See [`doc_store`] for what the host-path store does and doesn't reach.
#[cfg(target_os = "linux")]
pub fn documents_for_session() -> DocumentStoreHandle {
    DocumentStoreHandle::host_paths()
}

/// Bus name we register under. xdg-desktop-portal expects backends to
/// own `org.freedesktop.impl.portal.desktop.<vendor>`; matched against
/// `portals/<vendor>.portal` (see
/// `os/usr/share/xdg-desktop-portal/portals/tilo.portal`). The compositor is
/// deliberately the sole owner; there is no competing activation service.
pub const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.tilo";

/// Object path the portal's frontend dispatches to; xdg-desktop-portal hard-codes `/org/freedesktop/portal/desktop`.
pub const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";

/// Spawn the portal server on `connection`, registering every supported
/// interfaces under [`OBJECT_PATH`] and requesting [`BUS_NAME`].
///
/// Returns the connection back to the caller (kept alive by the caller);
/// dropping the returned [`ServerHandle`] releases the bus name and
/// drops the connection. **Tests** pass an in-process p2p connection;
/// **production** passes `zbus::Connection::session()`.
///
/// # Errors
///
/// Returns `Err` when registering any interface object on
/// [`OBJECT_PATH`] fails. Failing to *own* [`BUS_NAME`] is not an error
/// here — it is recorded on the returned [`ServerHandle::owns_name`],
/// since a p2p test connection has no broker to request a name from.
#[cfg(target_os = "linux")]
pub async fn serve(
    connection: zbus::Connection,
    ctx: PortalContext,
) -> Result<ServerHandle, zbus::Error> {
    serve_with_inhibit(connection, ctx, tilo_power_mgmt::InhibitRegistry::new()).await
}

/// Serve the portal backend using the compositor's actual inhibitor registry.
/// Native sessions call this so an application hold reaches both the idle
/// state machine and logind; standalone test/headless servers use [`serve`].
#[cfg(target_os = "linux")]
pub async fn serve_with_inhibit(
    connection: zbus::Connection,
    ctx: PortalContext,
    inhibit_registry: tilo_power_mgmt::InhibitRegistry,
) -> Result<ServerHandle, zbus::Error> {
    serve_with_inhibit_and_clipboard(
        connection,
        ctx,
        inhibit_registry,
        clipboard::ClipboardBrokerHandle::unavailable(),
    )
    .await
}

/// Native compositor entry point.  The caller supplies its live
/// focus-gated selection mirror; hosted/headless runs intentionally use the
/// unavailable broker above rather than pretending Clipboard works.
#[cfg(target_os = "linux")]
pub async fn serve_with_inhibit_and_clipboard(
    connection: zbus::Connection,
    mut ctx: PortalContext,
    inhibit_registry: tilo_power_mgmt::InhibitRegistry,
    clipboard_broker: clipboard::ClipboardBrokerHandle,
) -> Result<ServerHandle, zbus::Error> {
    use account::AccountBackend;
    use app_chooser::AppChooserBackend;
    use background::BackgroundBackend;
    use camera::AccessBackend;
    use clipboard::ClipboardBackend;
    use dynamic_launcher::DynamicLauncherBackend;
    use email::EmailBackend;
    use file_chooser::FileChooserBackend;
    use global_shortcuts::GlobalShortcutsBackend;
    use inhibit::InhibitBackend;
    use input_capture::InputCaptureBackend;
    use lockdown::LockdownBackend;
    use notification::NotificationBackend;
    use permission_store::PermissionStoreBackend;
    use print::PrintBackend;
    use remote_desktop::RemoteDesktopBackend;
    use screencast::ScreenCastBackend;
    use screenshot::ScreenshotBackend;
    use secret::SecretBackend;
    use settings::SettingsBackend;
    use usb::UsbBackend;
    use wallpaper::WallpaperBackend;

    if ctx.authenticator.is_none() {
        let grants = ctx.grants.clone();
        let callers = ctx.caller_map.clone();
        let resolver = Arc::new(move |app_id: &str, _parent: Option<&str>| {
            callers
                .lookup("")
                .filter(|caller| caller.app_id == app_id)
                .or_else(|| grants.resolve_live_app(app_id))
        });
        ctx.authenticator = Some(if connection.unique_name().is_none() {
            PortalAuthenticator::fixed_for_tests(
                ":1.0".try_into().expect("static unique bus name"),
                resolver,
            )
        } else {
            PortalAuthenticator::session_bus(connection.clone(), resolver)
        });
    }

    let fc = FileChooserBackend::new(ctx.clone());
    // One registry owns all dynamic Request/Session objects for this D-Bus
    // connection. It binds every handle to the authenticated frontend and
    // makes Close release the portal-specific resource.
    let lifecycle = lifecycle::PortalLifecycle::new();
    lifecycle.spawn_disconnect_cleanup(connection.clone());
    let sc = ScreenCastBackend::with_lifecycle(ctx.clone(), lifecycle.clone());
    let ac = AccessBackend::with_lifecycle(ctx.clone(), lifecycle.clone());
    let rd = RemoteDesktopBackend::with_lifecycle(ctx.clone(), lifecycle.clone());
    let remote_sessions = rd.session_handle();
    let pr = PrintBackend::new(ctx.clone());
    let no = NotificationBackend::new(ctx.clone());
    let notification_portal = no.portal_handle();
    notification_portal.install_on_bridge(&ctx.notifications);
    let account = AccountBackend::with_lifecycle(ctx.clone(), lifecycle.clone());
    let chooser = AppChooserBackend::with_lifecycle(ctx.clone(), lifecycle.clone());
    let launcher =
        DynamicLauncherBackend::with_lifecycle(ctx.clone(), lifecycle.clone(), connection.clone());
    let email = EmailBackend::with_lifecycle(ctx.clone(), lifecycle.clone());
    let screenshot = ScreenshotBackend::with_lifecycle(ctx.clone(), lifecycle.clone());
    let settings_events = ctx.desktop_services.settings.subscribe();
    let settings = SettingsBackend::new(ctx.clone());
    let wallpaper = WallpaperBackend::with_lifecycle(ctx.clone(), lifecycle.clone());
    let lockdown = LockdownBackend::new(ctx.clone());
    let permissions = PermissionStoreBackend::new(ctx.clone());
    let background_service = background::ShellBackgroundService::global();
    background_service.launch_at_session_start(&ctx.grants.tree_handle());
    let background = BackgroundBackend::with_service_and_lifecycle(
        ctx.clone(),
        background::BackgroundServiceHandle::new(Arc::new(background_service)),
        lifecycle.clone(),
    );
    let input_capture =
        InputCaptureBackend::new(ctx.clone(), lifecycle.clone(), connection.clone());
    let input_capture_sessions = input_capture.session_handle();
    let clipboard = ClipboardBackend::with_session_providers(
        vec![
            clipboard::ClipboardSessionProviderHandle::new(Arc::new(remote_sessions)),
            clipboard::ClipboardSessionProviderHandle::new(Arc::new(input_capture_sessions)),
        ],
        clipboard_broker,
    )
    .with_session_wake(lifecycle.wake.clone());
    let clipboard_events = clipboard.clone();
    let shortcut_service = global_shortcuts::ShellGlobalShortcutsService::global();
    let shortcuts = GlobalShortcutsBackend::with_service(
        ctx.clone(),
        global_shortcuts::GlobalShortcutsServiceHandle::new(Arc::new(shortcut_service.clone())),
        lifecycle.clone(),
    );
    let shortcut_events = shortcuts.handle();
    shortcut_service.attach_event_bridge(shortcut_events.clone());
    let screensaver = screensaver::ScreenSaverService::new(inhibit_registry.clone());
    let inhibit = InhibitBackend::with_lifecycle(ctx.clone(), inhibit_registry, lifecycle.clone());
    let inhibit_monitors = inhibit.monitor_handle();
    input_capture.spawn_signal_emitter();
    let secret = SecretBackend::new(ctx.clone(), connection.clone());
    let usb = UsbBackend::with_lifecycle(ctx.clone(), lifecycle.clone());

    connection.object_server().at(OBJECT_PATH, fc).await?;
    connection.object_server().at(OBJECT_PATH, sc).await?;
    connection.object_server().at(OBJECT_PATH, ac).await?;
    connection.object_server().at(OBJECT_PATH, rd).await?;
    connection.object_server().at(OBJECT_PATH, pr).await?;
    connection.object_server().at(OBJECT_PATH, no).await?;
    connection.object_server().at(OBJECT_PATH, account).await?;
    connection.object_server().at(OBJECT_PATH, chooser).await?;
    connection.object_server().at(OBJECT_PATH, launcher).await?;
    connection.object_server().at(OBJECT_PATH, email).await?;
    connection
        .object_server()
        .at(OBJECT_PATH, screenshot)
        .await?;
    connection.object_server().at(OBJECT_PATH, settings).await?;
    settings::spawn_signal_emitter(connection.clone(), settings_events);
    settings::spawn_appearance_monitor(ctx.desktop_services.settings.clone());
    connection
        .object_server()
        .at(OBJECT_PATH, wallpaper)
        .await?;
    connection.object_server().at(OBJECT_PATH, lockdown).await?;
    connection
        .object_server()
        .at(OBJECT_PATH, permissions)
        .await?;
    connection
        .object_server()
        .at(OBJECT_PATH, background.clone())
        .await?;
    background::spawn_running_monitor(connection.clone(), background);
    connection
        .object_server()
        .at(OBJECT_PATH, clipboard)
        .await?;
    connection
        .object_server()
        .at(OBJECT_PATH, shortcuts)
        .await?;
    connection.object_server().at(OBJECT_PATH, inhibit).await?;
    inhibit::spawn_state_emitter(connection.clone(), inhibit_monitors);
    connection
        .object_server()
        .at(OBJECT_PATH, input_capture)
        .await?;
    connection.object_server().at(OBJECT_PATH, secret).await?;
    connection.object_server().at(OBJECT_PATH, usb).await?;
    for path in screensaver::OBJECT_PATHS {
        connection
            .object_server()
            .at(path, screensaver.clone())
            .await?;
    }
    screensaver.spawn_disconnect_cleanup(&connection);

    // Compositor-owned shortcut events are queued by `shortcut_events`; this
    // pump is the only path which emits activation signals for an approved
    // binding.
    global_shortcuts::spawn_signal_emitter(connection.clone(), shortcut_events);
    clipboard::spawn_signal_emitter(connection.clone(), clipboard_events);
    notification::spawn_action_emitter(connection.clone(), notification_portal);

    // `request_name` only makes sense on a real bus — p2p test connections
    // have no broker, so the result is *recorded* (`ServerHandle::owns_name`) rather than swallowed.
    let owns_name = match connection.request_name(BUS_NAME).await {
        Ok(()) => true,
        Err(e) => {
            tracing::debug!("portal: request_name did not succeed: {e}");
            false
        }
    };

    // Another screensaver service may already own the name (a nested
    // session); the portal is still useful without it.
    if let Err(e) = connection.request_name(screensaver::BUS_NAME).await {
        tracing::debug!("portal: {} not claimed: {e}", screensaver::BUS_NAME);
    }

    Ok(ServerHandle {
        _connection: connection,
        owns_name,
    })
}

/// RAII handle keeping the portal D-Bus connection alive; drop releases the bus name and tears down the object server.
#[cfg(target_os = "linux")]
pub struct ServerHandle {
    /// Never read: holding the connection keeps the bus name and object
    /// server alive until drop.
    _connection: zbus::Connection,
    owns_name: bool,
}

#[cfg(target_os = "linux")]
impl ServerHandle {
    /// Did this connection actually acquire [`BUS_NAME`]?
    ///
    /// `false` means the object server is up but nothing on the session bus
    /// can route to it — no broker (a p2p test connection), or another process already holding the name.
    pub fn owns_name(&self) -> bool {
        self.owns_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_ws(id: u64) -> Workspace {
        Workspace::new(WorkspaceId(id), "t")
    }

    #[tokio::test]
    async fn private_portal_prompt_cannot_approve_a_different_room() {
        for variant in 0..2 {
            for outcome in [
                PromptOutcome::AllowOnce,
                PromptOutcome::AllowSession,
                PromptOutcome::AllowAlways,
            ] {
                let (store, id) = GrantStoreHandle::test_with_workspace(empty_ws(31));
                let queue = PromptQueueHandle::new(PromptResolver::Channel);
                let ctx = (store.clone(), queue.clone());
                let caller = PortalCaller::new(id, "private-app");
                let response = async {
                    let cap = Capability::Camera { scope: GRANT_ONCE };
                    match variant {
                        0 => flow::resolve(&ctx, caller.clone(), CapabilityKind::Camera, cap).await,
                        _ => {
                            flow::resolve_for_request(
                                &ctx,
                                caller.clone(),
                                CapabilityKind::Camera,
                                cap,
                                "/request/camera".into(),
                            )
                            .await
                        }
                    }
                };
                tokio::pin!(response);
                assert!(futures_util::poll!(&mut response).is_pending());
                let prompts = queue.drain();
                assert_eq!(prompts.len(), 1);
                assert!(
                    store
                        .tree_handle()
                        .write()
                        .set_room_in_memory(id, crate::shell::state::RoomId::new())
                );
                prompts
                    .into_iter()
                    .next()
                    .unwrap()
                    .reply
                    .send(outcome)
                    .unwrap();
                assert_eq!(response.await, PromptOutcome::Deny);
                assert_eq!(store.lookup(&caller, CapabilityKind::Camera), None);
            }
        }
    }

    #[test]
    fn private_portal_consent_requires_a_durable_room_and_ignores_legacy_slots() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("consents.json");
        let (store, id) = GrantStoreHandle::test_with_workspace(empty_ws(31));
        let shared = store.tree_handle();
        let caller = PortalCaller::new(id, "private-app");
        let mut legacy = BTreeSet::new();
        legacy.insert(format!("{}\u{1f}private-app\u{1f}camera", id.0));
        persist_portal_consents(&path, &legacy).unwrap();
        let store = GrantStoreHandle::with_consent_path(shared.clone(), Some(path.clone()));
        assert_eq!(store.lookup(&caller, CapabilityKind::Camera), None);
        assert!(store.insert_portal_consent(
            &caller,
            &Capability::Camera {
                scope: GRANT_ALWAYS
            }
        ));

        // A different local slot for the same room preserves durable consent.
        let moved_id = WorkspaceId(32);
        shared.write().desktop.workspaces[0].id = moved_id;
        let moved = PortalCaller::new(moved_id, "private-app");
        let restored = GrantStoreHandle::with_consent_path(shared.clone(), Some(path));
        assert_eq!(restored.lookup(&caller, CapabilityKind::Camera), None);
        assert_eq!(
            restored.lookup(&moved, CapabilityKind::Camera),
            Some(GRANT_ALWAYS)
        );

        shared.write().desktop.workspaces[0].room = crate::shell::state::RoomId::nil();
        assert_eq!(restored.lookup(&moved, CapabilityKind::Camera), None);
        assert!(!restored.insert_portal_consent(
            &moved,
            &Capability::Camera {
                scope: GRANT_ALWAYS
            }
        ));
    }

    #[test]
    fn private_portal_consent_does_not_follow_a_workspace_into_another_room() {
        for scope in [GRANT_SESSION, GRANT_ALWAYS] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("consents.json");
            let (store, id) = GrantStoreHandle::test_with_workspace(empty_ws(31));
            let shared = store.tree_handle();
            let store = GrantStoreHandle::with_consent_path(shared.clone(), Some(path.clone()));
            let caller = PortalCaller::new(id, "private-app");
            assert!(store.insert_portal_consent(&caller, &Capability::Camera { scope }));
            assert_eq!(store.lookup(&caller, CapabilityKind::Camera), Some(scope));

            // Joining an invitation preserves the local slot and replaces
            // its collaboration room through the production join operation.
            let joined_room = crate::shell::state::RoomId::new();
            assert!(shared.write().set_room_in_memory(id, joined_room));
            assert_eq!(
                store.lookup(&caller, CapabilityKind::Camera),
                None,
                "privacy consent from the previous room must not authorize the joined room"
            );
            let restored = GrantStoreHandle::with_consent_path(shared, Some(path));
            assert_eq!(restored.lookup(&caller, CapabilityKind::Camera), None);
            assert!(restored.insert_portal_consent(&caller, &Capability::Camera { scope }));
            assert_eq!(
                restored.lookup(&caller, CapabilityKind::Camera),
                Some(scope)
            );
        }
    }

    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "st's only use is producing tree via tree(), read from through the inner \
                  block's last statement; nothing to drop earlier"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn grant_store_resolves_capabilities_scopes_and_callers() {
        // Phase `capability_kind_matches_filesystem_modes`.
        {
            let read = Capability::Filesystem {
                path: "/tmp".into(),
                mode: FsMode::Read,
                scope: GRANT_ONCE,
            };
            let rw = Capability::Filesystem {
                path: "/tmp".into(),
                mode: FsMode::ReadWrite,
                scope: GRANT_ALWAYS,
            };
            assert!(CapabilityKind::FilesystemRead.matches(&read));
            assert!(CapabilityKind::FilesystemRead.matches(&rw));
            // RW request *only* satisfied by an RW grant.
            assert!(!CapabilityKind::FilesystemReadWrite.matches(&read));
            assert!(CapabilityKind::FilesystemReadWrite.matches(&rw));
        }
        // Phase `grant_store_returns_strongest_scope`.
        {
            let mut ws = empty_ws(11);
            for scope in [GRANT_ONCE, GRANT_ALWAYS, GRANT_SESSION] {
                ws.grant_app("app.org", &Capability::Camera { scope });
            }
            let (store, ws_id) = GrantStoreHandle::test_with_workspace(ws);
            let caller = PortalCaller::new(ws_id, "app.org");
            assert_eq!(
                store.lookup(&caller, CapabilityKind::Camera),
                Some(GRANT_ALWAYS)
            );
        }
        // Phase `filesystem_path_granted_covers_exact_and_ancestor_paths`.
        {
            let mut ws = empty_ws(21);
            ws.grant_app(
                "Editor",
                &Capability::Filesystem {
                    path: "/home/u/proj".into(),
                    mode: FsMode::Read,
                    scope: GRANT_SESSION,
                },
            );
            let (store, ws_id) = GrantStoreHandle::test_with_workspace(ws);
            let caller = PortalCaller::new(ws_id, "Editor");
            // Exact match and any descendant are covered.
            assert!(store.filesystem_path_granted(&caller, std::path::Path::new("/home/u/proj")));
            assert!(
                store
                    .filesystem_path_granted(&caller, std::path::Path::new("/home/u/proj/main.ts"))
            );
            // A sibling path outside the granted subtree is NOT covered.
            assert!(
                !store.filesystem_path_granted(&caller, std::path::Path::new("/home/u/other.ts"))
            );
            // A different app id has no grants → not covered.
            let other = PortalCaller::new(ws_id, "Files");
            assert!(
                !store
                    .filesystem_path_granted(&other, std::path::Path::new("/home/u/proj/main.ts"))
            );
        }
        // Phase `grant_store_returns_none_for_missing_app`.
        {
            let ws = empty_ws(12);
            let (store, ws_id) = GrantStoreHandle::test_with_workspace(ws);
            let caller = PortalCaller::new(ws_id, "no.such.app");
            assert!(store.lookup(&caller, CapabilityKind::Camera).is_none());
        }
        // Phase `grant_store_insert_appends_into_workspace`.
        {
            let ws = empty_ws(13);
            let (store, ws_id) = GrantStoreHandle::test_with_workspace(ws);
            let caller = PortalCaller::new(ws_id, "app.org");
            assert!(store.insert(
                &caller,
                &Capability::Microphone {
                    scope: GRANT_ALWAYS
                }
            ));
            assert_eq!(
                store.lookup(&caller, CapabilityKind::Microphone),
                Some(GRANT_ALWAYS)
            );
        }
        // Phase `grant_inserted_via_portal_is_visible_in_ws_settings`.
        // A grant inserted via the portal's [`GrantStoreHandle`] must be
        // immediately visible to the shell chrome (the WS-Settings "Permissions"
        // tab reads from `ChromeState`'s shared `WorkspaceTreeHandle`), not sitting in an independent clone.
        {
            use crate::shell::chrome_state::ChromeState;
            use crate::shell::state::WorkspaceTreeHandleExt;

            let handle: WorkspaceTreeHandle = WorkspaceTreeHandle::test_default();
            let ws_id = WorkspaceId(1);
            {
                let mut tree = handle.write();
                tree.desktop.active = ws_id;
                tree.desktop.workspaces.push(empty_ws(1));
            }

            let portal = GrantStoreHandle::new(handle.clone());
            let chrome = ChromeState::new_with_handle(handle);

            // Simulate an `Allow → Session` outcome for `camera-app`.
            let caller = PortalCaller::new(ws_id, "camera-app");
            let cap = Capability::Camera {
                scope: GRANT_SESSION,
            };
            assert!(
                portal.insert(&caller, &cap),
                "portal insert must succeed on a workspace that exists",
            );

            // The production code path the Permissions tab uses, see `shell::workspace_settings::permissions_pane_rows`.
            let has_camera_app_grant = {
                let st = chrome.borrow();
                let tree = st.tree();
                let ws = tree
                    .desktop
                    .active_workspace()
                    .expect("active workspace exists");
                ws.app_grants_snapshot().contains_key("camera-app")
            };
            assert!(
                has_camera_app_grant,
                "portal-side grant insert must be visible to the shell chrome \
                 (audit C-3: shared WorkspaceTreeHandle ties subsystems together)",
            );
        }
        // Phase `caller_map_round_trips`.
        {
            let m = CallerMap::new();
            let caller = PortalCaller::new(WorkspaceId(7), "app.x");
            m.insert(":1.42", caller.clone());
            assert_eq!(m.lookup(":1.42"), Some(caller));
            assert!(m.lookup(":1.43").is_none());
        }
    }

    #[test]
    fn prompt_queue_resolves_buffers_and_cancels_exactly_one_request() {
        // Phase `prompt_queue_auto_allow_resolves_immediately`.
        {
            let q = PromptQueueHandle::new(PromptResolver::AutoAllow);
            let caller = PortalCaller::new(WorkspaceId(1), "app");
            let rx = q.enqueue(caller, Capability::Screencast { scope: GRANT_ONCE });
            // No `.await` needed — AutoAllow fills the oneshot synchronously before `enqueue` returns.
            let outcome = rx.blocking_recv().expect("oneshot");
            assert_eq!(outcome, PromptOutcome::AllowOnce);
            // Nothing should have been buffered.
            assert_eq!(q.queued_len(), 0);
        }
        // Phase `prompt_queue_channel_buffers_for_drain`.
        {
            let q = PromptQueueHandle::new(PromptResolver::Channel);
            let caller = PortalCaller::new(WorkspaceId(2), "app");
            let _rx = q.enqueue(caller.clone(), Capability::Camera { scope: GRANT_ONCE });
            assert_eq!(q.queued_len(), 1);
            let drained = q.drain();
            assert_eq!(drained.len(), 1);
            assert_eq!(drained[0].caller, caller);
            assert_eq!(q.queued_len(), 0);
        }
        // Phase `cancelling_one_request_prompt_never_removes_another`.
        {
            let q = PromptQueueHandle::new(PromptResolver::Channel);
            let caller = PortalCaller::new(WorkspaceId(3), "app");
            let cancelled = q.enqueue_for_request(
                caller.clone(),
                Capability::Camera { scope: GRANT_ONCE },
                "/request/one".into(),
            );
            let retained = q.enqueue_for_request(
                caller,
                Capability::Microphone { scope: GRANT_ONCE },
                "/request/two".into(),
            );
            assert!(q.cancel_request("/request/one"));
            assert_eq!(
                cancelled.blocking_recv().expect("cancel response"),
                PromptOutcome::Deny
            );
            assert_eq!(q.queued_len(), 1);
            let remaining = q.drain();
            assert_eq!(remaining.len(), 1);
            assert!(matches!(
                remaining[0].capability,
                Capability::Microphone { .. }
            ));
            drop(remaining);
            assert!(
                retained.blocking_recv().is_err(),
                "dropped UI sheet cancels only its own request"
            );
        }
        // Phase `cancellation_before_enqueue_closes_the_request_enqueue_race`.
        {
            let q = PromptQueueHandle::new(PromptResolver::Channel);
            assert!(q.cancel_request("/request/racing"));
            let rx = q.enqueue_for_request(
                PortalCaller::new(WorkspaceId(4), "app"),
                Capability::Camera { scope: GRANT_ONCE },
                "/request/racing".into(),
            );
            assert_eq!(
                rx.blocking_recv().expect("deny response"),
                PromptOutcome::Deny
            );
            assert_eq!(q.queued_len(), 0);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn flow_resolve_persists_only_session_and_always_outcomes() {
        // Phase `flow_resolve_fast_path_always_grant_skips_prompt`.
        {
            // Channel mode would buffer any enqueued request, so an empty queue at the end proves the fast path bypassed it.
            let mut ws = empty_ws(21);
            ws.grant_app(
                "app",
                &Capability::Camera {
                    scope: GRANT_ALWAYS,
                },
            );
            let (store, ws_id) = GrantStoreHandle::test_with_workspace(ws);
            let queue = PromptQueueHandle::new(PromptResolver::Channel);
            let caller = PortalCaller::new(ws_id, "app");
            let ctx = (store, queue.clone());

            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let outcome = rt.block_on(flow::resolve(
                &ctx,
                caller,
                CapabilityKind::Camera,
                Capability::Camera { scope: GRANT_ONCE },
            ));
            assert_eq!(outcome, PromptOutcome::AllowAlways);
            assert_eq!(queue.queued_len(), 0, "fast path must skip enqueue");
        }
        // Phase `flow_resolve_session_outcome_persists_grant`.
        {
            let ws = empty_ws(22);
            let (store, ws_id) = GrantStoreHandle::test_with_workspace(ws);
            let queue =
                PromptQueueHandle::new(PromptResolver::AutoOutcome(PromptOutcome::AllowSession));
            let caller = PortalCaller::new(ws_id, "app");
            let ctx = (store.clone(), queue);
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let outcome = rt.block_on(flow::resolve(
                &ctx,
                caller.clone(),
                CapabilityKind::Microphone,
                Capability::Microphone { scope: GRANT_ONCE },
            ));
            assert_eq!(outcome, PromptOutcome::AllowSession);
            // The Session-scoped grant should now be persisted.
            assert_eq!(
                store.lookup(&caller, CapabilityKind::Microphone),
                Some(GRANT_SESSION)
            );
            let tree = store.tree_handle();
            let tree = tree.read();
            assert!(
                tree.desktop
                    .active_workspace()
                    .unwrap()
                    .app_grants_snapshot()
                    .get("app")
                    .is_none(),
                "a local privacy choice must never enter replicated workspace state"
            );
        }
        // Phase `always_portal_consent_is_private_and_survives_restart`.
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("consents.json");
            // Type-agnostic over the desktop model backing `WorkspaceTree::desktop`.
            let mut tree = WorkspaceTree::default();
            tree.desktop.active = WorkspaceId(31);
            tree.desktop.workspaces.push(empty_ws(31));
            let shared = tree.shared();
            let caller = PortalCaller::new(WorkspaceId(31), "private-app");
            let store = GrantStoreHandle::with_consent_path(shared.clone(), Some(path.clone()));
            assert!(store.insert_portal_consent(
                &caller,
                &Capability::Camera {
                    scope: GRANT_ALWAYS,
                }
            ));
            assert_eq!(
                GrantStoreHandle::with_consent_path(shared.clone(), Some(path))
                    .lookup(&caller, CapabilityKind::Camera),
                Some(GRANT_ALWAYS)
            );
            assert!(
                shared.read().desktop.workspaces[0]
                    .app_grants_snapshot()
                    .get("private-app")
                    .is_none()
            );
        }
        // Phase `flow_resolve_once_outcome_does_not_persist`.
        {
            let ws = empty_ws(23);
            let (store, ws_id) = GrantStoreHandle::test_with_workspace(ws);
            let queue = PromptQueueHandle::new(PromptResolver::AutoAllow);
            let caller = PortalCaller::new(ws_id, "app");
            let ctx = (store.clone(), queue);
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let outcome = rt.block_on(flow::resolve(
                &ctx,
                caller.clone(),
                CapabilityKind::Camera,
                Capability::Camera { scope: GRANT_ONCE },
            ));
            assert_eq!(outcome, PromptOutcome::AllowOnce);
            assert!(
                store.lookup(&caller, CapabilityKind::Camera).is_none(),
                "Once outcomes must not persist"
            );
        }
        // Phase `flow_resolve_deny_outcome_is_propagated`.
        {
            let ws = empty_ws(24);
            let (store, ws_id) = GrantStoreHandle::test_with_workspace(ws);
            let queue = PromptQueueHandle::new(PromptResolver::AutoDeny);
            let caller = PortalCaller::new(ws_id, "app");
            let ctx = (store.clone(), queue);
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let outcome = rt.block_on(flow::resolve(
                &ctx,
                caller.clone(),
                CapabilityKind::Network,
                Capability::Network {
                    hosts: None,
                    scope: GRANT_ONCE,
                },
            ));
            assert_eq!(outcome, PromptOutcome::Deny);
            assert!(store.lookup(&caller, CapabilityKind::Network).is_none());
        }
    }
}

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use crate::shell::state::WorkspaceExt as _;
