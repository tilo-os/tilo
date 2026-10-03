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
//! a live Tilo application before it calls `FilePickerSpawner::spawn` to
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

use crate::shell::state::FsMode;
#[cfg(target_os = "linux")]
use zbus::interface;
#[cfg(target_os = "linux")]
use zbus::names::OwnedUniqueName;
#[cfg(target_os = "linux")]
use zbus::object_server::SignalEmitter;
#[cfg(target_os = "linux")]
use zbus::zvariant::ObjectPath;

use std::collections::HashMap;
use std::path::PathBuf;

use zbus::zvariant::{OwnedValue, Value};

#[cfg(test)]
use super::PortalCaller;
use super::{FilePickerOptions, FilePickerSpawnerHandle};
#[cfg(target_os = "linux")]
use super::{PortalContext, auth::authenticate_call, lockdown};

/// D-Bus interface impl. One instance per [`super::serve`] call, held by the zbus `ObjectServer` for the connection's lifetime.
#[cfg(target_os = "linux")]
pub struct FileChooserBackend {
    ctx: PortalContext,
}

#[cfg(target_os = "linux")]
impl FileChooserBackend {
    pub fn new(ctx: PortalContext) -> Self {
        Self { ctx }
    }
}

#[interface(name = "org.freedesktop.impl.portal.FileChooser")]
#[cfg(target_os = "linux")]
impl FileChooserBackend {
    /// `OpenFile` — request one or more existing files.
    async fn open_file(
        &self,
        #[zbus(signal_emitter)] _ctx: SignalEmitter<'_>,
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
        let opts = decode_open_options(caller.clone(), title, &options);
        pick_and_grant(&self.ctx.file_picker, opts, FsMode::Read).await
    }

    /// `SaveFile` — request a destination for one new file.
    async fn save_file(
        &self,
        #[zbus(signal_emitter)] _ctx: SignalEmitter<'_>,
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
        pick_and_grant(&self.ctx.file_picker, opts, FsMode::ReadWrite).await
    }

    /// `SaveFiles` — request a folder + multiple destination filenames.
    async fn save_files(
        &self,
        #[zbus(signal_emitter)] _ctx: SignalEmitter<'_>,
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
        pick_and_grant(&self.ctx.file_picker, opts, FsMode::ReadWrite).await
    }
}

