//! `xdg-desktop-portal` backend over zbus — Sandbox Phase 4.3.
//!
//! Spec: `docs/superpowers/plans/2026-05-02-per-workspace-sandbox-permissions.md`
//! (Task 4.3, lines 335-353).
//!
//! ## What this module does
//!
//! `xdg-desktop-portal` is the freedesktop.org component that sandboxed
//! apps talk to when they need privileged operations the sandbox blocks —
//! file open/save, screencast, camera access. The host portal proxies
//! every request to a *backend* implementation that owns the
//! decision-and-UI side; this is that backend for the Nirios compositor.
//!
//! We expose these D-Bus interfaces under the service name
//! `org.freedesktop.impl.portal.desktop.qos`:
//!
//! - `org.freedesktop.impl.portal.FileChooser` — `OpenFile` / `SaveFile`
//!   / `SaveFiles`.
//! - `org.freedesktop.impl.portal.ScreenCast` — `CreateSession` /
//!   `SelectSources` / `Start` / `OpenPipeWireRemote`, plus the
//!   `AvailableSourceTypes` / `AvailableCursorModes` / `version`
//!   properties.
//! - `org.freedesktop.impl.portal.RemoteDesktop` — `CreateSession` /
//!   `SelectDevices` / `Start` / `Notify*` seat injection.
//! - `org.freedesktop.impl.portal.Access` — `AccessDialog`. The host
//!   portal's `Camera.AccessCamera` call ends up here with
//!   `app_id="camera"` (this is how `xdg-desktop-portal` does it: the
//!   Camera frontend doesn't have its own `impl.portal.Camera`
//!   interface, it reuses `Access`).
//!
//! Every method shares the same shape — peek at [`flow::resolve`]: look
//! up the caller's workspace + app id, ask the grant store whether an
//! `Always`-scoped grant covers this capability, fast-path `Allow` if
//! so; otherwise enqueue a `PendingGrant` and await the user's prompt
//! choice. The result is translated back into the portal response code
//! (0 = success, 1 = user cancelled, 2 = other error) and the
//! interface-specific results dict.
//!
//! ## Test discipline
//!
//! Tests construct a `PromptQueueHandle` in
//! [`PromptResolver::AutoAllow`] mode so a `FileChooser.OpenFile` round
//! trip never blocks on a real dialog. The companion
//! `tests/portal_test.rs` exercises one round-trip per interface plus
//! the "Always-grant skips the prompt" path against a private session
//! bus brought up via `zbus::ConnectionBuilder::p2p()` — same in-process
//! bus the upstream `xdg-desktop-portal` test fixtures use.

pub mod camera;
pub mod doc_store;
pub mod file_chooser;
pub mod file_picker_spawner;
pub mod picker_queue;
pub mod print;
pub mod remote_desktop;
pub mod screencast;
pub mod screencast_pipewire;

// Re-export the backend types so integration tests can construct them
// directly and register them on a custom `ConnectionBuilder`. The
// production entry point ([`serve`]) does the same wire-up under the
// hood; tests need it inline so the server- and client-side
// handshake can join concurrently (see `tests/portal_test.rs`).
pub use camera::AccessBackend;
pub use doc_store::{DocumentStore, DocumentStoreHandle, HostPathStore, PublishedDoc};
pub use file_chooser::FileChooserBackend;
pub use file_picker_spawner::{
    FilePickerOptions, FilePickerRequest, FilePickerSpawner, FilePickerSpawnerHandle,
    MockFilePickerSpawner, NullFilePickerSpawner, PickerFilter,
};
pub use picker_queue::{FilePickerQueue, PickerScope};
pub use print::{
    LpSpooler, NullPrintSpooler, PrintBackend, PrintJobRequest, PrintSpooler, PrintSpoolerHandle,
    RecordingSpooler,
};
pub use remote_desktop::{
    ChannelInjector, RecordingInjector, RemoteDesktopBackend, RemoteInputInjector,
    RemoteInputInjectorHandle,
};
pub use screencast::ScreenCastBackend;
pub use screencast_pipewire::{
    MockScreenCastProvisioner, PipewireStreamInfo, ProvisioningError, ScreenCastStreamProvisioner,
    ScreenCastStreamProvisionerHandle,
};

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

