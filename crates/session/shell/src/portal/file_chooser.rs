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

use super::reply::{Ended, OrFail, Outcome, Reply, Results, answer};
use crate::shell::state::FsMode;
#[cfg(target_os = "linux")]
use zbus::interface;
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
    ) -> Reply {
        answer(async move {
            tracing::debug!(
                "FileChooser.OpenFile: app={app_id} title={title} opts={}",
                options.len()
            );
            let auth = authenticate_call(&self.ctx, &hdr, &app_id, Some(&parent_window))
                .await
                .or_fail()?;
            let caller = auth.caller;
            let opts = decode_open_options(caller.clone(), title, &options);
            let mode = if writable_directory_requested(&options) {
                if lockdown::current_policy().disable_save_to_disk {
                    return Err(Ended::Cancelled);
                }
                FsMode::ReadWrite
            } else {
                FsMode::Read
            };
            pick_and_grant(&self.ctx.file_picker, opts, mode).await
        })
        .await
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
    ) -> Reply {
        answer(async move {
            tracing::debug!(
                "FileChooser.SaveFile: app={app_id} title={title} opts={}",
                options.len()
            );
            let auth = authenticate_call(&self.ctx, &hdr, &app_id, Some(&parent_window))
                .await
                .or_fail()?;
            if lockdown::current_policy().disable_save_to_disk {
                return Err(Ended::Cancelled);
            }
            let caller = auth.caller;
            let opts = FilePickerOptions::new(caller.clone(), title, /*save=*/ true)
                .accept_label(opt_string(&options, "accept_label"))
                .current_name(opt_string(&options, "current_name"))
                .filters(filters_from_options(&options));
            pick_and_grant(&self.ctx.file_picker, opts, FsMode::ReadWrite).await
        })
        .await
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
    ) -> Reply {
        answer(async move {
            tracing::debug!(
                "FileChooser.SaveFiles: app={app_id} title={title} opts={}",
                options.len()
            );
            let auth = authenticate_call(&self.ctx, &hdr, &app_id, Some(&parent_window))
                .await
                .or_fail()?;
            if lockdown::current_policy().disable_save_to_disk {
                return Err(Ended::Cancelled);
            }
            let caller = auth.caller;
            let opts = FilePickerOptions::new(caller.clone(), title, /*save=*/ true)
                .accept_label(opt_string(&options, "accept_label"))
                .filters(filters_from_options(&options));
            pick_and_grant(&self.ctx.file_picker, opts, FsMode::ReadWrite).await
        })
        .await
    }
}

