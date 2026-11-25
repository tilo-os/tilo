//! The native side of Android's documents API: `so.tilo.android.TiloWorkspaceFiles` asks the
//! phone's device daemon what the person's workspaces hold, and it reads and writes their files.
//!
//! The provider may be the first thing the system starts in this process (the Files app browsed
//! Tilo without the app ever opening a window), so [`Java_so_tilo_android_TiloWorkspaceFiles_nativeStart`]
//! puts the app's directories in the environment and starts the same worker the view asks, and
//! every other call runs on that worker's thread through [`crate::files`].
//!
//! Every method here answers JSON for what Java reads as an object (a root, a document), and a
//! thrown `java.io.IOException` for what a person may see as a refusal.

use std::path::PathBuf;

use jni::JNIEnv;
use jni::objects::{JByteArray, JClass, JObject, JObjectArray, JString};
use jni::sys::{jboolean, jbyteArray, jstring};
use serde::Serialize;

use crate::files;
use crate::workspace_files::{self, Entry};
use tilo_runtime::in_process::{Device, FileKind};

/// What Java reads of one root: one of the person's workspaces.
#[derive(Serialize)]
struct Root<'a> {
    id: &'a str,
    name: &'a str,
}

/// What Java reads of one document of a workspace.
#[derive(Serialize)]
struct Document<'a> {
    id: String,
    name: &'a str,
    mime: &'a str,
    size: u64,
    modified: i64,
    directory: bool,
}

fn document<'a>(workspace: &str, entry: &'a Entry) -> Document<'a> {
    Document {
        id: workspace_files::document_id(workspace, &entry.path),
        name: &entry.name,
        mime: &entry.mime,
        size: entry.size,
        modified: entry.modified_unix_seconds,
        directory: entry.kind == FileKind::Directory,
    }
}

fn text(env: &mut JNIEnv<'_>, value: &JString<'_>) -> Result<String, String> {
    if value.is_null() {
        return Err("a required string was null".to_owned());
    }
    env.get_string(value)
        .map(|text| text.into())
        .map_err(|error| error.to_string())
}

/// Throw `java.io.IOException(message)`, which Java's `openFile` and friends already declare.
fn refuse(env: &mut JNIEnv<'_>, message: String) {
    if let Err(error) = env.throw_new("java/io/IOException", message) {
        tracing::error!(%error, "a refusal could not be handed to Java");
    }
}

fn json<T: Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|error| error.to_string())
}

/// The phone's device, asked for the answer `work` gives on its own thread.
fn ask<T: Send + 'static>(
    work: impl FnOnce(&mut Device) -> T + Send + 'static,
) -> Result<T, String> {
    files()?.ask(work)
}

/// A Java array of the strings `rows`.
fn strings<'local>(
    env: &mut JNIEnv<'local>,
    rows: &[String],
) -> jni::errors::Result<JObjectArray<'local>> {
    let length = i32::try_from(rows.len()).unwrap_or(i32::MAX);
    let array = env.new_object_array(length, "java/lang/String", JObject::null())?;
    for (index, row) in rows.iter().enumerate() {
        let Ok(index) = i32::try_from(index) else {
            break;
        };
        if index >= length {
            break;
        }
        let row = env.new_string(row)?;
        env.set_object_array_element(&array, index, row)?;
    }
    Ok(array)
}

/// The app's files directory, which `Context.getFilesDir()` answers.
fn files_dir(env: &mut JNIEnv<'_>, context: &JObject<'_>) -> Result<PathBuf, String> {
    let dir = env
        .call_method(context, "getFilesDir", "()Ljava/io/File;", &[])
        .and_then(|dir| dir.l())
        .map_err(|error| format!("the app's files directory: {error}"))?;
    let path = env
        .call_method(&dir, "getAbsolutePath", "()Ljava/lang/String;", &[])
        .and_then(|path| path.l())
        .map_err(|error| format!("the app's files directory: {error}"))?;
    let path: String = env
        .get_string(&JString::from(path))
        .map_err(|error| error.to_string())?
        .into();
    Ok(PathBuf::from(path))
}

/// Starts the phone's device for the provider, and answers whether it runs. The system may start
/// this process for the provider alone, so the app's directories go into the environment here,
/// exactly as the activity's own start puts them.
#[unsafe(no_mangle)]
pub extern "system" fn Java_so_tilo_android_TiloWorkspaceFiles_nativeStart(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    context: JObject<'_>,
) -> jboolean {
    let started = (|| -> Result<(), String> {
        let dir = files_dir(&mut env, &context)?;
        // The activity sets these too, unless it already did: a process its window started reads
        // one environment for the daemon and the provider alike.
        if std::env::var_os("HOME").is_none() {
            ui::launch::android::install_environment_in(&dir);
        }
        files().map(drop)
    })();
    match started {
        Ok(()) => jboolean::from(true),
        Err(why) => {
            tracing::error!(%why, "the phone's device did not start for its file provider");
            jboolean::from(false)
        }
    }
}