use crate::shell::state::{
    AppId, CapOverride, Capability, Expiry, FsMode, GRANT_ALWAYS, GRANT_ONCE, GRANT_SESSION,
    GrantScope, ResourceRef, Verb, Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle,
};

/// Identity of the sandboxed app that issued a portal request.
///
/// In production this is filled in by the connection-acceptance path
/// from the bridge tag (Phase 3) + the per-workspace PID→app map. In
/// tests, callers construct a `PortalCaller` directly so the round-trip
/// is fully deterministic without spinning up a real bwrap'd app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortalCaller {
    /// Workspace the caller was spawned into.
    pub workspace_id: WorkspaceId,
    /// Wayland-style app id. Stored verbatim into [`Workspace::grants`]'s
    /// outer key on accept.
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

/// Capability *kind* — the workspace's `grants` map stores full
/// [`Capability`] enums (with scope, path, hosts, etc.); the portal
/// only ever asks "is there any grant of *kind* X for this app?". This
/// is the kind-side of that filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapabilityKind {
    FilesystemRead,
    FilesystemReadWrite,
    Camera,
    Microphone,
    Screencast,
    RemoteDesktop,
    Print,
    Network,
}

impl CapabilityKind {
    /// Does this capability variant satisfy a request for `self`?
    pub fn matches(self, cap: &Capability) -> bool {
        match (self, cap) {
            (CapabilityKind::FilesystemRead, Capability::Filesystem { mode, .. }) => {
                matches!(mode, FsMode::Read | FsMode::ReadWrite)
            }
            (CapabilityKind::FilesystemReadWrite, Capability::Filesystem { mode, .. }) => {
                matches!(mode, FsMode::ReadWrite)
            }
            (CapabilityKind::Camera, Capability::Camera { .. }) => true,
            (CapabilityKind::Microphone, Capability::Microphone { .. }) => true,
            (CapabilityKind::Screencast, Capability::Screencast { .. }) => true,
            (CapabilityKind::RemoteDesktop, Capability::RemoteDesktop { .. }) => true,
            (CapabilityKind::Print, Capability::Print { .. }) => true,
            (CapabilityKind::Network, Capability::Network { .. }) => true,
            _ => false,
        }
    }
}

/// Handle the portal hands to its interface impls so they can consult /
/// mutate the workspace's `grants` map.
///
/// Holds a [`WorkspaceTreeHandle`] — the same `Arc<parking_lot::RwLock<_>>`
/// the shell chrome and the peer-sync replica clone from (C-3). Portal
/// `insert` writes land on the shared tree, so the shell's Permissions
/// tab observes them immediately and peer-sync's next `export_changes_since`
/// ships them to other peers.
///
/// Cloneable so each interface impl can hold its own clone without
/// locking-strategy concerns. The handle is just an `Arc` bump.
#[derive(Clone)]
pub struct GrantStoreHandle {
    tree: WorkspaceTreeHandle,
}

impl GrantStoreHandle {
    /// Construct a grant store over the shared workspace-tree handle.
    /// In production the same handle is passed to `ChromeState` and
    /// `WorkspaceTreeReplica` so every subsystem mutates the same tree.
    pub fn new(tree: WorkspaceTreeHandle) -> Self {
        Self { tree }
    }

    /// Build a single-workspace store for unit tests / examples.
    pub fn test_with_workspace(ws: Workspace) -> (Self, WorkspaceId) {
        let ws_id = ws.id;
        let mut tree = WorkspaceTree::default();
        tree.active = ws_id;
        tree.workspaces.push(ws);
        (Self::new(tree.shared()), ws_id)
    }

    /// Lookup the strongest existing grant (`Always` > `Session` > `Once`)
    /// of `kind` for `caller`. `None` means "no grant" → the portal
    /// will enqueue a prompt.
    ///
    /// Acquires a read lock for the duration of the lookup. Returns
    /// `None` if the workspace doesn't exist (drained workspace, racy
    /// portal request) — defaults safely to deny.
    pub fn lookup(&self, caller: &PortalCaller, kind: CapabilityKind) -> Option<GrantScope> {
        let tree = self.tree.read();
        let ws = tree
            .workspaces
            .iter()
            .find(|w| w.id == caller.workspace_id)?;
        let mut best: Option<GrantScope> = None;
        for cap in ws.app_capabilities(&caller.app_id) {
            let Some(desc) = Capability::from_capability(cap) else {
                continue;
            };
            if !kind.matches(&desc) {
                continue;
            }
            best = Some(match best {
                Some(prev) => stronger_scope(prev, cap.expiry),
                None => cap.expiry,
            });
        }
        best
    }

