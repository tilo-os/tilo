//! `org.freedesktop.impl.portal.FileChooser` backend.
//!
//! Spec: `/usr/share/dbus-1/interfaces/org.freedesktop.impl.portal.FileChooser.xml`.
//!
//! Three methods, same shape: `OpenFile`/`SaveFile`/`SaveFiles(handle,
//! app_id, parent_window, title, options) → (response, results)`.
//! `response` is the portal response code (0 = success, 1 = user
//! cancelled, 2 = other error); `results` is `a{sv}`, with `uris`
//! mandatory on success.
//!
//! Every method resolves the caller's [`PortalCaller`] via
//! [`CallerMap`], delegates to [`flow::resolve`] with
//! `CapabilityKind::FilesystemRead` (or `FilesystemReadWrite` for
//! `SaveFile`/`SaveFiles`), and on `Allow*` calls
//! [`FilePickerSpawner::spawn`] to surface a real dialog — the reply
//! (`Option<Vec<PathBuf>>`) becomes a `uris` array, with `None` treated
//! as `response=1` (cancelled). On `Deny`, returns response=1 with an empty results dict.

use std::collections::HashMap;
use std::path::PathBuf;

use zbus::interface;
use zbus::names::OwnedUniqueName;
use zbus::object_server::SignalContext;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::shell::state::{Capability, FsMode, GRANT_SESSION};

use super::{FilePickerOptions, PortalCaller, PortalContext, WorkspaceId};

/// D-Bus interface impl. One instance per [`super::serve`] call, held by the zbus `ObjectServer` for the connection's lifetime.
pub struct FileChooserBackend {
    ctx: PortalContext,
}

impl FileChooserBackend {
    pub fn new(ctx: PortalContext) -> Self {
        Self { ctx }
    }

    /// Resolve the caller to a [`PortalCaller`]. Falls back to a stub caller
    /// when no connection peer-credentials are available (e.g. in-process
    /// p2p test bus) — production wires the bridge accept path's PID lookup into [`CallerMap`].
    ///
    /// [`CallerMap::lookup`] honours `force_caller_for_tests` regardless of
    /// `sender`, so the map is always consulted (empty-string key on the missing-sender branch) before falling back to the synth.
    pub(super) fn resolve_caller(
        ctx: &PortalContext,
        sender: Option<&OwnedUniqueName>,
        app_id: &str,
    ) -> PortalCaller {
        let key = sender.map(|n| n.as_str().to_string()).unwrap_or_default();
        if let Some(c) = ctx.caller_map.lookup(&key) {
            return c;
        }
        // Fallback: synthesise from the app-id alone, with a zero workspace.
        // Tests poke a real entry into the caller map ahead of the call;
        // production reaches the bridge accept path, which populates the map before any portal request can be issued.
        PortalCaller::new(WorkspaceId(0), app_id)
    }
}

