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
//! Every method authenticates the portal frontend and resolves its app id to
//! a live qOS application before it calls [`FilePickerSpawner::spawn`] to
//! surface a compositor-owned dialog. The reply
//! (`Option<Vec<PathBuf>>`) becomes a `uris` array, with `None` treated
//! as `response=1` (cancelled). On `Deny`, returns response=1 with an empty results dict.

#![allow(
    clippy::used_underscore_binding,
    reason = "the D-Bus method signatures below require unused positional parameters (handle, \
              parent_window, ...); zbus's #[interface] macro deserialises them into these named \
              parameters and forwards them by identifier into its generated dispatcher, so the \
              lint fires on macro-generated code a per-function #[allow] cannot reach"
)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::os::unix::ffi::OsStrExt as _;
use std::path::PathBuf;

use zbus::interface;
use zbus::names::OwnedUniqueName;
use zbus::object_server::SignalContext;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::shell::state::FsMode;

#[cfg(test)]
use super::PortalCaller;
use super::{FilePickerOptions, PortalContext, auth::authenticate_call, lockdown};

/// D-Bus interface impl. One instance per [`super::serve`] call, held by the zbus `ObjectServer` for the connection's lifetime.
pub struct FileChooserBackend {
    ctx: PortalContext,
}

impl FileChooserBackend {
    pub fn new(ctx: PortalContext) -> Self {
        Self { ctx }
    }
}

#[interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl FileChooserBackend {
    /// `OpenFile` — request one or more existing files.
    #[allow(
        clippy::too_many_arguments,
        reason = "argument list mirrors the fixed org.freedesktop.impl.portal.FileChooser.OpenFile D-Bus signature"
    )]
    async fn open_file(
        &self,
        #[zbus(signal_context)] _ctx: SignalContext<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        _handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        tracing::debug!(
            "FileChooser.OpenFile: app={app_id} title={title} opts={}",
            options.len()
        );
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let Ok(auth) =
            authenticate_call(&self.ctx, sender.as_ref(), &app_id, Some(&parent_window)).await
        else {
            return (2, HashMap::new());
        };
        let caller = auth.caller;
        let opts = FilePickerOptions::new(caller.clone(), title, /*save=*/ false)
            .multiple(opt_bool(&options, "multiple").unwrap_or(false))
            .accept_label(opt_string(&options, "accept_label"))
            .filters(filters_from_options(&options));
        self.pick_and_grant(opts, FsMode::Read).await
    }

    /// `SaveFile` — request a destination for one new file.
    #[allow(
        clippy::too_many_arguments,
        reason = "argument list mirrors the fixed org.freedesktop.impl.portal.FileChooser.SaveFile D-Bus signature"
    )]
    async fn save_file(
        &self,
        #[zbus(signal_context)] _ctx: SignalContext<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        _handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        tracing::debug!(
            "FileChooser.SaveFile: app={app_id} title={title} opts={}",
            options.len()
        );
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let Ok(auth) =
            authenticate_call(&self.ctx, sender.as_ref(), &app_id, Some(&parent_window)).await
        else {
            return (2, HashMap::new());
        };
        if lockdown::current_policy().disable_save_to_disk {
            return (1, HashMap::new());
        }
        let caller = auth.caller;
        let opts = FilePickerOptions::new(caller.clone(), title, /*save=*/ true)
            .accept_label(opt_string(&options, "accept_label"))
            .current_name(opt_string(&options, "current_name"))
            .filters(filters_from_options(&options));
        self.pick_and_grant(opts, FsMode::ReadWrite).await
    }

    /// `SaveFiles` — request a folder + multiple destination filenames.
    #[allow(
        clippy::too_many_arguments,
        reason = "argument list mirrors the fixed org.freedesktop.impl.portal.FileChooser.SaveFiles D-Bus signature"
    )]
    async fn save_files(
        &self,
        #[zbus(signal_context)] _ctx: SignalContext<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        _handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        tracing::debug!(
            "FileChooser.SaveFiles: app={app_id} title={title} opts={}",
            options.len()
        );
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let Ok(auth) =
            authenticate_call(&self.ctx, sender.as_ref(), &app_id, Some(&parent_window)).await
        else {
            return (2, HashMap::new());
        };
        if lockdown::current_policy().disable_save_to_disk {
            return (1, HashMap::new());
        }
        let caller = auth.caller;
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

        // Return host file URIs to xdg-desktop-portal. The frontend exports
        // them through xdg-document-portal before replying to the sandbox,
        // which gives the already-running app a revocable document path.
        // Recording these private choices in collaborative workspace grants
        // would both leak them to peers and widen the next launch's mounts.
        let _ = (caller, mode);
        paths_to_open_response(Some(paths), save)
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
    let Some(uris) = paths
        .iter()
        .map(|path| file_uri(path))
        .collect::<Option<Vec<_>>>()
    else {
        return (2, HashMap::new());
    };
    let uris_value = Value::new(uris);
    if let Ok(owned) = OwnedValue::try_from(uris_value) {
        results.insert("uris".to_string(), owned);
    }
    if save && let Ok(w) = OwnedValue::try_from(Value::Bool(true)) {
        results.insert("writable".to_string(), w);
    }
    (0, results)
}