    /// The authoritative "may this app do this?" check.
    ///
    /// Unlike [`Self::lookup`], which answers "is there a grant of this
    /// shape" for the prompt logic, this asks the capability model
    /// directly about a concrete resource — so a filesystem grant on a
    /// directory correctly authorises a file inside it, and a network
    /// grant on one host does not authorise another.
    ///
    /// Fail-closed on every unknown: a missing workspace, an unknown
    /// app, an expired capability all deny.
    pub fn permits(&self, caller: &PortalCaller, resource: &ResourceRef, verb: Verb) -> bool {
        let tree = self.tree.read();
        let Some(ws) = tree.workspaces.iter().find(|w| w.id == caller.workspace_id) else {
            return false;
        };
        let actor = ws.actor_for_app(&caller.app_id);
        ws.allows(&actor, resource, verb)
    }

    /// Persist `capability` under `caller.app_id` in `caller.workspace_id`.
    /// Used after the user clicks `Session` or `Always` on the prompt.
    ///
    /// Acquires the write lock briefly, mutates both the cache map and
    /// the automerge doc (via `doc_insert_grant`), then releases the
    /// lock. Peer-sync picks up the change on the next `export_changes`
    /// tick.
    pub fn insert(&self, caller: &PortalCaller, capability: Capability) -> bool {
        let mut tree = self.tree.write();
        let Some(ws) = tree
            .workspaces
            .iter_mut()
            .find(|w| w.id == caller.workspace_id)
        else {
            return false;
        };
        let ids = ws.grant_app(&caller.app_id, &capability);
        // Mirror into the automerge doc so peer-sync ships the grant on
        // the next tick, and so a restart reloads it. Tolerant of the
        // doc not being seeded yet (e.g. a brand-new workspace not yet
        // flushed through `seed_doc_from_cache`).
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

    /// True if `caller` already holds a filesystem grant whose path
    /// covers `path` — an exact match or an ancestor directory. The
    /// file-drop router calls this before recording a drop grant so a
    /// second drop of the same file (or a file already inside a granted
    /// directory) doesn't append a duplicate `Capability::Filesystem`.
    ///
    /// Acquires a read lock for the lookup; returns `false` if the
    /// workspace or the app has no grants (default-deny, so the caller
    /// records the grant).
    pub fn filesystem_path_granted(&self, caller: &PortalCaller, path: &std::path::Path) -> bool {
        let tree = self.tree.read();
        let Some(ws) = tree.workspaces.iter().find(|w| w.id == caller.workspace_id) else {
            return false;
        };
        // Asks the capability model rather than re-deriving prefix
        // logic: `Scope::FsPath` already covers its subtree, on
        // component boundaries, and the same rule is what the
        // filesystem itself enforces.
        let actor = ws.actor_for_app(&caller.app_id);
        ws.allows(
            &actor,
            &ResourceRef::FsPath(path.to_string_lossy().into_owned()),
            Verb::Read,
        )
    }

    /// Direct tree-handle accessor for tests that need to assert on
    /// multiple fields at once, or that need to construct a peer
    /// subsystem holding the same handle. Production callers go through
    /// [`Self::lookup`] / [`Self::insert`].
    pub fn tree_handle(&self) -> WorkspaceTreeHandle {
        self.tree.clone()
    }

    /// Back-compat alias for [`Self::tree_handle`]. A handful of older
    /// tests/portals still reference `.tree()`; both now return the
    /// shared `WorkspaceTreeHandle`.
    // NOT cfg-gated: reachable from the shots harness, which is a release build.
    pub fn tree(&self) -> WorkspaceTreeHandle {
        self.tree.clone()
    }
}

/// The longer-lived of two expiries.
///
/// Delegates to [`Expiry::longest`] rather than re-deriving the order:
/// the capability store merges a re-grant with the same rule, and two
/// implementations of "strongest wins" would eventually disagree about
/// whether a permanent grant is permanent.
fn stronger_scope(a: GrantScope, b: GrantScope) -> GrantScope {
    Expiry::longest(a, b)
}

/// The user-prompt the portal needs to surface, waiting on a choice.
pub struct PendingGrant {
    pub caller: PortalCaller,
    pub capability: Capability,
    /// One-shot reply channel. The shell's permission-prompt pump
    /// fulfils this when the user clicks Allow/Session/Always/Deny;
    /// test mode fulfils it synchronously from [`PromptResolver`].
    pub reply: oneshot::Sender<PromptOutcome>,
}

/// Outcome of a permission prompt — mirrors the widget's
/// [`ui::widget::PromptChoice`] but keeps the portal module
/// dependency-free from `quick_ui` for the lookup path.
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
/// [`PromptResolver::Channel`] — the request is buffered and the shell's
/// permission-prompt pump drains it on the main thread. Tests use
/// [`PromptResolver::AutoAllow`] / `AutoDeny` so the round-trip
/// completes without any GUI thread.
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

/// Handle the portal interfaces hold to push new prompts onto the
/// queue. Cloneable so each interface impl gets its own clone.
#[derive(Clone)]
pub struct PromptQueueHandle {
    resolver: PromptResolver,
    pending: Arc<Mutex<Vec<PendingGrant>>>,
}

impl PromptQueueHandle {
    pub fn new(resolver: PromptResolver) -> Self {
        Self {
            resolver,
            pending: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Enqueue a new request and return the reply receiver. The caller
    /// `.await`s the receiver; the prompt path / auto-resolver fulfils
    /// it.
    pub fn enqueue(
        &self,
        caller: PortalCaller,
        capability: Capability,
    ) -> oneshot::Receiver<PromptOutcome> {
        let (tx, rx) = oneshot::channel();
        let pending = PendingGrant {
            caller,
            capability,
            reply: tx,
        };
        match self.resolver {
            PromptResolver::Channel => {
                if let Ok(mut q) = self.pending.lock() {
                    q.push(pending);
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

    /// Drain pending prompts for the shell's per-frame pump to convert
    /// into `ChromeState::pending_grants` entries. Each drained entry
    /// still owns its reply channel — the caller is responsible for
    /// fulfilling it.
    pub fn drain(&self) -> Vec<PendingGrant> {
        match self.pending.lock() {
            Ok(mut q) => std::mem::take(&mut *q),
            Err(_) => Vec::new(),
        }
    }

    /// Number of queued prompts — for tests that need to assert the
    /// queue length before the pump drains it.
    pub fn queued_len(&self) -> usize {
        self.pending.lock().map(|q| q.len()).unwrap_or(0)
    }

    /// Direct accessor (test only): peek at the front entry's caller +
    /// capability without removing it.
    #[cfg(any(test, debug_assertions))]
    pub fn front_for_tests(&self) -> Option<(PortalCaller, Capability)> {
        let q = self.pending.lock().ok()?;
        q.first().map(|p| (p.caller.clone(), p.capability.clone()))
    }
}

/// Configuration handed to [`serve`] / interface constructors.
#[derive(Clone)]
pub struct PortalContext {
    pub grants: GrantStoreHandle,
    pub prompts: PromptQueueHandle,
    /// Map from a D-Bus connection's peer-credentials (unique-name) to a
    /// `PortalCaller`. v1 keeps this as a simple in-memory map keyed by
    /// the connection's UniqueName — production fills it in when the
    /// per-workspace bridge spawns an app; tests poke entries directly.
    pub caller_map: CallerMap,
    /// File-picker spawn handle. The FileChooser backend calls
    /// [`FilePickerSpawner::spawn`] on `Allow` to surface a real dialog;
    /// tests pass a [`MockFilePickerSpawner`] that returns canned paths
    /// without rendering anything. Defaults to a
    /// [`NullFilePickerSpawner`] in [`PortalContext::null_spawners`] so
    /// existing tests that don't care about the dialog keep their
    /// fast-path behaviour (the null spawner cancels every request,
    /// which surfaces as `response=1`).
    pub file_picker: FilePickerSpawnerHandle,
    /// ScreenCast Pipewire stream provisioner. The ScreenCast backend
    /// calls [`ScreenCastStreamProvisioner::provision_stream`] on
    /// `Start` Allow to create a Pipewire node and return its id +
    /// props. Production wires the real Pipewire connection; tests use
    /// [`MockScreenCastProvisioner`] which returns a synthetic node id
    /// without touching the system Pipewire daemon.
    pub screencast_provisioner: ScreenCastStreamProvisionerHandle,
    /// RemoteDesktop seat-injection sink. The RemoteDesktop backend hands
    /// each granted `Notify*` action here; production wires a
    /// [`ChannelInjector`] the compositor loop drains into the seat, and
    /// `--portal-server-only` mode (no live seat) uses the null handle.
    pub remote_input_injector: RemoteInputInjectorHandle,
    /// How a picked path is made reachable by the app that asked for
    /// it. See [`doc_store`] for why recording the grant is not enough
    /// on its own for an app that is already running.
    pub documents: DocumentStoreHandle,
    /// Print submission sink. The Print backend hands each consented
    /// job here; production runs `lp`, tests inject a
    /// [`RecordingSpooler`] and assert on what actually arrived.
    ///
    /// This is the only component in the print path that touches CUPS,
    /// and it lives on *this* side of the sandbox — which is the whole
    /// point of the interface.
    pub print_spooler: PrintSpoolerHandle,
}

impl PortalContext {
    /// Build a context wired with null spawners — the FileChooser
    /// dialog cancels every request, the ScreenCast provisioner
    /// returns a synthetic node-id with empty props. Used by tests
    /// that pre-date the dialog/provisioner abstractions and only
    /// care about the response-code path. Production callers always
    /// inject real spawners via [`PortalContext`] field initialisers.
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
                MockScreenCastProvisioner::new_with_node(1, "default"),
            )),
            remote_input_injector: RemoteInputInjectorHandle::null(),
            documents: DocumentStoreHandle::host_paths(),
            // The real spooler, not a null one. Unlike the picker and
            // the provisioner it needs no chrome, no GPU and no live
            // seat — it is a process invocation — so there is nothing
            // for a headless context to be missing. On a machine with
            // no CUPS it reports no destinations and refuses
            // submissions, which is the honest answer rather than a
            // fabricated printer.
            print_spooler: PrintSpoolerHandle::cups(),
        }
    }
}

/// In-memory lookup from a D-Bus unique-name → `PortalCaller`. Wrapped
/// behind an `Arc<Mutex<…>>` so it's cheap to clone across interface
/// impls + the connection-acceptance path can push entries from a
/// different thread than the D-Bus reader. Production-time entries are
/// added by the per-workspace bridge accept path; tests poke entries
/// directly.
#[derive(Clone, Default)]
pub struct CallerMap {
    inner: Arc<Mutex<HashMap<String, PortalCaller>>>,
    /// Test-only forced override. When `Some`, every `lookup` call
    /// returns this caller regardless of the key — useful in
    /// integration tests where the underlying transport (p2p socket
    /// pair) doesn't surface a unique-name on the request header.
    /// Production never sets this; the bridge accept path always
    /// keys by the real D-Bus unique-name.
    forced: Arc<Mutex<Option<PortalCaller>>>,
}

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
        if let Ok(forced) = self.forced.lock() {
            if let Some(c) = forced.as_ref() {
                return Some(c.clone());
            }
        }
        let m = self.inner.lock().ok()?;
        m.get(dbus_unique_name).cloned()
    }