#[interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl FileChooserBackend {
    /// `OpenFile` — request one or more existing files.
    #[allow(
        clippy::too_many_arguments,
        clippy::used_underscore_binding,
        reason = "argument list mirrors the fixed org.freedesktop.impl.portal.FileChooser.OpenFile D-Bus signature; zbus's #[interface] dispatch wraps every call in an async task that captures unused underscore-prefixed parameters, tripping used_underscore_binding on the desugared code"
    )]
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
        let opts = FilePickerOptions::new(caller.clone(), title, /*save=*/ false)
            .multiple(opt_bool(&options, "multiple").unwrap_or(false))
            .accept_label(opt_string(&options, "accept_label"))
            .filters(filters_from_options(&options));
        self.pick_and_grant(opts, FsMode::Read).await
    }

    /// `SaveFile` — request a destination for one new file.
    #[allow(
        clippy::too_many_arguments,
        clippy::used_underscore_binding,
        reason = "argument list mirrors the fixed org.freedesktop.impl.portal.FileChooser.SaveFile D-Bus signature; zbus's #[interface] dispatch wraps every call in an async task that captures unused underscore-prefixed parameters, tripping used_underscore_binding on the desugared code"
    )]
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
        let opts = FilePickerOptions::new(caller.clone(), title, /*save=*/ true)
            .accept_label(opt_string(&options, "accept_label"))
            .current_name(opt_string(&options, "current_name"))
            .filters(filters_from_options(&options));
        self.pick_and_grant(opts, FsMode::ReadWrite).await
    }

    /// `SaveFiles` — request a folder + multiple destination filenames.
    #[allow(
        clippy::too_many_arguments,
        clippy::used_underscore_binding,
        reason = "argument list mirrors the fixed org.freedesktop.impl.portal.FileChooser.SaveFiles D-Bus signature; zbus's #[interface] dispatch wraps every call in an async task that captures unused underscore-prefixed parameters, tripping used_underscore_binding on the desugared code"
    )]
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
        let opts = FilePickerOptions::new(caller.clone(), title, /*save=*/ true)
            .accept_label(opt_string(&options, "accept_label"))
            .filters(filters_from_options(&options));
        self.pick_and_grant(opts, FsMode::ReadWrite).await
    }
}

impl FileChooserBackend {
    /// Raise the dialog, and turn what the user picked into both a grant and a response.
    ///
    /// **The dialog is the prompt.** There is deliberately no separate "may
    /// this app read the filesystem?" prompt in front of this — that
    /// question has no good abstract answer, unlike "open which file?", and
    /// a blanket capability on `"/"` is not what the user is consenting to.
    /// Browsing a compositor-owned dialog and choosing a file is the act of
    /// authorisation, naming exactly one path — nothing wider, nothing the app chose.
    async fn pick_and_grant(
        &self,
        opts: FilePickerOptions,
        mode: FsMode,
    ) -> (u32, HashMap<String, OwnedValue>) {
        let caller = opts.caller.clone();
        let save = opts.save;
        let rx = self.ctx.file_picker.request_with(opts);
        // Spawner dropped the reply — treat as cancel, the same defensive
        // posture as `flow::resolve`'s "channel-closed-without-reply ≡ deny": a request nobody answered is not an approved one.
        let picked = rx.await.unwrap_or_default();
        let Some(paths) = picked.filter(|p| !p.is_empty()) else {
            return (1, HashMap::new());
        };

        // Record what the user just authorised, per path — this is what
        // survives into `Workspace::capabilities`, becomes a bind on the app's
        // next launch, and shows in the Permissions tab so the user can revoke it.
        for path in &paths {
            self.ctx.grants.insert(
                &caller,
                &Capability::Filesystem {
                    path: path.clone(),
                    mode: mode.clone(),
                    scope: GRANT_SESSION,
                },
            );
        }

        // Publish each path into the caller's document store, so the running
        // app can actually reach it now: an already-confined app's mount
        // namespace was fixed at launch, so the grant alone isn't enough.
        let published = self.ctx.documents.publish(&caller, &paths, mode);
        paths_to_open_response(Some(published), save)
    }
}

/// Decode the portal's `filters` option: `a(sa(us))`.
///
/// A malformed entry is skipped rather than failing the call — filters
/// are a display convenience, and refusing to open a dialog over one bad filter would turn a cosmetic problem into an unusable Open menu.
pub(super) fn filters_from_options(
    options: &HashMap<String, OwnedValue>,
) -> Vec<super::PickerFilter> {
    let Some(value) = options.get("filters").and_then(|v| v.try_clone().ok()) else {
        return Vec::new();
    };
    let Ok(entries) = <Vec<(String, Vec<(u32, String)>)>>::try_from(value) else {
        return Vec::new();
    };
    entries
        .into_iter()
        .map(|(label, pairs)| super::PickerFilter::from_pairs(label, &pairs))
        .collect()
}