/// Raise the dialog and return the selection for the document portal to grant.
///
/// The caller must already be authenticated by the host adapter. This operation
/// grants no collaborative workspace capability and performs no authentication.
///
/// **The dialog is the prompt.** There is deliberately no separate "may
/// this app read the filesystem?" prompt in front of this — that
/// question has no good abstract answer, unlike "open which file?", and
/// a blanket capability on `"/"` is not what the user is consenting to.
/// Browsing a compositor-owned dialog and choosing a file is the act of
/// authorisation, naming exactly one path — nothing wider, nothing the app chose.
pub async fn pick_and_grant(
    file_picker: &FilePickerSpawnerHandle,
    opts: FilePickerOptions,
    mode: FsMode,
) -> (u32, HashMap<String, OwnedValue>) {
    let caller = opts.caller.clone();
    let save = opts.save;
    let rx = file_picker.request_with(opts);
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

/// Decode the portal's `filters` option: `a(sa(us))`.
///
/// A malformed entry is skipped rather than failing the call — filters
/// are a display convenience, and refusing to open a dialog over one bad filter would turn a cosmetic problem into an unusable Open menu.
#[cfg(any(target_os = "linux", test))]
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

#[cfg(any(target_os = "linux", test))]
fn opt_bool(options: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    let v = options.get(key)?.try_clone().ok()?;
    bool::try_from(v).ok()
}

#[cfg(any(target_os = "linux", test))]
fn opt_string(options: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    let v = options.get(key)?.try_clone().ok()?;
    String::try_from(v).ok().filter(|s| !s.is_empty())
}

#[cfg(any(target_os = "linux", test))]
fn decode_open_options(
    caller: super::PortalCaller,
    title: String,
    options: &HashMap<String, OwnedValue>,
) -> FilePickerOptions {
    FilePickerOptions::new(caller, title, /*save=*/ false)
        .directory(opt_bool(options, "directory").unwrap_or(false))
        .multiple(opt_bool(options, "multiple").unwrap_or(false))
        .accept_label(opt_string(options, "accept_label"))
        .filters(filters_from_options(options))
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

/// Encode an absolute path as an RFC 8089 file URI, percent-encoding bytes
/// that are not UTF-8 rather than converting them lossily.
pub(crate) fn file_uri(path: &std::path::Path) -> Option<String> {
    url::Url::from_file_path(path).ok().map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portal::{GrantStoreHandle, MockFilePickerSpawner};
    use crate::shell::state::WorkspaceAuthority as _;
    use crate::shell::state::{Workspace, WorkspaceId};
    use std::os::unix::ffi::OsStrExt as _;

    use std::sync::Arc;

    fn empty_ws(id: u64) -> Workspace {
        Workspace::new(WorkspaceId(id), "fc")
    }

    #[test]
    fn open_options_decode_selection_mode_independently() {
        // Phase `open_options_default_to_file_selection`.
        {
            let opts = decode_open_options(
                PortalCaller::new(WorkspaceId(30), "org.example"),
                "Open".into(),
                &HashMap::new(),
            );
            assert!(!opts.directory);
            assert!(!opts.multiple);
        }
        // Phase `open_options_decode_directory_independently_of_multiple_and_label`.
        {
            let mut options = HashMap::new();
            options.insert(
                "directory".into(),
                OwnedValue::try_from(Value::Bool(true)).unwrap(),
            );
            options.insert(
                "multiple".into(),
                OwnedValue::try_from(Value::Bool(true)).unwrap(),
            );
            options.insert(
                "accept_label".into(),
                OwnedValue::try_from(Value::new("Attach folder".to_string())).unwrap(),
            );
            let opts = decode_open_options(
                PortalCaller::new(WorkspaceId(31), "org.example"),
                "Choose".into(),
                &options,
            );
            assert!(opts.directory);
            assert!(opts.multiple);
            assert_eq!(opts.accept_label.as_deref(), Some("Attach folder"));
        }
    }

    #[test]
    fn chooser_paths_map_to_portal_responses_and_encoded_uris() {
        // Phase `paths_allow_returns_response_zero_and_uri`.
        {
            let (code, results) = paths_to_open_response(
                Some(vec![PathBuf::from("/tmp/x.txt")]),
                /*save=*/ false,
            );
            assert_eq!(code, 0);
            assert!(results.contains_key("uris"));
            assert!(!results.contains_key("writable"));
        }
        // Phase `paths_save_returns_writable_true`.
        {
            let (code, results) = paths_to_open_response(
                Some(vec![PathBuf::from("/tmp/out.bin")]),
                /*save=*/ true,
            );
            assert_eq!(code, 0);
            assert!(results.contains_key("uris"));
            assert!(results.contains_key("writable"));
        }
        // Phase `paths_none_returns_response_one_no_results`.
        {
            let (code, results) = paths_to_open_response(None, /*save=*/ false);
            assert_eq!(code, 1);
            assert!(results.is_empty());
        }
        // Phase `paths_empty_returns_response_one`.
        {
            // Empty list ≡ cancel — keeps the contract symmetric with `None` (the spawner uses an empty Vec ≡ cancel internally).
            let (code, results) = paths_to_open_response(Some(vec![]), /*save=*/ false);
            assert_eq!(code, 1);
            assert!(results.is_empty());
        }
        // Phase `paths_serialise_to_file_uri`.
        {
            let (_code, results) =
                paths_to_open_response(Some(vec![PathBuf::from("/tmp/a.txt")]), false);
            let uris_v = results.get("uris").expect("uris");
            let cloned: OwnedValue = uris_v.try_clone().expect("clone OwnedValue");
            let uris: Vec<String> = Vec::<String>::try_from(cloned).expect("Vec<String>");
            assert_eq!(uris, vec!["file:///tmp/a.txt".to_string()]);
        }
        // Phase `file_uri_percent_encodes_reserved_and_non_utf8_bytes`.
        {
            use std::ffi::OsStr;

            let path = PathBuf::from(OsStr::from_bytes(b"/tmp/a #\xff.txt"));
            assert_eq!(
                file_uri(&path).as_deref(),
                Some("file:///tmp/a%20%23%FF.txt")
            );
            assert!(file_uri(std::path::Path::new("relative.txt")).is_none());
        }
    }

    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the read guard protects `ws`, which the closure below needs through the \
                  block's final `.count()` — already dropped at the earliest possible point"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn open_and_save_flows_grant_only_what_the_user_picked() {
        // Phase `open_file_returns_user_selected_uri`.
        // Wires a `MockFilePickerSpawner` returning `/tmp/test.txt`, calls the
        // backend's response builder, and asserts the response contains `file:///tmp/test.txt`.
        {
            let (_store, ws_id) = GrantStoreHandle::test_with_workspace(empty_ws(33));
            let mock = Arc::new(MockFilePickerSpawner::returning(vec![PathBuf::from(
                "/tmp/test.txt",
            )]));
            let fp = FilePickerSpawnerHandle::new(mock.clone());
            let caller = PortalCaller::new(ws_id, "org.example");
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (code, results) = rt.block_on(pick_and_grant(
                &fp,
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
        // Phase `a_pick_is_left_to_the_document_portal_and_not_replicated_to_peers`.
        {
            let (store, ws_id) = GrantStoreHandle::test_with_workspace(empty_ws(36));
            let picked = PathBuf::from("/tmp/only-this-one.txt");
            let fp =
                FilePickerSpawnerHandle::new(Arc::new(MockFilePickerSpawner::returning(vec![
                    picked.clone(),
                ])));
            let caller = PortalCaller::new(ws_id, "org.example");
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (code, _) = rt.block_on(pick_and_grant(
                &fp,
                FilePickerOptions::new(caller.clone(), "Open", false),
                FsMode::Read,
            ));
            assert_eq!(code, 0);

            // Read the capabilities back out of the workspace the store wrote to, not out of anything this test built.
            let tree = store.tree_handle();
            let grant_count = {
                let guard = tree.read();
                let ws = guard
                    .desktop
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
        // Phase `a_cancelled_dialog_grants_nothing`.
        {
            use crate::shell::state::Capability;

            let (store, ws_id) = GrantStoreHandle::test_with_workspace(empty_ws(37));
            let fp = FilePickerSpawnerHandle::new(Arc::new(MockFilePickerSpawner::always_cancel()));
            let caller = PortalCaller::new(ws_id, "org.example");
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (code, _) = rt.block_on(pick_and_grant(
                &fp,
                FilePickerOptions::new(caller.clone(), "Open", false),
                FsMode::Read,
            ));
            assert_eq!(code, 1, "a cancelled dialog is response=1");

            let tree = store.tree_handle();
            let granted_fs = {
                let guard = tree.read();
                let ws = guard
                    .desktop
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
        // Phase `open_file_with_cancelled_spawner_returns_response_one`.
        {
            let (_store, ws_id) = GrantStoreHandle::test_with_workspace(empty_ws(34));
            let fp = FilePickerSpawnerHandle::new(Arc::new(MockFilePickerSpawner::always_cancel()));
            let caller = PortalCaller::new(ws_id, "org.example");
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (code, results) = rt.block_on(pick_and_grant(
                &fp,
                FilePickerOptions::new(caller, "Open", false),
                FsMode::Read,
            ));
            assert_eq!(code, 1);
            assert!(results.is_empty());
        }
        // Phase `save_responses_are_writable_and_open_responses_are_not`.
        for (save, mode, expect_write) in [
            (false, FsMode::Read, false),
            (true, FsMode::ReadWrite, true),
        ] {
            let (_store, ws_id) = GrantStoreHandle::test_with_workspace(empty_ws(38));
            let picked = PathBuf::from("/tmp/target.txt");
            let fp =
                FilePickerSpawnerHandle::new(Arc::new(MockFilePickerSpawner::returning(vec![
                    picked.clone(),
                ])));
            let caller = PortalCaller::new(ws_id, "org.example");
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (code, result) = rt.block_on(pick_and_grant(
                &fp,
                FilePickerOptions::new(caller.clone(), "t", save),
                mode,
            ));
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

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use crate::shell::state::WorkspaceExt as _;