    /// Test-only knob: every subsequent `lookup` call (regardless of
    /// key, even when the sender is `None`) returns `caller`. Mirrors
    /// the bridge-tag override the production path will eventually
    /// use; tests reach for this when the p2p transport doesn't
    /// surface a unique-name on the message header.
    pub fn force_caller_for_tests(&self, caller: PortalCaller) {
        if let Ok(mut f) = self.forced.lock() {
            *f = Some(caller);
        }
    }
}

/// Shared sub-flow every interface uses: lookup or prompt, return the
/// outcome. Stays in this module (rather than each interface file) so
/// the "Always-grant skips the prompt" branch is asserted by one test
/// that all three interfaces share.
pub mod flow {
    use super::*;

    pub async fn resolve(
        ctx: &PortalContext,
        caller: PortalCaller,
        kind: CapabilityKind,
        cap_template: Capability,
    ) -> PromptOutcome {
        // 1. Fast path — `Always`-scoped grant already present.
        if matches!(ctx.grants.lookup(&caller, kind), Some(GRANT_ALWAYS)) {
            return PromptOutcome::AllowAlways;
        }
        // 2. Slow path — enqueue + await user choice.
        let rx = ctx.prompts.enqueue(caller.clone(), cap_template.clone());
        match rx.await {
            Ok(outcome) => {
                // Persist Session/Always — Once is treated as one-shot.
                if let Some(scope) = outcome.to_scope() {
                    if !matches!(scope, GRANT_ONCE) {
                        let cap = super::with_scope(cap_template, scope);
                        ctx.grants.insert(&caller, cap);
                    }
                }
                outcome
            }
            // Channel closed without a reply == treat as deny. Defensive
            // posture: if the prompt pump shut down mid-flight we'd
            // rather deny than hang the caller forever.
            Err(_) => PromptOutcome::Deny,
        }
    }
}

/// Helper: rewrite a [`Capability`] template's `scope` field. Mirrors
/// `overlay_host::with_grant_scope` — duplicated here so the portal
/// module doesn't take a hard dep on `shell::overlay_host` (which pulls
/// the whole UI stack).
pub fn with_scope(cap: Capability, scope: GrantScope) -> Capability {
    match cap {
        Capability::Filesystem { path, mode, .. } => Capability::Filesystem { path, mode, scope },
        Capability::Camera { .. } => Capability::Camera { scope },
        Capability::Microphone { .. } => Capability::Microphone { scope },
        Capability::Screencast { .. } => Capability::Screencast { scope },
        Capability::RemoteDesktop { .. } => Capability::RemoteDesktop { scope },
        Capability::Print { .. } => Capability::Print { scope },
        Capability::Network { hosts, .. } => Capability::Network { hosts, scope },
        Capability::Privileged { capability, .. } => Capability::Privileged { capability, scope },
    }
}

/// The document store this session should use.
///
/// One place to make the choice, so the compositor and any future
/// caller cannot disagree about which mechanism is live. Today that is
/// the host-path store; see [`doc_store`] for what it does and does not
/// reach, and for the design of the FUSE-backed store that replaces it
/// for confined callers.
pub fn documents_for_session() -> DocumentStoreHandle {
    DocumentStoreHandle::host_paths()
}

/// Bus name we register under. xdg-desktop-portal expects backends to
/// own `org.freedesktop.impl.portal.desktop.<vendor>`; the vendor suffix
/// is matched against the `portals/<vendor>.portal` config file (see
/// `os/usr/share/xdg-desktop-portal/portals/qos.portal`, its D-Bus
/// activation `.service`, and `shell-portal.service` — all of which name
/// `...desktop.qos`).
pub const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.qos";

/// Object path the portal's frontend dispatches to on each interface.
/// xdg-desktop-portal hard-codes `/org/freedesktop/portal/desktop`.
pub const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";

/// Spawn the portal server on `connection`, registering all three
/// interfaces under [`OBJECT_PATH`] and requesting [`BUS_NAME`].
///
/// Returns the connection back to the caller (kept alive by the
/// caller) — the connection's task pool drives method dispatch.
/// Dropping the returned [`ServerHandle`] releases the bus name and
/// drops the connection.
///
/// **Tests** pass an in-process p2p connection so no real session bus
/// is touched. **Production** passes a session-bus connection from
/// `zbus::Connection::session()`.
pub async fn serve(
    connection: zbus::Connection,
    ctx: PortalContext,
) -> Result<ServerHandle, zbus::Error> {
    use camera::AccessBackend;
    use file_chooser::FileChooserBackend;
    use print::PrintBackend;
    use remote_desktop::RemoteDesktopBackend;
    use screencast::ScreenCastBackend;

    let fc = FileChooserBackend::new(ctx.clone());
    let sc = ScreenCastBackend::new(ctx.clone());
    let ac = AccessBackend::new(ctx.clone());
    let rd = RemoteDesktopBackend::new(ctx.clone());
    let pr = PrintBackend::new(ctx.clone());

    connection
        .object_server()
        .at(OBJECT_PATH, fc)
        .await?;
    connection
        .object_server()
        .at(OBJECT_PATH, sc)
        .await?;
    connection
        .object_server()
        .at(OBJECT_PATH, ac)
        .await?;
    connection
        .object_server()
        .at(OBJECT_PATH, rd)
        .await?;
    connection
        .object_server()
        .at(OBJECT_PATH, pr)
        .await?;

    // `request_name` only makes sense on a real bus — p2p test
    // connections have no broker, and failing there would mean every
    // hermetic test needed one. So the result is *recorded* rather than
    // swallowed: `ServerHandle::owns_name` is what the caller checks.
    //
    // This used to be a `tracing::debug!` and nothing else, which is the
    // worst shape available: on a real bus, losing the name to another
    // claimant left a portal that answered nothing, logged at a level
    // nobody runs, and looked exactly like a portal that was working.
    let owns_name = match connection.request_name(BUS_NAME).await {
        Ok(_) => true,
        Err(e) => {
            tracing::debug!("portal: request_name did not succeed: {e}");
            false
        }
    };

    Ok(ServerHandle {
        connection,
        owns_name,
    })
}

/// RAII handle keeping the portal D-Bus connection alive. Drop releases
/// the bus name and tears down the object server.
pub struct ServerHandle {
    // Never read on purpose: holding the `Connection` IS the job. Drop it
    // and zbus releases `org.freedesktop.impl.portal.desktop.nirios` and
    // tears the object server down, so the portal stops answering.
    #[allow(dead_code)]
    connection: zbus::Connection,
    owns_name: bool,
}

impl ServerHandle {
    /// Did this connection actually acquire [`BUS_NAME`]?
    ///
    /// `false` means the object server is up but nothing on the session
    /// bus can route to it — either there is no broker (a p2p test
    /// connection) or another process already holds the name. On a real
    /// session the second case is the one that matters: the portal is
    /// running and answering nobody.
    pub fn owns_name(&self) -> bool {
        self.owns_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn empty_ws(id: u64) -> Workspace {
        Workspace::test_default(WorkspaceId(id), "t")
    }

    #[test]
    fn capability_kind_matches_filesystem_modes() {
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

    #[test]
    fn grant_store_returns_strongest_scope() {
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

    #[test]
    fn filesystem_path_granted_covers_exact_and_ancestor_paths() {
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
            store.filesystem_path_granted(&caller, std::path::Path::new("/home/u/proj/main.ts"))
        );
        // A sibling path outside the granted subtree is NOT covered.
        assert!(!store.filesystem_path_granted(&caller, std::path::Path::new("/home/u/other.ts")));
        // A different app id has no grants → not covered.
        let other = PortalCaller::new(ws_id, "Files");
        assert!(
            !store.filesystem_path_granted(&other, std::path::Path::new("/home/u/proj/main.ts"))
        );
    }

    #[test]
    fn grant_store_returns_none_for_missing_app() {
        let ws = empty_ws(12);
        let (store, ws_id) = GrantStoreHandle::test_with_workspace(ws);
        let caller = PortalCaller::new(ws_id, "no.such.app");
        assert!(store.lookup(&caller, CapabilityKind::Camera).is_none());
    }

    #[test]
    fn grant_store_insert_appends_into_workspace() {
        let ws = empty_ws(13);
        let (store, ws_id) = GrantStoreHandle::test_with_workspace(ws);
        let caller = PortalCaller::new(ws_id, "app.org");
        assert!(store.insert(
            &caller,
            Capability::Microphone {
                scope: GRANT_ALWAYS
            }
        ));
        assert_eq!(
            store.lookup(&caller, CapabilityKind::Microphone),
            Some(GRANT_ALWAYS)
        );
    }

    /// C-3 Phase C integration test.
    ///
    /// A grant inserted via the portal's [`GrantStoreHandle`] must be
    /// immediately visible to the shell chrome (the WS-Settings
    /// "Permissions" tab reads from `ChromeState`'s shared
    /// `WorkspaceTreeHandle`). Before this refactor each subsystem held
    /// an independent `WorkspaceTree` clone and the grant never reached
    /// the shell — closes the first observable consequence in
    /// `docs/superpowers/specs/2026-05-22-workspace-tree-arc-mutex-design.md`.
    #[test]
    fn grant_inserted_via_portal_is_visible_in_ws_settings() {
        use crate::shell::chrome_state::ChromeState;
        use crate::shell::state::WorkspaceTreeHandleExt;

        // Build the shared handle once and seed a workspace so the
        // portal has somewhere to land grants.
        let handle: WorkspaceTreeHandle = WorkspaceTreeHandle::test_default();
        let ws_id = WorkspaceId(1);
        {
            let mut tree = handle.write();
            tree.active = ws_id;
            tree.workspaces.push(empty_ws(1));
        }

        // Wire portal + chrome to the *same* handle.
        let portal = GrantStoreHandle::new(handle.clone());
        let chrome = ChromeState::new_with_handle(handle.clone());

        // Portal-side insert: simulate an `Allow → Session` outcome
        // landing on the camera capability for `camera-app`.
        let caller = PortalCaller::new(ws_id, "camera-app");
        let cap = Capability::Camera {
            scope: GRANT_SESSION,
        };
        assert!(
            portal.insert(&caller, cap),
            "portal insert must succeed on a workspace that exists",
        );

        // Shell-side read: walking the chrome's tree must surface the
        // grant under `camera-app`. This is the production code path
        // the Permissions tab uses (see
        // `shell::workspace_settings::permissions_pane_rows`).
        let st = chrome.borrow();
        let tree = st.tree();
        let ws = tree.active_workspace().expect("active workspace exists");
        assert!(
            ws.app_grants_snapshot().contains_key("camera-app"),
            "portal-side grant insert must be visible to the shell chrome \
             (audit C-3: shared WorkspaceTreeHandle ties subsystems together)",
        );
    }

    #[test]
    fn prompt_queue_auto_allow_resolves_immediately() {
        let q = PromptQueueHandle::new(PromptResolver::AutoAllow);
        let caller = PortalCaller::new(WorkspaceId(1), "app");
        let rx = q.enqueue(caller, Capability::Screencast { scope: GRANT_ONCE });
        // No `.await` needed — AutoAllow fills the oneshot synchronously
        // *before* `enqueue` returns, so the receiver is already ready.
        let outcome = rx.blocking_recv().expect("oneshot");
        assert_eq!(outcome, PromptOutcome::AllowOnce);
        // Nothing should have been buffered.
        assert_eq!(q.queued_len(), 0);
    }

    #[test]
    fn prompt_queue_channel_buffers_for_drain() {
        let q = PromptQueueHandle::new(PromptResolver::Channel);
        let caller = PortalCaller::new(WorkspaceId(2), "app");
        let _rx = q.enqueue(caller.clone(), Capability::Camera { scope: GRANT_ONCE });
        assert_eq!(q.queued_len(), 1);
        let drained = q.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].caller, caller);
        assert_eq!(q.queued_len(), 0);
    }