/// Raise the dialog and return the selection for the document portal to grant.
///
/// The caller must already be authenticated by the host adapter. This operation
/// grants no collaborative workspace capability and performs no authentication.
/// `ReadWrite` on a directory is explicit destination consent: the frontend
/// exports only that selected directory with write permission. Ordinary Open
/// requests must pass `Read`; Save requests retain their write behavior.
///
/// **The dialog is the prompt.** There is deliberately no separate "may
/// this app read the filesystem?" prompt in front of this — that
/// question has no good abstract answer, unlike "open which file?", and
/// a blanket capability on `"/"` is not what the user is consenting to.
/// Browsing a compositor-owned dialog and choosing a file is the act of
/// authorisation, naming exactly one path — nothing wider, nothing the app chose.
pub async fn pick_and_grant(
    file_picker: &FilePickerSpawnerHandle,
    mut opts: FilePickerOptions,
    mode: FsMode,
) -> Outcome {
    let writable_directory = opts.directory && mode == FsMode::ReadWrite;
    // The host's access decision must reach the actual shared picker before
    // confirmation; the caller's choice alone is never consent.
    opts.writable_directory = writable_directory;
    let writable = opts.save || writable_directory;
    let rx = file_picker.request_with(opts);
    // Spawner dropped the reply — treat as cancel, the same defensive
    // posture as `flow::resolve`'s "channel-closed-without-reply ≡ deny": a request nobody answered is not an approved one.
    let picked = rx.await.unwrap_or_default();
    let Some(paths) = picked.filter(|p| !p.is_empty()) else {
        return Err(Ended::Cancelled);
    };

    // Return host file URIs to xdg-desktop-portal. The frontend exports
    // them through xdg-document-portal before replying to the sandbox,
    // which gives the already-running app a revocable document path.
    // Recording these private choices in collaborative workspace grants
    // would both leak them to peers and widen the next launch's mounts.
    if writable_directory && (paths.len() != 1 || !paths[0].is_dir()) {
        return Err(Ended::Failed);
    }
    let mut results = paths_to_open_response(Some(paths), writable)?;
    if writable_directory {
        let choices = vec![(
            tilo_dialogs::portal::WRITABLE_DIRECTORY_CHOICE.to_owned(),
            "true".to_owned(),
        )];
        if let Ok(value) = OwnedValue::try_from(Value::new(choices)) {
            results.insert("choices".into(), value);
        }
    }
    Ok(results)
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
fn opt_string(options: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    let v = options.get(key)?.try_clone().ok()?;
    String::try_from(v).ok().filter(|s| !s.is_empty())
}

/// Only a single-directory request can opt into writing. Unknown or malformed
/// choices and ordinary Open requests retain read-only behavior.
#[cfg(any(target_os = "linux", test))]
fn writable_directory_requested(options: &HashMap<String, OwnedValue>) -> bool {
    if crate::portal::option::<bool>(options, "directory") != Some(true)
        || crate::portal::option::<bool>(options, "multiple").unwrap_or(false)
    {
        return false;
    }
    type Choices = Vec<(String, String, Vec<(String, String)>, String)>;
    options
        .get("choices")
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| Choices::try_from(value).ok())
        .is_some_and(|choices| {
            choices.iter().any(|(id, _, alternatives, selected)| {
                id == tilo_dialogs::portal::WRITABLE_DIRECTORY_CHOICE
                    && alternatives.is_empty()
                    && selected == "true"
            })
        })
}

#[cfg(any(target_os = "linux", test))]
fn decode_open_options(
    caller: super::PortalCaller,
    title: String,
    options: &HashMap<String, OwnedValue>,
) -> FilePickerOptions {
    let mut decoded = FilePickerOptions::new(caller, title, /*save=*/ false)
        .directory(crate::portal::option::<bool>(options, "directory").unwrap_or(false))
        .multiple(crate::portal::option::<bool>(options, "multiple").unwrap_or(false))
        .accept_label(opt_string(options, "accept_label"))
        .filters(filters_from_options(options));
    decoded.writable_directory = writable_directory_requested(options);
    decoded
}

/// Translate a real user-selected path list into the results the
/// `FileChooser` frontend expects. No selection (`None`, or dismissed with
/// nothing picked) is a cancel.
pub(super) fn paths_to_open_response(paths: Option<Vec<PathBuf>>, writable: bool) -> Outcome {
    let mut results = Results::new();
    let paths = paths
        .filter(|paths| !paths.is_empty())
        .ok_or(Ended::Cancelled)?;
    let uris = paths
        .iter()
        .map(|path| file_uri(path))
        .collect::<Option<Vec<_>>>()
        .or_fail()?;
    let uris_value = Value::new(uris);
    if let Ok(owned) = OwnedValue::try_from(uris_value) {
        results.insert("uris".to_string(), owned);
    }
    // The frontend defaults missing `writable` to true. Always state the
    // access explicitly so a read-only Open cannot acquire write permission
    // when xdg-desktop-portal exports it through the document portal.
    results.insert("writable".to_string(), OwnedValue::from(writable));
    Ok(results)
}

