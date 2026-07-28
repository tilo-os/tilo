//! `org.freedesktop.impl.portal.FileChooser` backend.
//!
//! Spec: `/usr/share/dbus-1/interfaces/org.freedesktop.impl.portal.FileChooser.xml`.
//! Plan: Sandbox Phase 4.3, lines 335-353.
//!
//! Three methods:
//!
//! - `OpenFile(handle, app_id, parent_window, title, options) → (response, results)`
//! - `SaveFile(...)` — same shape
//! - `SaveFiles(...)` — same shape
//!
//! `response` is the portal response code (0 = success, 1 = user cancelled,
//! 2 = other error). `results` is `a{sv}`; for FileChooser the `uris` key
//! is mandatory on success.
//!
//! Every method:
//! 1. Resolves the caller's [`PortalCaller`] via the [`CallerMap`] using
//!    the D-Bus connection's unique-name.
//! 2. Delegates to [`flow::resolve`] with `CapabilityKind::FilesystemRead`
//!    (or `FilesystemReadWrite` for `SaveFile`/`SaveFiles`).
//! 3. On `Allow*`, calls into [`FilePickerSpawner::spawn`] to surface a
//!    real file-picker dialog. The dialog reply (`Option<Vec<PathBuf>>`)
//!    is converted into a `uris` array (`file://...`) for the response.
//!    `None` (user closed the dialog without picking) is treated as
//!    `response=1` (user cancelled).
//! 4. On `Deny`, returns response=1 + an empty results dict.

use std::collections::HashMap;
use std::path::PathBuf;

use zbus::interface;
use zbus::object_server::SignalContext;
use zbus::names::OwnedUniqueName;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::shell::state::{Capability, FsMode, GrantScope};

use super::{
    flow, CapabilityKind, PortalCaller, PortalContext, PromptOutcome, WorkspaceId,
};

/// D-Bus interface impl. One instance per [`super::serve`] call; held
/// by the zbus `ObjectServer` for the lifetime of the connection.
pub struct FileChooserBackend {
    ctx: PortalContext,
}

impl FileChooserBackend {
    pub fn new(ctx: PortalContext) -> Self {
        Self { ctx }
    }

    /// Resolve the caller to a [`PortalCaller`]. Falls back to a stub
    /// caller when no connection peer-credentials are available (e.g.
    /// in-process p2p test bus) — production wires the bridge accept
    /// path's PID lookup into [`CallerMap`].
    ///
    /// Note that [`CallerMap::lookup`] also honours the
    /// `force_caller_for_tests` override regardless of `sender` —
    /// `None` sender + forced caller still resolves correctly. We
    /// therefore always consult the map (with an empty-string key on
    /// the missing-sender branch) before falling back to the synth.
    pub(super) fn resolve_caller(
        ctx: &PortalContext,
        sender: Option<&OwnedUniqueName>,
        app_id: &str,
    ) -> PortalCaller {
        let key = sender.map(|n| n.as_str().to_string()).unwrap_or_default();
        if let Some(c) = ctx.caller_map.lookup(&key) {
            return c;
        }
        // Fallback: synthesise from the app-id alone, with a zero
        // workspace. Tests poke a real entry into the caller map ahead
        // of the call; production reaches the production-only bridge
        // accept path, which populates the map before any portal request
        // can be issued.
        PortalCaller::new(WorkspaceId(0), app_id)
    }
}