    #[test]
    fn flow_resolve_fast_path_always_grant_skips_prompt() {
        // Build a store with an Always-scoped Camera grant and a
        // Channel-mode prompt queue. The "Channel" mode would buffer
        // any enqueued request — by asserting the queue stays empty,
        // we prove the fast-path bypassed it.
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
        let ctx = PortalContext::new_with_null_spawners(store, queue.clone(), CallerMap::new());

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

    #[test]
    fn flow_resolve_session_outcome_persists_grant() {
        let ws = empty_ws(22);
        let (store, ws_id) = GrantStoreHandle::test_with_workspace(ws);
        let queue =
            PromptQueueHandle::new(PromptResolver::AutoOutcome(PromptOutcome::AllowSession));
        let caller = PortalCaller::new(ws_id, "app");
        let ctx = PortalContext::new_with_null_spawners(store.clone(), queue, CallerMap::new());
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
    }

    #[test]
    fn flow_resolve_once_outcome_does_not_persist() {
        let ws = empty_ws(23);
        let (store, ws_id) = GrantStoreHandle::test_with_workspace(ws);
        let queue = PromptQueueHandle::new(PromptResolver::AutoAllow);
        let caller = PortalCaller::new(ws_id, "app");
        let ctx = PortalContext::new_with_null_spawners(store.clone(), queue, CallerMap::new());
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

    #[test]
    fn flow_resolve_deny_outcome_is_propagated() {
        let ws = empty_ws(24);
        let (store, ws_id) = GrantStoreHandle::test_with_workspace(ws);
        let queue = PromptQueueHandle::new(PromptResolver::AutoDeny);
        let caller = PortalCaller::new(ws_id, "app");
        let ctx = PortalContext::new_with_null_spawners(store.clone(), queue, CallerMap::new());
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

    #[test]
    fn caller_map_round_trips() {
        let m = CallerMap::new();
        let caller = PortalCaller::new(WorkspaceId(7), "app.x");
        m.insert(":1.42", caller.clone());
        assert_eq!(m.lookup(":1.42"), Some(caller));
        assert!(m.lookup(":1.43").is_none());
    }
}