/// The person's workspaces, one JSON object each.
#[unsafe(no_mangle)]
pub extern "system" fn Java_so_tilo_android_TiloWorkspaceFiles_nativeRoots(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
) -> jni::sys::jobjectArray {
    let roots = match ask(|device| workspace_files::roots(device)) {
        Ok(Ok(roots)) => roots,
        Ok(Err(why)) | Err(why) => {
            tracing::warn!(%why, "the person's workspaces are not read for the file provider");
            return std::ptr::null_mut();
        }
    };
    let rows: Vec<String> = roots
        .iter()
        .filter_map(|workspace| {
            json(&Root {
                id: &workspace.key,
                name: &workspace.name,
            })
            .ok()
        })
        .collect();
    strings(&mut env, &rows).map_or_else(
        |error| {
            tracing::error!(%error, "the roots do not reach Java");
            std::ptr::null_mut()
        },
        JObjectArray::into_raw,
    )
}

/// The document `document_id` names, as one JSON object: null when nothing is there.
#[unsafe(no_mangle)]
pub extern "system" fn Java_so_tilo_android_TiloWorkspaceFiles_nativeDocument(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    document_id: JString<'_>,
) -> jstring {
    let found = (|| -> Result<Option<String>, String> {
        let id = text(&mut env, &document_id)?;
        // A root's document is the workspace itself, which the tree has no node for.
        if workspace_files::is_workspace(&id) {
            let workspace = ask({
                let id = id.clone();
                move |device| {
                    workspace_files::roots(device)
                        .map(|roots| roots.into_iter().find(|workspace| workspace.key == id))
                }
            })??;
            return workspace
                .map(|workspace| {
                    json(&Document {
                        id: id.clone(),
                        name: &workspace.name,
                        mime: workspace_files::DIRECTORY_MIME,
                        size: 0,
                        modified: 0,
                        directory: true,
                    })
                })
                .transpose();
        }
        let (workspace, path) = workspace_files::split_document_id(&id)?;
        let entry = ask(move |device| workspace_files::stat(device, &workspace, &path))??;
        let (workspace, _) = workspace_files::split_document_id(&id)?;
        entry
            .as_ref()
            .map(|entry| json(&document(&workspace, entry)))
            .transpose()
    })();
    match found {
        Ok(Some(row)) => env.new_string(row).map_or_else(
            |error| {
                refuse(
                    &mut env,
                    format!("the document does not reach Java: {error}"),
                );
                std::ptr::null_mut()
            },
            JString::into_raw,
        ),
        Ok(None) => std::ptr::null_mut(),
        Err(why) => {
            refuse(&mut env, why);
            std::ptr::null_mut()
        }
    }
}

/// What the folder `parent_document_id` holds, one JSON object each.
#[unsafe(no_mangle)]
pub extern "system" fn Java_so_tilo_android_TiloWorkspaceFiles_nativeChildren(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    parent_document_id: JString<'_>,
) -> jni::sys::jobjectArray {
    let listed = (|| -> Result<Vec<String>, String> {
        let id = text(&mut env, &parent_document_id)?;
        let (workspace, folder) = workspace_files::split_document_id(&id)?;
        let entries = {
            let (workspace, folder) = (workspace.clone(), folder.clone());
            ask(move |device| workspace_files::list(device, &workspace, &folder))??
        };
        entries
            .iter()
            .map(|entry| json(&document(&workspace, entry)))
            .collect()
    })();
    match listed {
        Ok(rows) => strings(&mut env, &rows).map_or_else(
            |error| {
                tracing::error!(%error, "a folder's entries do not reach Java");
                std::ptr::null_mut()
            },
            JObjectArray::into_raw,
        ),
        Err(why) => {
            refuse(&mut env, why);
            std::ptr::null_mut()
        }
    }
}

/// The bytes of the file `document_id` names.
#[unsafe(no_mangle)]
pub extern "system" fn Java_so_tilo_android_TiloWorkspaceFiles_nativeRead(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    document_id: JString<'_>,
) -> jbyteArray {
    let read = (|| -> Result<Vec<u8>, String> {
        let id = text(&mut env, &document_id)?;
        let (workspace, path) = workspace_files::split_document_id(&id)?;
        ask(move |device| workspace_files::read(device, &workspace, &path))?
    })();
    match read {
        Ok(bytes) => env.byte_array_from_slice(&bytes).map_or_else(
            |error| {
                refuse(&mut env, format!("the bytes do not reach Java: {error}"));
                std::ptr::null_mut()
            },
            JByteArray::into_raw,
        ),
        Err(why) => {
            refuse(&mut env, why);
            std::ptr::null_mut()
        }
    }
}