#[interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl FileChooserBackend {
    /// `OpenFile` — request one or more existing files.
    async fn open_file(
        &self,
        #[zbus(signal_context)] _ctx: SignalContext<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        _handle: ObjectPath<'_>,
        app_id: String,
        _parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        tracing::debug!(
            "FileChooser.OpenFile: app={app_id} title={title} opts={}",
            options.len()
        );
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let caller = Self::resolve_caller(&self.ctx, sender.as_ref(), &app_id);
        let cap_template = Capability::Filesystem {
            // The portal protocol doesn't surface a target path here —
            // the dialog selects one. For the grant model we tag the
            // "any path" workspace-root sentinel so the grant store
            // knows this is a filesystem capability.
            path: PathBuf::from("/"),
            mode: FsMode::Read,
            scope: GrantScope::Once,
        };
        let outcome = flow::resolve(
            &self.ctx,
            caller.clone(),
            CapabilityKind::FilesystemRead,
            cap_template,
        )
        .await;
        self.open_response_via_picker(outcome, caller, title, /*save=*/ false)
            .await
    }

    /// `SaveFile` — request a destination for one new file.
    async fn save_file(
        &self,
        #[zbus(signal_context)] _ctx: SignalContext<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        _handle: ObjectPath<'_>,
        app_id: String,
        _parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        tracing::debug!(
            "FileChooser.SaveFile: app={app_id} title={title} opts={}",
            options.len()
        );
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let caller = Self::resolve_caller(&self.ctx, sender.as_ref(), &app_id);
        let cap_template = Capability::Filesystem {
            path: PathBuf::from("/"),
            mode: FsMode::ReadWrite,
            scope: GrantScope::Once,
        };
        let outcome = flow::resolve(
            &self.ctx,
            caller.clone(),
            CapabilityKind::FilesystemReadWrite,
            cap_template,
        )
        .await;
        self.open_response_via_picker(outcome, caller, title, /*save=*/ true)
            .await
    }

    /// `SaveFiles` — request a folder + multiple destination filenames.
    async fn save_files(
        &self,
        #[zbus(signal_context)] _ctx: SignalContext<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        _handle: ObjectPath<'_>,
        app_id: String,
        _parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        tracing::debug!(
            "FileChooser.SaveFiles: app={app_id} title={title} opts={}",
            options.len()
        );
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let caller = Self::resolve_caller(&self.ctx, sender.as_ref(), &app_id);
        let cap_template = Capability::Filesystem {
            path: PathBuf::from("/"),
            mode: FsMode::ReadWrite,
            scope: GrantScope::Once,
        };
        let outcome = flow::resolve(
            &self.ctx,
            caller.clone(),
            CapabilityKind::FilesystemReadWrite,
            cap_template,
        )
        .await;
        self.open_response_via_picker(outcome, caller, title, /*save=*/ true)
            .await
    }
}

impl FileChooserBackend {
    /// Bridge from a `PromptOutcome` to the FileChooser response.
    /// On Allow*, spawns a real file-picker dialog via the
    /// `FilePickerSpawner` handle and awaits the user's selection.
    /// On Deny, returns response=1 + empty results.
    pub(super) async fn open_response_via_picker(
        &self,
        outcome: PromptOutcome,
        caller: PortalCaller,
        title: String,
        save: bool,
    ) -> (u32, HashMap<String, OwnedValue>) {
        if !outcome.is_allowed() {
            return (1, HashMap::new());
        }
        let rx = self
            .ctx
            .file_picker
            .request(caller, title, save);
        let picked = match rx.await {
            Ok(p) => p,
            Err(_) => {
                // Spawner dropped the reply — treat as cancel. Defensive
                // posture mirrors `flow::resolve`'s
                // "channel-closed-without-reply ≡ deny" branch.
                None
            }
        };
        paths_to_open_response(picked, save)
    }
}