/// Encode an absolute path as an RFC 8089 file URI, percent-encoding bytes
/// that are not UTF-8 rather than converting them lossily.
pub(crate) fn file_uri(path: &std::path::Path) -> Option<String> {
    url::Url::from_file_path(path).ok().map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portal::MockFilePickerSpawner;
    use crate::shell::state::WorkspaceId;
    use std::os::unix::ffi::OsStrExt as _;

    use std::sync::Arc;

    fn destination_options(directory: bool) -> HashMap<String, OwnedValue> {
        let choices = vec![(
            tilo_dialogs::portal::WRITABLE_DIRECTORY_CHOICE.to_owned(),
            "Import here".to_owned(),
            Vec::<(String, String)>::new(),
            "true".to_owned(),
        )];
        HashMap::from([
            ("directory".into(), OwnedValue::from(directory)),
            (
                "choices".into(),
                OwnedValue::try_from(Value::new(choices)).unwrap(),
            ),
        ])
    }

    #[test]
    fn writable_directory_requires_explicit_single_directory_request() {
        assert!(!writable_directory_requested(&HashMap::new()));
        assert!(!writable_directory_requested(&destination_options(false)));
        let mut options = destination_options(true);
        assert!(writable_directory_requested(&options));
        options.insert("multiple".into(), OwnedValue::from(true));
        assert!(!writable_directory_requested(&options));
        options.remove("multiple");
        options.insert("choices".into(), OwnedValue::from(false));
        assert!(!writable_directory_requested(&options));
    }

    #[tokio::test]
    async fn directory_consent_shows_trusted_write_intent_and_exports_only_selection() {
        let root = tempfile::tempdir().unwrap();
        let selected = root.path().join("import # destination");
        std::fs::create_dir(&selected).unwrap();
        let sibling = root.path().join("not selected");
        std::fs::create_dir(&sibling).unwrap();
        let caller = PortalCaller::new(WorkspaceId(39), "org.example");
        for (mode, selection) in [
            (FsMode::Read, Some(vec![selected.clone()])),
            (FsMode::ReadWrite, Some(vec![selected.clone()])),
            (FsMode::ReadWrite, None),
        ] {
            let queue = super::super::FilePickerQueue::new();
            let picker = FilePickerSpawnerHandle::new(Arc::new(queue.clone()));
            let mut options = destination_options(true);
            options.insert(
                "accept_label".into(),
                OwnedValue::try_from(Value::from("Open")).unwrap(),
            );
            let opts = decode_open_options(caller.clone(), "Open folder".into(), &options);
            assert!(opts.writable_directory);
            let write = mode == FsMode::ReadWrite;
            let cancelled = selection.is_none();
            let respond = async {
                tokio::task::yield_now().await;
                let request = queue.drain().pop().expect("actual shared picker request");
                assert!(request.directory);
                assert!(!request.save);
                assert!(!request.multiple);
                assert_eq!(request.writable_directory, write);
                if write {
                    assert_eq!(
                        request.display_title(),
                        crate::shell::i18n::t("portal-file-grant-write-folder")
                    );
                    assert_eq!(
                        request.confirm_label(),
                        crate::shell::i18n::t("portal-file-allow-write-folder")
                    );
                    assert_ne!(request.display_title(), "Open folder");
                    assert_ne!(request.confirm_label(), "Open");
                } else {
                    assert_eq!(request.confirm_label(), "Open");
                }
                request.reply.send(selection).unwrap();
            };
            let (response, ()) = tokio::join!(pick_and_grant(&picker, opts, mode), respond);
            let (code, result) = crate::portal::reply::reply(response);
            if cancelled {
                assert_eq!(code, 1);
                assert!(result.is_empty());
            } else {
                assert_eq!(code, 0);
                let uris = Vec::<String>::try_from(result["uris"].try_clone().unwrap()).unwrap();
                assert_eq!(uris, vec![file_uri(&selected).unwrap()]);
                assert!(!uris.contains(&file_uri(&sibling).unwrap()));
                assert_eq!(
                    crate::portal::option::<bool>(&result, "writable"),
                    Some(write)
                );
                assert_eq!(result.contains_key("choices"), write);
            }
        }
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
    fn writable_destination_rejects_files_and_multiple_selections() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("file");
        std::fs::write(&file, b"unchanged").unwrap();
        let caller = PortalCaller::new(WorkspaceId(40), "org.example");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        for paths in [vec![file.clone()], vec![directory.path().to_path_buf(); 2]] {
            let picker =
                FilePickerSpawnerHandle::new(Arc::new(MockFilePickerSpawner::returning(paths)));
            let options =
                FilePickerOptions::new(caller.clone(), "Import into folder", false).directory(true);
            let (code, result) = crate::portal::reply::reply(runtime.block_on(pick_and_grant(
                &picker,
                options,
                FsMode::ReadWrite,
            )));
            assert_eq!(code, 2);
            assert!(result.is_empty());
        }
        assert_eq!(std::fs::read(file).unwrap(), b"unchanged");
    }

    #[test]
    fn chooser_paths_map_to_portal_responses_and_encoded_uris() {
        // Phase `paths_allow_returns_response_zero_and_uri`.
        {
            let (code, results) = crate::portal::reply::reply(paths_to_open_response(
                Some(vec![PathBuf::from("/tmp/x.txt")]),
                /*save=*/ false,
            ));
            assert_eq!(code, 0);
            assert!(results.contains_key("uris"));
            assert_eq!(
                crate::portal::option::<bool>(&results, "writable"),
                Some(false)
            );
        }
        // Phase `paths_save_returns_writable_true`.
        {
            let (code, results) = crate::portal::reply::reply(paths_to_open_response(
                Some(vec![PathBuf::from("/tmp/out.bin")]),
                /*save=*/ true,
            ));
            assert_eq!(code, 0);
            assert!(results.contains_key("uris"));
            assert!(results.contains_key("writable"));
        }
        // Phase `paths_none_returns_response_one_no_results`.
        {
            let (code, results) =
                crate::portal::reply::reply(paths_to_open_response(None, /*save=*/ false));
            assert_eq!(code, 1);
            assert!(results.is_empty());
        }
        // Phase `paths_empty_returns_response_one`.
        {
            // Empty list ≡ cancel — keeps the contract symmetric with `None` (the spawner uses an empty Vec ≡ cancel internally).
            let (code, results) = crate::portal::reply::reply(paths_to_open_response(
                Some(vec![]),
                /*save=*/ false,
            ));
            assert_eq!(code, 1);
            assert!(results.is_empty());
        }
        // Phase `paths_serialise_to_file_uri`.
        {
            let (_code, results) = crate::portal::reply::reply(paths_to_open_response(
                Some(vec![PathBuf::from("/tmp/a.txt")]),
                false,
            ));
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
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn open_and_save_flows_grant_only_what_the_user_picked() {
        // Phase `open_file_returns_user_selected_uri`.
        // Wires a `MockFilePickerSpawner` returning `/tmp/test.txt`, calls the
        // backend's response builder, and asserts the response contains `file:///tmp/test.txt`.
        {
            let ws_id = WorkspaceId(33);
            let mock = Arc::new(MockFilePickerSpawner::returning(vec![PathBuf::from(
                "/tmp/test.txt",
            )]));
            let fp = FilePickerSpawnerHandle::new(mock.clone());
            let caller = PortalCaller::new(ws_id, "org.example");
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (code, results) = crate::portal::reply::reply(rt.block_on(pick_and_grant(
                &fp,
                FilePickerOptions::new(caller, "Open file", /*save=*/ false),
                FsMode::Read,
            )));
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
        // Workspace isolation is asserted against the backend-connected store
        // in portal_file_chooser_e2e, not an unattached unit-test store.
        // Phase `open_file_with_cancelled_spawner_returns_response_one`.
        {
            let ws_id = WorkspaceId(34);
            let fp = FilePickerSpawnerHandle::new(Arc::new(MockFilePickerSpawner::always_cancel()));
            let caller = PortalCaller::new(ws_id, "org.example");
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (code, results) = crate::portal::reply::reply(rt.block_on(pick_and_grant(
                &fp,
                FilePickerOptions::new(caller, "Open", false),
                FsMode::Read,
            )));
            assert_eq!(code, 1);
            assert!(results.is_empty());
        }
        // Phase `save_responses_are_writable_and_open_responses_are_not`.
        for (save, mode, expect_write) in [
            (false, FsMode::Read, false),
            (true, FsMode::ReadWrite, true),
        ] {
            let ws_id = WorkspaceId(38);
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
            let (code, result) = crate::portal::reply::reply(rt.block_on(pick_and_grant(
                &fp,
                FilePickerOptions::new(caller.clone(), "t", save),
                mode,
            )));
            assert_eq!(code, 0);
            let writable = crate::portal::option::<bool>(&result, "writable").unwrap_or(false);
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