/// Write `bytes` at the file `document_id` names.
#[unsafe(no_mangle)]
pub extern "system" fn Java_so_tilo_android_TiloWorkspaceFiles_nativeWrite(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    document_id: JString<'_>,
    bytes: JByteArray<'_>,
) {
    let written = (|| -> Result<(), String> {
        let id = text(&mut env, &document_id)?;
        let bytes = env
            .convert_byte_array(&bytes)
            .map_err(|error| error.to_string())?;
        let (workspace, path) = workspace_files::split_document_id(&id)?;
        ask(move |device| workspace_files::write(device, &workspace, &path, &bytes))?
    })();
    if let Err(why) = written {
        refuse(&mut env, why);
    }
}

/// Make a file or a folder named `name` in `parent_document_id`, and answer its document id.
#[unsafe(no_mangle)]
pub extern "system" fn Java_so_tilo_android_TiloWorkspaceFiles_nativeCreate(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    parent_document_id: JString<'_>,
    name: JString<'_>,
    directory: jboolean,
) -> jstring {
    let created = (|| -> Result<String, String> {
        let id = text(&mut env, &parent_document_id)?;
        let name = text(&mut env, &name)?;
        let (workspace, folder) = workspace_files::split_document_id(&id)?;
        let made = {
            let (workspace, folder, name) = (workspace.clone(), folder.clone(), name.clone());
            ask(move |device| {
                workspace_files::create(device, &workspace, &folder, &name, directory != 0)
            })??
        };
        Ok(workspace_files::document_id(&workspace, &made.path))
    })();
    match created {
        Ok(document_id) => env.new_string(document_id).map_or_else(
            |error| {
                refuse(
                    &mut env,
                    format!("the new document does not reach Java: {error}"),
                );
                std::ptr::null_mut()
            },
            JString::into_raw,
        ),
        Err(why) => {
            refuse(&mut env, why);
            std::ptr::null_mut()
        }
    }
}

/// Take the document `document_id` names out of the workspace.
#[unsafe(no_mangle)]
pub extern "system" fn Java_so_tilo_android_TiloWorkspaceFiles_nativeDelete(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    document_id: JString<'_>,
) {
    let deleted = (|| -> Result<(), String> {
        let id = text(&mut env, &document_id)?;
        let (workspace, path) = workspace_files::split_document_id(&id)?;
        ask(move |device| workspace_files::delete(device, &workspace, &path))?
    })();
    if let Err(why) = deleted {
        refuse(&mut env, why);
    }
}

/// Give the document `document_id` names the name `name`, and answer its new document id.
#[unsafe(no_mangle)]
pub extern "system" fn Java_so_tilo_android_TiloWorkspaceFiles_nativeRename(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    document_id: JString<'_>,
    name: JString<'_>,
) -> jstring {
    let renamed = (|| -> Result<String, String> {
        let id = text(&mut env, &document_id)?;
        let name = text(&mut env, &name)?;
        let (workspace, path) = workspace_files::split_document_id(&id)?;
        ask(move |device| workspace_files::rename(device, &workspace, &path, &name))?
    })();
    match renamed {
        Ok(document_id) => env.new_string(document_id).map_or_else(
            |error| {
                refuse(
                    &mut env,
                    format!("the renamed document does not reach Java: {error}"),
                );
                std::ptr::null_mut()
            },
            JString::into_raw,
        ),
        Err(why) => {
            refuse(&mut env, why);
            std::ptr::null_mut()
        }
    }
}

/// A token that changes whenever the workspace's files do, for the provider's change watch.
#[unsafe(no_mangle)]
pub extern "system" fn Java_so_tilo_android_TiloWorkspaceFiles_nativeRevision(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    workspace_id: JString<'_>,
) -> jstring {
    let revision = (|| -> Result<String, String> {
        let workspace = text(&mut env, &workspace_id)?;
        ask(move |device| workspace_files::revision(device, &workspace))?
    })();
    match revision {
        Ok(token) => env
            .new_string(token)
            .map_or_else(|_| std::ptr::null_mut(), JString::into_raw),
        // A workspace not read yet, or the device still starting: the provider watches again.
        Err(_) => std::ptr::null_mut(),
    }
}