/// Encode an absolute Linux path as an RFC 8089 file URI without forcing
/// arbitrary filesystem bytes through lossy UTF-8 conversion.
pub(crate) fn file_uri(path: &std::path::Path) -> Option<String> {
    if !path.is_absolute() {
        return None;
    }
    let mut uri = String::from("file://");
    for byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'_' | b'~') {
            uri.push(char::from(*byte));
        } else {
            write!(uri, "%{byte:02X}").ok()?;
        }
    }
    Some(uri)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portal::{
        CallerMap, FilePickerSpawnerHandle, GrantStoreHandle, MockFilePickerSpawner,
        MockScreenCastProvisioner, PromptQueueHandle, PromptResolver,
        ScreenCastStreamProvisionerHandle,
    };
    use crate::shell::state::{Workspace, WorkspaceId};

    use std::sync::Arc;

    fn empty_ws(id: u64) -> Workspace {
        Workspace::test_default(WorkspaceId(id), "fc")
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
    fn file_uri_percent_encodes_reserved_and_non_utf8_bytes() {
        use std::ffi::OsStr;

        let path = PathBuf::from(OsStr::from_bytes(b"/tmp/a #\xff.txt"));
        assert_eq!(
            file_uri(&path).as_deref(),
            Some("file:///tmp/a%20%23%FF.txt")
        );
        assert!(file_uri(std::path::Path::new("relative.txt")).is_none());
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
            input_capture: crate::portal::InputCaptureCompositorHandle::unavailable(),
            print_spooler: crate::portal::PrintSpoolerHandle::null(),
            documents: crate::portal::DocumentStoreHandle::host_paths(),
            notifications: crate::notifications::NotificationBridge::new(),
            desktop_services: Default::default(),
            authenticator: None,
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

    #[test]
    fn a_pick_is_left_to_the_document_portal_and_not_replicated_to_peers() {
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
            input_capture: crate::portal::InputCaptureCompositorHandle::unavailable(),
            print_spooler: crate::portal::PrintSpoolerHandle::null(),
            documents: crate::portal::DocumentStoreHandle::host_paths(),
            notifications: crate::notifications::NotificationBridge::new(),
            desktop_services: Default::default(),
            authenticator: None,
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
        let grant_count = {
            let guard = tree.read();
            let ws = guard
                .workspaces
                .iter()
                .find(|w| w.id == ws_id)
                .expect("workspace");
            ws.app_capabilities(&caller.app_id).into_iter().count()
        };
        assert_eq!(
            grant_count, 0,
            "private file choices must not enter the collaborative workspace document"
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
            input_capture: crate::portal::InputCaptureCompositorHandle::unavailable(),
            print_spooler: crate::portal::PrintSpoolerHandle::null(),
            documents: crate::portal::DocumentStoreHandle::host_paths(),
            notifications: crate::notifications::NotificationBridge::new(),
            desktop_services: Default::default(),
            authenticator: None,
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
            input_capture: crate::portal::InputCaptureCompositorHandle::unavailable(),
            print_spooler: crate::portal::PrintSpoolerHandle::null(),
            documents: crate::portal::DocumentStoreHandle::host_paths(),
            notifications: crate::notifications::NotificationBridge::new(),
            desktop_services: Default::default(),
            authenticator: None,
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
    fn save_responses_are_writable_and_open_responses_are_not() {
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
                input_capture: crate::portal::InputCaptureCompositorHandle::unavailable(),
                print_spooler: crate::portal::PrintSpoolerHandle::null(),
                documents: crate::portal::DocumentStoreHandle::host_paths(),
                notifications: crate::notifications::NotificationBridge::new(),
                desktop_services: Default::default(),
                authenticator: None,
            };
            let backend = FileChooserBackend::new(ctx);
            let caller = PortalCaller::new(ws_id, "org.example");
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (code, result) = rt.block_on(
                backend.pick_and_grant(FilePickerOptions::new(caller.clone(), "t", save), mode),
            );
            assert_eq!(code, 0);
            let writable = result
                .get("writable")
                .and_then(|value| value.try_clone().ok())
                .and_then(|value| bool::try_from(value).ok())
                .unwrap_or(false);
            assert_eq!(
                writable, expect_write,
                "save={save}: an Open must not hand out write access, and a \
                 Save must — otherwise the app's first write raises a second \
                 prompt for the file the user just chose to save to"
            );
        }
    }
}
