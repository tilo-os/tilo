//! File choosing through the Storage Access Framework.
//!
//! The system chooser answers with `content:` URIs, not paths, and the app
//! may not reach the chosen document through the filesystem at all. So:
//!
//! - [`FilePicker::open_file`] and [`FilePicker::save_file`] deliver the
//!   descriptor `ContentResolver.openFileDescriptor` opened, with the
//!   provider's display name. They are the complete Android picker.
//! - [`FilePicker::pick`] and [`FilePicker::pick_multiple`] (Open) copy each
//!   document into a private, read-only snapshot and answer with its path, so
//!   an app that reads by path works. The snapshot is not the document: a
//!   write to it fails with permission denied rather than being silently
//!   lost, and it is gone in the next process.
//! - A path-based Save and a folder chooser fail before any chooser is shown:
//!   a created document and a document tree have no path the app could write
//!   to or list.
//! - [`FilePicker::launch`] shares the already-open file through
//!   `TiloFileProvider` with a one-URI read grant; Android has no "reveal in
//!   folder" for an app's files.
//!
//! None of these take persistable URI permissions: every grant is used once,
//! immediately, while the chooser's temporary grant holds.

use std::fs::{File, Permissions};
use std::os::fd::{FromRawFd, IntoRawFd};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::Once;
use std::sync::atomic::{AtomicU64, Ordering};

use jni::JNIEnv;
use jni::objects::{JObject, JValue};

use super::bridge::{self, DOCUMENTS, optional_string};
use crate::host::FilePicker;
use crate::portal::{
    LaunchChannel, LaunchOutcome, LaunchRequest, MultiChannel, MultiOutcome, OpenFileChannel,
    OpenFileOutcome, OpenedFile, PickKind, PickRequest, PortalChannel, PortalOutcome,
};
use crate::saf::{self, DocumentResult};
use crate::wake::reply;

const SAVE_HAS_NO_PATH: &str =
    "android saves to a document, not a path; use `PickRequest::send_save_file`";
const TREE_HAS_NO_PATH: &str = "android has no folder path to give the app";
const REVEAL_UNSUPPORTED: &str = "android cannot reveal an app's file in a folder";
const OPEN_FILE_ONLY: &str = "an already-open file request must be a single-file Open chooser";
const SAVE_FILE_ONLY: &str = "an already-open save request must be a Save chooser";

pub(super) struct DocumentPicker;

impl FilePicker for DocumentPicker {
    fn pick(&self, request: PickRequest) -> PortalChannel {
        let (tx, rx) = reply();
        match request.kind() {
            PickKind::Open { .. } => choose_documents(&request, false, move |result| {
                tx.send(match result {
                    DocumentResult::Picked(uris) => match uris.first().map(|uri| snapshot(uri)) {
                        Some(Ok(path)) => PortalOutcome::Opened(path),
                        Some(Err(error)) => PortalOutcome::Failed(error),
                        None => PortalOutcome::Cancelled,
                    },
                    DocumentResult::Cancelled => PortalOutcome::Cancelled,
                    DocumentResult::Failed(error) => PortalOutcome::Failed(error),
                });
            }),
            PickKind::Save { .. } => {
                tx.send(PortalOutcome::Failed(SAVE_HAS_NO_PATH.to_string()));
            }
            PickKind::Directory => {
                tx.send(PortalOutcome::Failed(TREE_HAS_NO_PATH.to_string()));
            }
        }
        rx
    }

    fn pick_multiple(&self, request: PickRequest) -> MultiChannel {
        let (tx, rx) = reply();
        match request.kind() {
            PickKind::Open { .. } => choose_documents(&request, true, move |result| {
                tx.send(match result {
                    DocumentResult::Picked(uris) => snapshots(&uris),
                    DocumentResult::Cancelled => MultiOutcome::Cancelled,
                    DocumentResult::Failed(error) => MultiOutcome::Failed(error),
                });
            }),
            PickKind::Save { .. } => {
                tx.send(MultiOutcome::Failed(SAVE_HAS_NO_PATH.to_string()));
            }
            PickKind::Directory => {
                tx.send(MultiOutcome::Failed(TREE_HAS_NO_PATH.to_string()));
            }
        }
        rx
    }

    fn open_file(&self, request: PickRequest) -> OpenFileChannel {
        let (tx, rx) = reply();
        if !matches!(request.kind(), PickKind::Open { multiple: false }) {
            tx.send(OpenFileOutcome::Failed(OPEN_FILE_ONLY.to_string()));
            return rx;
        }
        choose_documents(&request, false, move |result| {
            tx.send(match result {
                DocumentResult::Picked(uris) => match uris.first() {
                    Some(uri) => open_document(uri, "r"),
                    None => OpenFileOutcome::Cancelled,
                },
                DocumentResult::Cancelled => OpenFileOutcome::Cancelled,
                DocumentResult::Failed(error) => OpenFileOutcome::Failed(error),
            });
        });
        rx
    }