/// Translate a real user-selected path list into the
/// `(response_code, results)` pair the FileChooser frontend expects.
///
/// `response_code` matches the freedesktop.org Request convention:
///   0 = OK (with results), 1 = user cancelled, 2 = other failure.
///
/// `None` (the dialog was dismissed / closed without picking) is
/// treated as cancel — response=1.
pub(super) fn paths_to_open_response(
    paths: Option<Vec<PathBuf>>,
    save: bool,
) -> (u32, HashMap<String, OwnedValue>) {
    let mut results: HashMap<String, OwnedValue> = HashMap::new();
    let Some(paths) = paths else {
        return (1, results);
    };
    if paths.is_empty() {
        return (1, results);
    }
    let uris: Vec<String> = paths
        .iter()
        .map(|p| {
            // Naive `file://` formatter — good enough for ASCII paths
            // and matches the path representation our compositor uses
            // internally. Spec-compliant percent-encoding for binary
            // path bytes lands when we plumb in `url::form_urlencoded`
            // (deferred — most desktop apps only get ASCII paths).
            format!("file://{}", p.display())
        })
        .collect();
    let uris_value = Value::new(uris);
    if let Ok(owned) = OwnedValue::try_from(uris_value) {
        results.insert("uris".to_string(), owned);
    }
    if save {
        if let Ok(w) = OwnedValue::try_from(Value::Bool(true)) {
            results.insert("writable".to_string(), w);
        }
    }
    (0, results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portal::{
        CallerMap, FilePickerSpawnerHandle, GrantStoreHandle, MockFilePickerSpawner,
        PromptQueueHandle, PromptResolver, ScreenCastStreamProvisionerHandle,
        MockScreenCastProvisioner,
    };
    use crate::shell::state::Workspace;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn empty_ws(id: u64) -> Workspace {
        Workspace {
            id: WorkspaceId(id),
            name: "fc".into(),
            strips: vec![],
            roles: vec![],
            workspace_files: vec![],
            collaborators: vec![],
            grants: HashMap::new(),
            shared_surfaces: HashMap::new(),
        }
    }

    fn null_spawners() -> (FilePickerSpawnerHandle, ScreenCastStreamProvisionerHandle) {
        (
            FilePickerSpawnerHandle::new(Arc::new(super::super::NullFilePickerSpawner::cancel())),
            ScreenCastStreamProvisionerHandle::new(Arc::new(
                MockScreenCastProvisioner::new_with_node(1, "t"),
            )),
        )
    }

    #[test]
    fn paths_allow_returns_response_zero_and_uri() {
        let (code, results) = paths_to_open_response(
            Some(vec![PathBuf::from("/tmp/x.txt")]),
            /*save=*/ false,
        );
        assert_eq!(code, 0);
        assert!(results.contains_key("uris"));
        assert!(!results.contains_key("writable"));
    }

    #[test]
    fn paths_save_returns_writable_true() {
        let (code, results) = paths_to_open_response(
            Some(vec![PathBuf::from("/tmp/out.bin")]),
            /*save=*/ true,
        );
        assert_eq!(code, 0);
        assert!(results.contains_key("uris"));
        assert!(results.contains_key("writable"));
    }

    #[test]
    fn paths_none_returns_response_one_no_results() {
        let (code, results) = paths_to_open_response(None, /*save=*/ false);
        assert_eq!(code, 1);
        assert!(results.is_empty());
    }

    #[test]
    fn paths_empty_returns_response_one() {
        // Empty list ≡ cancel — keeps the contract symmetric with
        // `None` (the spawner uses an empty Vec ≡ cancel internally).
        let (code, results) = paths_to_open_response(Some(vec![]), /*save=*/ false);
        assert_eq!(code, 1);
        assert!(results.is_empty());
    }

    #[test]
    fn paths_serialise_to_file_uri() {
        let (_code, results) =
            paths_to_open_response(Some(vec![PathBuf::from("/tmp/a.txt")]), false);
        let uris_v = results.get("uris").expect("uris");
        let cloned: OwnedValue = uris_v.try_clone().expect("clone OwnedValue");
        let uris: Vec<String> = Vec::<String>::try_from(cloned).expect("Vec<String>");
        assert_eq!(uris, vec!["file:///tmp/a.txt".to_string()]);
    }

    #[test]
    fn resolve_caller_falls_back_to_app_id_when_no_map_entry() {
        let (store, _ws_id) = GrantStoreHandle::test_with_workspace(empty_ws(31));
        let queue = PromptQueueHandle::new(PromptResolver::AutoAllow);
        let (fp, sc) = null_spawners();
        let ctx = PortalContext {
            grants: store,
            prompts: queue,
            caller_map: CallerMap::new(),
            file_picker: fp,
            screencast_provisioner: sc,
            remote_input_injector: crate::portal::RemoteInputInjectorHandle::null(),
        };
        let caller = FileChooserBackend::resolve_caller(&ctx, None, "org.example");
        assert_eq!(caller.app_id, "org.example");
        // Zero workspace id flags "no bridge match" — production-side
        // grant lookup will return None and fall through to the prompt
        // path (which is the conservative posture).
        assert_eq!(caller.workspace_id, WorkspaceId(0));
    }

    #[test]
    fn resolve_caller_uses_map_when_unique_name_present() {
        let (store, ws_id) = GrantStoreHandle::test_with_workspace(empty_ws(32));
        let queue = PromptQueueHandle::new(PromptResolver::AutoAllow);
        let map = CallerMap::new();
        map.insert(":1.7", PortalCaller::new(ws_id, "org.real"));
        let (fp, sc) = null_spawners();
        let ctx = PortalContext {
            grants: store,
            prompts: queue,
            caller_map: map,
            file_picker: fp,
            screencast_provisioner: sc,
            remote_input_injector: crate::portal::RemoteInputInjectorHandle::null(),
        };
        let name: OwnedUniqueName = ":1.7".try_into().unwrap();
        let caller = FileChooserBackend::resolve_caller(&ctx, Some(&name), "ignored");
        assert_eq!(caller.app_id, "org.real");
        assert_eq!(caller.workspace_id, ws_id);
    }

    /// Required test from the Sandbox 4.3 follow-up brief —
    /// `open_file_returns_user_selected_uri`. Wires a
    /// `MockFilePickerSpawner` returning `/tmp/test.txt`, calls the
    /// backend's response builder, and asserts the FileChooser
    /// response contains `file:///tmp/test.txt`.
    #[test]
    fn open_file_returns_user_selected_uri() {
        let (store, ws_id) = GrantStoreHandle::test_with_workspace(empty_ws(33));
        let queue = PromptQueueHandle::new(PromptResolver::AutoAllow);
        let mock = Arc::new(MockFilePickerSpawner::returning(vec![PathBuf::from(
            "/tmp/test.txt",
        )]));
        let fp = FilePickerSpawnerHandle::new(mock.clone());
        let sc = ScreenCastStreamProvisionerHandle::new(Arc::new(
            MockScreenCastProvisioner::new_with_node(1, "t"),
        ));
        let ctx = PortalContext {
            grants: store,
            prompts: queue,
            caller_map: CallerMap::new(),
            file_picker: fp,
            screencast_provisioner: sc,
            remote_input_injector: crate::portal::RemoteInputInjectorHandle::null(),
        };
        let backend = FileChooserBackend::new(ctx);
        let caller = PortalCaller::new(ws_id, "org.example");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (code, results) = rt.block_on(backend.open_response_via_picker(
            PromptOutcome::AllowOnce,
            caller.clone(),
            "Open file".into(),
            /*save=*/ false,
        ));
        assert_eq!(code, 0);
        let uris_v = results.get("uris").expect("uris");
        let cloned: OwnedValue = uris_v.try_clone().expect("clone OwnedValue");
        let uris: Vec<String> = Vec::<String>::try_from(cloned).expect("Vec<String>");
        assert_eq!(uris, vec!["file:///tmp/test.txt".to_string()]);
        // The mock saw exactly one request — and the title + save flag
        // were forwarded correctly.
        let seen = mock.requests();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].1, "Open file");
        assert!(!seen[0].2);
    }

    #[test]
    fn open_file_with_cancelled_spawner_returns_response_one() {
        let (store, ws_id) = GrantStoreHandle::test_with_workspace(empty_ws(34));
        let queue = PromptQueueHandle::new(PromptResolver::AutoAllow);
        let fp = FilePickerSpawnerHandle::new(Arc::new(
            MockFilePickerSpawner::always_cancel(),
        ));
        let sc = ScreenCastStreamProvisionerHandle::new(Arc::new(
            MockScreenCastProvisioner::new_with_node(1, "t"),
        ));
        let ctx = PortalContext {
            grants: store,
            prompts: queue,
            caller_map: CallerMap::new(),
            file_picker: fp,
            screencast_provisioner: sc,
            remote_input_injector: crate::portal::RemoteInputInjectorHandle::null(),
        };
        let backend = FileChooserBackend::new(ctx);
        let caller = PortalCaller::new(ws_id, "org.example");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (code, results) = rt.block_on(backend.open_response_via_picker(
            PromptOutcome::AllowOnce,
            caller,
            "Open".into(),
            false,
        ));
        assert_eq!(code, 1);
        assert!(results.is_empty());
    }

    #[test]
    fn deny_outcome_skips_spawner_and_returns_one() {
        // Even with a paths-returning spawner, a Deny outcome must
        // short-circuit before the dialog spawns.
        let (store, ws_id) = GrantStoreHandle::test_with_workspace(empty_ws(35));
        let queue = PromptQueueHandle::new(PromptResolver::AutoDeny);
        let mock = Arc::new(MockFilePickerSpawner::returning(vec![PathBuf::from(
            "/tmp/never.txt",
        )]));
        let fp = FilePickerSpawnerHandle::new(mock.clone());
        let sc = ScreenCastStreamProvisionerHandle::new(Arc::new(
            MockScreenCastProvisioner::new_with_node(1, "t"),
        ));
        let ctx = PortalContext {
            grants: store,
            prompts: queue,
            caller_map: CallerMap::new(),
            file_picker: fp,
            screencast_provisioner: sc,
            remote_input_injector: crate::portal::RemoteInputInjectorHandle::null(),
        };
        let backend = FileChooserBackend::new(ctx);
        let caller = PortalCaller::new(ws_id, "org.example");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (code, _) = rt.block_on(backend.open_response_via_picker(
            PromptOutcome::Deny,
            caller,
            "Open".into(),
            false,
        ));
        assert_eq!(code, 1);
        // The spawner must not have been called at all — Deny
        // short-circuits before the dialog.
        assert!(mock.requests().is_empty());
    }
}