fn opt_bool(options: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    let v = options.get(key)?.try_clone().ok()?;
    bool::try_from(v).ok()
}

fn opt_string(options: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    let v = options.get(key)?.try_clone().ok()?;
    String::try_from(v).ok().filter(|s| !s.is_empty())
}

/// Translate a real user-selected path list into the
/// `(response_code, results)` pair the `FileChooser` frontend expects.
///
/// `response_code` follows the freedesktop.org Request convention: 0 =
/// OK (with results), 1 = user cancelled, 2 = other failure. `None`
/// (dialog dismissed without picking) is treated as cancel.
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
            // Naive `file://` formatter — good enough for ASCII paths, matching the
            // path representation our compositor uses internally. Spec-compliant
            // percent-encoding for binary path bytes is deferred (most desktop apps only get ASCII paths).
            format!("file://{}", p.display())
        })
        .collect();
    let uris_value = Value::new(uris);
    if let Ok(owned) = OwnedValue::try_from(uris_value) {
        results.insert("uris".to_string(), owned);
    }
    if save && let Ok(w) = OwnedValue::try_from(Value::Bool(true)) {
        results.insert("writable".to_string(), w);
    }
    (0, results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portal::{
        CallerMap, FilePickerSpawnerHandle, GrantStoreHandle, MockFilePickerSpawner,
        MockScreenCastProvisioner, PromptQueueHandle, PromptResolver,
        ScreenCastStreamProvisionerHandle,
    };
    use crate::shell::state::Workspace;

    use std::sync::Arc;

    fn empty_ws(id: u64) -> Workspace {
        Workspace::test_default(WorkspaceId(id), "fc")
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
        // Empty list ≡ cancel — keeps the contract symmetric with `None` (the spawner uses an empty Vec ≡ cancel internally).
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
            print_spooler: crate::portal::PrintSpoolerHandle::null(),
            documents: crate::portal::DocumentStoreHandle::host_paths(),
        };
        let caller = FileChooserBackend::resolve_caller(&ctx, None, "org.example");
        assert_eq!(caller.app_id, "org.example");
        // Zero workspace id flags "no bridge match" — production-side grant
        // lookup returns None and falls through to the prompt path (the conservative posture).
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
            print_spooler: crate::portal::PrintSpoolerHandle::null(),
            documents: crate::portal::DocumentStoreHandle::host_paths(),
        };
        let name: OwnedUniqueName = ":1.7".try_into().unwrap();
        let caller = FileChooserBackend::resolve_caller(&ctx, Some(&name), "ignored");
        assert_eq!(caller.app_id, "org.real");
        assert_eq!(caller.workspace_id, ws_id);
    }

    /// Wires a `MockFilePickerSpawner` returning `/tmp/test.txt`, calls the
    /// backend's response builder, and asserts the response contains `file:///tmp/test.txt`.
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
            print_spooler: crate::portal::PrintSpoolerHandle::null(),
            documents: crate::portal::DocumentStoreHandle::host_paths(),
        };
        let backend = FileChooserBackend::new(ctx);
        let caller = PortalCaller::new(ws_id, "org.example");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (code, results) = rt.block_on(backend.pick_and_grant(
            FilePickerOptions::new(caller, "Open file", /*save=*/ false),
            FsMode::Read,
        ));
        assert_eq!(code, 0);
        let uris_v = results.get("uris").expect("uris");
        let cloned: OwnedValue = uris_v.try_clone().expect("clone OwnedValue");
        let uris: Vec<String> = Vec::<String>::try_from(cloned).expect("Vec<String>");
        assert_eq!(uris, vec!["file:///tmp/test.txt".to_string()]);
        let seen = mock.requests();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].1, "Open file");
        assert!(!seen[0].2);
    }

    /// A grant on `"/"` would cover the launcher's whole
    /// `--ro-bind-try <p> <p>` subtree, read-only mounting the entire host
    /// filesystem at the app's next launch — so this asserts the grant
    /// names only the picked path, on the production path
    /// (`pick_and_grant`), against the real grant store.
    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the read guard protects `ws`, which the closure below needs through the \
                  block's final `.collect()` — already dropped at the earliest possible point"
    )]
    fn the_grant_names_the_picked_path_and_never_the_filesystem_root() {
        use crate::shell::state::Capability;

        let (store, ws_id) = GrantStoreHandle::test_with_workspace(empty_ws(36));
        let picked = PathBuf::from("/tmp/only-this-one.txt");
        let fp = FilePickerSpawnerHandle::new(Arc::new(MockFilePickerSpawner::returning(vec![
            picked.clone(),
        ])));
        let sc = ScreenCastStreamProvisionerHandle::new(Arc::new(
            MockScreenCastProvisioner::new_with_node(1, "t"),
        ));
        let ctx = PortalContext {
            grants: store.clone(),
            prompts: PromptQueueHandle::new(PromptResolver::Channel),
            caller_map: CallerMap::new(),
            file_picker: fp,
            screencast_provisioner: sc,
            remote_input_injector: crate::portal::RemoteInputInjectorHandle::null(),
            print_spooler: crate::portal::PrintSpoolerHandle::null(),
            documents: crate::portal::DocumentStoreHandle::host_paths(),
        };
        let backend = FileChooserBackend::new(ctx);
        let caller = PortalCaller::new(ws_id, "org.example");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (code, _) = rt.block_on(backend.pick_and_grant(
            FilePickerOptions::new(caller.clone(), "Open", false),
            FsMode::Read,
        ));
        assert_eq!(code, 0);

        // Read the capabilities back out of the workspace the store wrote to, not out of anything this test built.
        let tree = store.tree_handle();
        let fs_paths: Vec<PathBuf> = {
            let guard = tree.read();
            let ws = guard
                .workspaces
                .iter()
                .find(|w| w.id == ws_id)
                .expect("workspace");
            ws.app_capabilities(&caller.app_id)
                .into_iter()
                .filter_map(|c| match Capability::from_capability(c) {
                    Some(Capability::Filesystem { path, .. }) => Some(path),
                    _ => None,
                })
                .collect()
        };

        assert!(
            fs_paths.contains(&picked),
            "the path the user picked must be granted; got {fs_paths:?}"
        );
        assert!(
            !fs_paths.iter().any(|p| p == std::path::Path::new("/")),
            "a file-open must never grant the filesystem root; got {fs_paths:?}"
        );
    }

    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the read guard protects `ws`, which the closure below needs through the \
                  block's final `.count()` — already dropped at the earliest possible point"
    )]
    fn a_cancelled_dialog_grants_nothing() {
        use crate::shell::state::Capability;

        let (store, ws_id) = GrantStoreHandle::test_with_workspace(empty_ws(37));
        let fp = FilePickerSpawnerHandle::new(Arc::new(MockFilePickerSpawner::always_cancel()));
        let sc = ScreenCastStreamProvisionerHandle::new(Arc::new(
            MockScreenCastProvisioner::new_with_node(1, "t"),
        ));
        let ctx = PortalContext {
            grants: store.clone(),
            prompts: PromptQueueHandle::new(PromptResolver::Channel),
            caller_map: CallerMap::new(),
            file_picker: fp,
            screencast_provisioner: sc,
            remote_input_injector: crate::portal::RemoteInputInjectorHandle::null(),
            print_spooler: crate::portal::PrintSpoolerHandle::null(),
            documents: crate::portal::DocumentStoreHandle::host_paths(),
        };
        let backend = FileChooserBackend::new(ctx);
        let caller = PortalCaller::new(ws_id, "org.example");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (code, _) = rt.block_on(backend.pick_and_grant(
            FilePickerOptions::new(caller.clone(), "Open", false),
            FsMode::Read,
        ));
        assert_eq!(code, 1, "a cancelled dialog is response=1");

        let tree = store.tree_handle();
        let granted_fs = {
            let guard = tree.read();
            let ws = guard
                .workspaces
                .iter()
                .find(|w| w.id == ws_id)
                .expect("workspace");
            ws.app_capabilities(&caller.app_id)
                .into_iter()
                .filter(|c| {
                    matches!(
                        Capability::from_capability(c),
                        Some(Capability::Filesystem { .. })
                    )
                })
                .count()
        };
        assert_eq!(
            granted_fs, 0,
            "declining the dialog must not hand the app any filesystem access"
        );
    }

    #[test]
    fn open_file_with_cancelled_spawner_returns_response_one() {
        let (store, ws_id) = GrantStoreHandle::test_with_workspace(empty_ws(34));
        let queue = PromptQueueHandle::new(PromptResolver::AutoAllow);
        let fp = FilePickerSpawnerHandle::new(Arc::new(MockFilePickerSpawner::always_cancel()));
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
            print_spooler: crate::portal::PrintSpoolerHandle::null(),
            documents: crate::portal::DocumentStoreHandle::host_paths(),
        };
        let backend = FileChooserBackend::new(ctx);
        let caller = PortalCaller::new(ws_id, "org.example");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (code, results) = rt.block_on(
            backend.pick_and_grant(FilePickerOptions::new(caller, "Open", false), FsMode::Read),
        );
        assert_eq!(code, 1);
        assert!(results.is_empty());
    }

    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the read guard protects `ws`, which the closure below needs through the \
                  block's final `.any()` — already dropped at the earliest possible point"
    )]
    fn a_save_grant_is_writable_and_an_open_grant_is_not() {
        use crate::shell::state::Capability;

        for (save, mode, expect_write) in [
            (false, FsMode::Read, false),
            (true, FsMode::ReadWrite, true),
        ] {
            let (store, ws_id) = GrantStoreHandle::test_with_workspace(empty_ws(38));
            let picked = PathBuf::from("/tmp/target.txt");
            let fp =
                FilePickerSpawnerHandle::new(Arc::new(MockFilePickerSpawner::returning(vec![
                    picked.clone(),
                ])));
            let sc = ScreenCastStreamProvisionerHandle::new(Arc::new(
                MockScreenCastProvisioner::new_with_node(1, "t"),
            ));
            let ctx = PortalContext {
                grants: store.clone(),
                prompts: PromptQueueHandle::new(PromptResolver::Channel),
                caller_map: CallerMap::new(),
                file_picker: fp,
                screencast_provisioner: sc,
                remote_input_injector: crate::portal::RemoteInputInjectorHandle::null(),
                print_spooler: crate::portal::PrintSpoolerHandle::null(),
                documents: crate::portal::DocumentStoreHandle::host_paths(),
            };
            let backend = FileChooserBackend::new(ctx);
            let caller = PortalCaller::new(ws_id, "org.example");
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (code, _) = rt.block_on(
                backend.pick_and_grant(FilePickerOptions::new(caller.clone(), "t", save), mode),
            );
            assert_eq!(code, 0);

            let tree = store.tree_handle();
            let writable = {
                let guard = tree.read();
                let ws = guard
                    .workspaces
                    .iter()
                    .find(|w| w.id == ws_id)
                    .expect("workspace");
                ws.app_capabilities(&caller.app_id)
                    .into_iter()
                    .filter_map(|c| match Capability::from_capability(c) {
                        Some(Capability::Filesystem { path, mode, .. }) if path == picked => {
                            Some(mode)
                        }
                        _ => None,
                    })
                    .any(|m| matches!(m, FsMode::ReadWrite))
            };
            assert_eq!(
                writable, expect_write,
                "save={save}: an Open must not hand out write access, and a \
                 Save must — otherwise the app's first write raises a second \
                 prompt for the file the user just chose to save to"
            );
        }
    }
}