    fn save_file(&self, request: PickRequest) -> OpenFileChannel {
        let (tx, rx) = reply();
        let Some(suggested_name) = request.suggested_name() else {
            tx.send(OpenFileOutcome::Failed(SAVE_FILE_ONLY.to_string()));
            return rx;
        };
        let name = saf::document_file_name(Some(suggested_name));
        let filters = request.filters().to_vec();
        let thread_name = request.worker_name().to_string();
        let finish = move |result| {
            tx.send(match result {
                DocumentResult::Picked(uris) => match uris.first() {
                    Some(uri) => open_document(uri, "w"),
                    None => OpenFileOutcome::Cancelled,
                },
                DocumentResult::Cancelled => OpenFileOutcome::Cancelled,
                DocumentResult::Failed(error) => OpenFileOutcome::Failed(error),
            });
        };
        choose(
            thread_name,
            move |env, activity| {
                let documents = bridge::class(env, activity, DOCUMENTS)?;
                let mime = saf::save_mime_type(&filters, &name, |extension| {
                    mime_type_for_extension(env, &documents, extension)
                });
                let mime = env.new_string(mime)?;
                let name = env.new_string(&name)?;
                env.call_static_method(
                    &documents,
                    "createIntent",
                    "(Ljava/lang/String;Ljava/lang/String;)Landroid/content/Intent;",
                    &[JValue::Object(&mime), JValue::Object(&name)],
                )?
                .l()
            },
            finish,
        );
        rx
    }

    fn launch(&self, request: LaunchRequest) -> LaunchChannel {
        let (tx, rx) = reply();
        if request.is_reveal() {
            tx.send(LaunchOutcome::Failed(REVEAL_UNSUPPORTED.to_string()));
            return rx;
        }
        let thread_name = request.worker_name().to_string();
        crate::portal::spawn_worker(&thread_name, move || {
            tx.send(view(&request));
        });
        rx
    }
}

/// Show `ACTION_OPEN_DOCUMENT` for `request`; `finish` runs on a worker
/// thread with the answer.
fn choose_documents(
    request: &PickRequest,
    multiple: bool,
    finish: impl FnOnce(DocumentResult) + Send + 'static,
) {
    let filters = request.filters().to_vec();
    choose(
        request.worker_name().to_string(),
        move |env, activity| {
            let documents = bridge::class(env, activity, DOCUMENTS)?;
            let types = saf::open_mime_types(&filters, |extension| {
                mime_type_for_extension(env, &documents, extension)
            });
            let types = bridge::string_array(env, &types)?;
            env.call_static_method(
                &documents,
                "openIntent",
                "([Ljava/lang/String;Z)Landroid/content/Intent;",
                &[JValue::Object(&types), JValue::Bool(multiple.into())],
            )?
            .l()
        },
        finish,
    );
}

/// Start the chooser `intent` builds. The answer arrives on the UI thread;
/// reading the documents is I/O, so `finish` runs on `thread_name`.
fn choose(
    thread_name: String,
    intent: impl for<'local> FnOnce(
        &mut JNIEnv<'local>,
        &JObject<'_>,
    ) -> jni::errors::Result<JObject<'local>>,
    finish: impl FnOnce(DocumentResult) + Send + 'static,
) {
    bridge::start_for_result(intent, move |result| {
        let result = saf::document_result(result.code, result.uris, result.error);
        crate::portal::spawn_worker(&thread_name, move || finish(result));
    });
}

fn mime_type_for_extension(
    env: &mut JNIEnv<'_>,
    documents: &jni::objects::JClass<'_>,
    extension: &str,
) -> Option<String> {
    let lookup = |env: &mut JNIEnv<'_>| {
        let extension = env.new_string(extension)?;
        let mime = env
            .call_static_method(
                documents,
                "mimeTypeForExtension",
                "(Ljava/lang/String;)Ljava/lang/String;",
                &[JValue::Object(&extension)],
            )?
            .l()?;
        optional_string(env, mime)
    };
    let mime = lookup(env).ok().flatten();
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
    mime
}

/// The document's display name and a descriptor for it opened in `mode`.
fn open_descriptor(uri: &str, mode: &str) -> Result<(Option<String>, File), String> {
    let (name, fd) = super::with_activity(|env, activity| {
        let documents = bridge::class(env, activity, DOCUMENTS)?;
        let uri = env.new_string(uri)?;
        let name = env
            .call_static_method(
                &documents,
                "displayName",
                "(Landroid/content/Context;Ljava/lang/String;)Ljava/lang/String;",
                &[JValue::Object(activity), JValue::Object(&uri)],
            )?
            .l()?;
        let name = optional_string(env, name)?;
        let mode = env.new_string(mode)?;
        let fd = env
            .call_static_method(
                &documents,
                "openFd",
                "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;)I",
                &[
                    JValue::Object(activity),
                    JValue::Object(&uri),
                    JValue::Object(&mode),
                ],
            )?
            .i()?;
        Ok((name, fd))
    })?;
    if fd < 0 {
        return Err("the provider returned no descriptor".to_string());
    }
    // SAFETY: `detachFd` handed this descriptor's ownership to the caller.
    Ok((name, unsafe { File::from_raw_fd(fd) }))
}

/// Deliver the chosen document as an open file. Like the desktop portal, only
/// a regular file is handed over.
fn open_document(uri: &str, mode: &str) -> OpenFileOutcome {
    let (name, file) = match open_descriptor(uri, mode) {
        Ok(opened) => opened,
        Err(error) => {
            return OpenFileOutcome::Failed(format!("could not open the chosen document: {error}"));
        }
    };
    match file.metadata() {
        Ok(metadata) if metadata.file_type().is_file() => OpenFileOutcome::Opened(OpenedFile::new(
            file,
            name.unwrap_or_else(|| "selected file".to_string()),
        )),
        Ok(_) => OpenFileOutcome::Failed("the chosen document is not a regular file".to_string()),
        Err(error) => {
            OpenFileOutcome::Failed(format!("could not inspect the chosen document: {error}"))
        }
    }
}

fn snapshots(uris: &[String]) -> MultiOutcome {
    let mut paths = Vec::new();
    let mut last_error = None;
    for uri in uris {
        match snapshot(uri) {
            Ok(path) => paths.push(path),
            Err(error) => {
                tracing::warn!(target: "tilo_ui::portal", %error, "a chosen document could not be read");
                last_error = Some(error);
            }
        }
    }
    if paths.is_empty() {
        return MultiOutcome::Failed(
            last_error.unwrap_or_else(|| "the chooser returned no document".to_string()),
        );
    }
    MultiOutcome::Selected(paths)
}

/// Copy the document at `uri` into a fresh read-only directory of the app's
/// cache and return the copy's path.
fn snapshot(uri: &str) -> Result<PathBuf, String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let (name, mut source) = open_descriptor(uri, "r")
        .map_err(|error| format!("could not open the chosen document: {error}"))?;
    let root = snapshot_root()?;
    let dir = root.join(format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .map_err(|error| format!("could not stage the chosen document: {error}"))?;
    let path = dir.join(saf::document_file_name(name.as_deref()));
    let copied = File::create(&path).and_then(|mut copy| {
        std::io::copy(&mut source, &mut copy)?;
        copy.sync_all()
    });
    if let Err(error) = copied {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(format!("could not copy the chosen document: {error}"));
    }
    std::fs::set_permissions(&path, Permissions::from_mode(0o444))
        .and_then(|()| std::fs::set_permissions(&dir, Permissions::from_mode(0o555)))
        .map_err(|error| format!("could not seal the copy of the chosen document: {error}"))?;
    Ok(path)
}

/// `<cache>/tilo-picked`, emptied of earlier processes' snapshots the first
/// time this process uses it.
fn snapshot_root() -> Result<PathBuf, String> {
    static SWEEP: Once = Once::new();
    let cache = super::with_activity(|env, activity| {
        let dir = env
            .call_method(activity, "getCacheDir", "()Ljava/io/File;", &[])?
            .l()?;
        let path = env
            .call_method(&dir, "getAbsolutePath", "()Ljava/lang/String;", &[])?
            .l()?;
        optional_string(env, path)
    })?
    .ok_or_else(|| "the app has no cache directory".to_string())?;
    let root = PathBuf::from(cache).join("tilo-picked");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)
        .map_err(|error| format!("could not create {}: {error}", root.display()))?;
    SWEEP.call_once(|| {
        let ours = format!("{}-", std::process::id());
        let Ok(entries) = std::fs::read_dir(&root) else {
            return;
        };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&ours) {
                continue;
            }
            let path = entry.path();
            let _ = std::fs::set_permissions(&path, Permissions::from_mode(0o700));
            let _ = std::fs::remove_dir_all(&path);
        }
    });
    Ok(root)
}

/// Hand the file to the app the user chose for its type, read-only.
fn view(request: &LaunchRequest) -> LaunchOutcome {
    let path = request.path();
    let file = match crate::portal::open_regular_file(path) {
        Ok(file) => file,
        Err(error) => {
            return LaunchOutcome::Failed(format!("could not open {}: {error}", path.display()));
        }
    };
    let name = saf::document_file_name(
        path.file_name()
            .map(|name| name.to_string_lossy())
            .as_deref(),
    );
    let fd = file.into_raw_fd();
    let shared = super::with_activity(|env, activity| {
        let documents = bridge::class(env, activity, DOCUMENTS)?;
        let name = env.new_string(&name)?;
        env.call_static_method(
            &documents,
            "view",
            "(Landroid/content/Context;ILjava/lang/String;)V",
            &[
                JValue::Object(activity),
                JValue::Int(fd),
                JValue::Object(&name),
            ],
        )?;
        Ok(())
    });
    match shared {
        // The provider serves the receiving app from this descriptor for as
        // long as the process lives.
        Ok(()) => LaunchOutcome::Opened,
        Err(error) => {
            // SAFETY: the provider did not take the descriptor; it is still ours.
            drop(unsafe { File::from_raw_fd(fd) });
            LaunchOutcome::Failed(error)
        }
    }
}
