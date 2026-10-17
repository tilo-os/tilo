//! The browser's origin-private file system (OPFS), through the async API the
//! page's main thread may use, and Web Locks.
//!
//! Layout: `opfs://<profile>/<path>` is the directory `<profile>` at the top
//! of the origin's OPFS, then the path's components.
//!
//! - A replacement writes through `createWritable()`, whose swap file the
//!   browser commits atomically on `close()`. A new file's name appears,
//!   empty, before its first commit.
//! - Every operation that creates, replaces or removes names holds the
//!   profile's Web Lock, so no-clobber creation and renames hold against every
//!   `tilo-files` writer in every tab of the origin.
//! - A rename uses `FileSystemHandle.move()` where the browser has it, and
//!   otherwise copies, then removes, the file or directory tree. Chrome has
//!   `move()` only on file handles, so a directory is always copied there.
//! - OPFS keeps no directory modification times; they read as the epoch.

use js_sys::{Function, Object, Promise, Reflect, Uint8Array};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::wasm_bindgen;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    File, FileSystemCreateWritableOptions, FileSystemDirectoryHandle, FileSystemFileHandle,
    FileSystemGetDirectoryOptions, FileSystemGetFileOptions, FileSystemHandle,
    FileSystemHandleKind, FileSystemRemoveOptions, FileSystemWritableFileStream, StorageManager,
};

use crate::{Entry, Feature, FsError, Kind, LockMode, Meta, VPath};

const NO_OPFS: &str = "This browser has no origin-private file system.";
const NO_WEB_LOCKS: &str = "This browser has no Web Locks, or the page is not a secure context.";
const ONE_COMPONENT: &str = "a storage profile is one path component";

#[wasm_bindgen]
extern "C" {
    /// `navigator.locks`; `web-sys` 0.3.85 keeps its binding unstable.
    type LockManager;

    #[wasm_bindgen(catch, method, js_name = request)]
    fn request(
        this: &LockManager,
        name: &str,
        options: &Object,
        callback: &JsValue,
    ) -> Result<LockRequest, JsValue>;

    /// The promise `request` returns, settled when the lock is released or
    /// refused.
    type LockRequest;

    #[wasm_bindgen(method, js_name = then)]
    fn then_or_else(this: &LockRequest, on_fulfilled: &JsValue, on_rejected: &Function);
}

/// A [`Dir`](crate::Dir) in OPFS.
#[derive(Clone, Debug)]
pub(crate) struct Scope {
    profile: String,
    prefix: VPath,
}

/// A granted Web Lock, released when dropped.
pub(crate) struct WebLock {
    release: Function,
}

impl Drop for WebLock {
    fn drop(&mut self) {
        let _ = self.release.call0(&JsValue::UNDEFINED);
    }
}

impl Scope {
    pub(crate) fn new(profile: String, prefix: VPath) -> Result<Self, FsError> {
        match VPath::new(&profile) {
            Ok(path) if path.components().count() == 1 => Ok(Self { profile, prefix }),
            _ => Err(FsError::InvalidPath(ONE_COMPONENT)),
        }
    }

    fn full(&self, path: &VPath) -> VPath {
        self.prefix.join(path)
    }

    pub(crate) fn profile(&self) -> &str {
        &self.profile
    }

    pub(crate) fn prefix(&self) -> &VPath {
        &self.prefix
    }

    /// The whole file and its modification time.
    pub(crate) async fn read_with_modified(
        &self,
        path: &VPath,
    ) -> Result<(Vec<u8>, tilo_clock::SystemTime), FsError> {
        let full = self.full(path);
        let parts: Vec<&str> = full.components().collect();
        let Some((name, parents)) = parts.split_last() else {
            return Err(FsError::IsADirectory);
        };
        let handle = file_handle(&self.dir(parents, false).await?, name, false).await?;
        let file: File = await_js(handle.get_file(), FsError::IsADirectory)
            .await?
            .unchecked_into();
        let buffer = await_js(file.array_buffer(), FsError::IsADirectory).await?;
        let modified = tilo_clock::UNIX_EPOCH
            .checked_add(tilo_clock::Duration::from_millis(whole(
                file.last_modified(),
            )))
            .unwrap_or(tilo_clock::UNIX_EPOCH);
        Ok((Uint8Array::new(&buffer).to_vec(), modified))
    }

    /// The directory at `parts` below the scope's profile.
    async fn dir(
        &self,
        parts: &[&str],
        create: bool,
    ) -> Result<FileSystemDirectoryHandle, FsError> {
        let mut dir = child_dir(&storage_root().await?, &self.profile, create).await?;
        for part in parts {
            dir = child_dir(&dir, part, create).await?;
        }
        Ok(dir)
    }

    /// Serializes every writer of names in this profile, across tabs.
    async fn names(&self) -> Result<WebLock, FsError> {
        acquire(
            &format!("tilo-files-names:{}", self.profile),
            LockMode::Exclusive,
        )
        .await
    }

    pub(crate) async fn read(&self, path: &VPath) -> Result<Vec<u8>, FsError> {
        let full = self.full(path);
        let parts: Vec<&str> = full.components().collect();
        let Some((name, parents)) = parts.split_last() else {
            return Err(FsError::IsADirectory);
        };
        let file = file_handle(&self.dir(parents, false).await?, name, false).await?;
        read_file(&file).await
    }

    pub(crate) async fn write_atomic(&self, path: &VPath, bytes: &[u8]) -> Result<(), FsError> {
        let full = self.full(path);
        let parts: Vec<&str> = full.components().collect();
        let Some((name, parents)) = parts.split_last() else {
            return Err(FsError::InvalidPath(crate::BELOW_ROOT));
        };
        let _names = self.names().await?;
        let file = file_handle(&self.dir(parents, true).await?, name, true).await?;
        write_file(&file, bytes).await
    }

    /// Reload and replace while holding the same lock as every profile writer.
    pub(crate) async fn update_atomic<F, Fut>(&self, path: &VPath, update: F) -> Result<(), FsError>
    where
        F: FnOnce(Option<Vec<u8>>) -> Fut,
        Fut: std::future::Future<Output = Result<Vec<u8>, FsError>>,
    {
        let _names = self.names().await?;
        let previous = match self.read(path).await {
            Ok(bytes) => Some(bytes),
            Err(FsError::NotFound) => None,
            Err(error) => return Err(error),
        };
        let existed = previous.is_some();
        let bytes = update(previous).await?;
        let full = self.full(path);
        let parts: Vec<&str> = full.components().collect();
        let Some((name, parents)) = parts.split_last() else {
            return Err(FsError::InvalidPath(crate::BELOW_ROOT));
        };
        let dir = self.dir(parents, true).await?;
        let file = file_handle(&dir, name, true).await?;
        if let Err(error) = write_file(&file, &bytes).await {
            // Creating a new handle exposes an empty file before close(). It
            // must not masquerade as a damaged existing vault on retry.
            if !existed {
                await_js(dir.remove_entry(name), FsError::NotADirectory).await?;
            }
            return Err(error);
        }
        crate::web::committed_directly(self, &crate::web::Direct::Write(path, &bytes));
        Ok(())
    }

    pub(crate) async fn create_new(&self, path: &VPath, bytes: &[u8]) -> Result<bool, FsError> {
        let full = self.full(path);
        let parts: Vec<&str> = full.components().collect();
        let Some((name, parents)) = parts.split_last() else {
            return Err(FsError::InvalidPath(crate::BELOW_ROOT));
        };
        let _names = self.names().await?;
        let dir = self.dir(parents, true).await?;
        if exists(&dir, name).await? {
            return Ok(false);
        }
        write_file(&file_handle(&dir, name, true).await?, bytes).await?;
        Ok(true)
    }

    pub(crate) async fn rename_noreplace(&self, from: &VPath, to: &VPath) -> Result<(), FsError> {
        let (from, to) = (self.full(from), self.full(to));
        let from_parts: Vec<&str> = from.components().collect();
        let to_parts: Vec<&str> = to.components().collect();
        let (Some((from_name, from_parents)), Some((to_name, to_parents))) =
            (from_parts.split_last(), to_parts.split_last())
        else {
            return Err(FsError::InvalidPath(crate::BELOW_ROOT));
        };
        let _names = self.names().await?;
        let from_dir = self.dir(from_parents, false).await?;
        let source: FileSystemHandle = match file_handle(&from_dir, from_name, false).await {
            Ok(file) => file.unchecked_into(),
            Err(FsError::IsADirectory) => child_dir(&from_dir, from_name, false)
                .await?
                .unchecked_into(),
            Err(error) => return Err(error),
        };
        let to_dir = self.dir(to_parents, true).await?;
        if exists(&to_dir, to_name).await? {
            return Err(FsError::AlreadyExists);
        }
        if let Some(move_to) = property(&source, "move").filter(JsValue::is_function) {
            let moved = Function::from(move_to)
                .call2(&source, &to_dir, &JsValue::from_str(to_name))
                .map_err(|error| js_error(&error, FsError::NotADirectory))?;
            await_js(moved.unchecked_into(), FsError::NotADirectory).await?;
            return Ok(());
        }
        if source.kind() == FileSystemHandleKind::File {
            let bytes = read_file(source.unchecked_ref()).await?;
            write_file(&file_handle(&to_dir, to_name, true).await?, &bytes).await?;
            await_js(from_dir.remove_entry(from_name), FsError::NotADirectory).await?;
            return Ok(());
        }
        // Chrome moves only file handles. A directory is copied whole, then
        // the source removed; an interrupted move leaves both trees, never
        // neither.
        let copied = child_dir(&to_dir, to_name, true).await?;
        if let Err(error) = copy_tree(source.unchecked_ref(), &copied).await {
            let _ = remove_tree(&to_dir, to_name).await;
            return Err(error);
        }
        remove_tree(&from_dir, from_name).await
    }

    pub(crate) async fn remove(&self, path: &VPath) -> Result<(), FsError> {
        let full = self.full(path);
        let parts: Vec<&str> = full.components().collect();
        let Some((name, parents)) = parts.split_last() else {
            return Err(FsError::InvalidPath(crate::BELOW_ROOT));
        };
        let _names = self.names().await?;
        let dir = self.dir(parents, false).await?;
        await_js(dir.remove_entry(name), FsError::NotADirectory).await?;
        Ok(())
    }

    pub(crate) async fn create_dir_all(&self, path: &VPath) -> Result<(), FsError> {
        let full = self.full(path);
        let parts: Vec<&str> = full.components().collect();
        self.dir(&parts, true).await.map(drop)
    }

    pub(crate) async fn list(&self, path: &VPath) -> Result<Vec<Entry>, FsError> {
        let full = self.full(path);
        let parts: Vec<&str> = full.components().collect();
        let dir = self.dir(&parts, false).await?;
        let mut entries: Vec<Entry> = children(&dir)
            .await?
            .into_iter()
            .map(|handle| Entry {
                name: handle.name(),
                kind: kind_of(&handle),
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    pub(crate) async fn metadata(&self, path: &VPath) -> Result<Meta, FsError> {
        let full = self.full(path);
        let parts: Vec<&str> = full.components().collect();
        let Some((name, parents)) = parts.split_last() else {
            self.dir(&[], false).await?;
            return Ok(directory_meta());
        };
        let dir = self.dir(parents, false).await?;
        match file_handle(&dir, name, false).await {
            Ok(file) => {
                let file: File = await_js(file.get_file(), FsError::IsADirectory)
                    .await?
                    .unchecked_into();
                Ok(Meta {
                    len: whole(file.size()),
                    modified: tilo_clock::UNIX_EPOCH
                        .checked_add(tilo_clock::Duration::from_millis(whole(
                            file.last_modified(),
                        )))
                        .unwrap_or(tilo_clock::UNIX_EPOCH),
                    kind: Kind::File,
                    mode: None,
                    symlink: false,
                })
            }
            Err(FsError::IsADirectory) => {
                child_dir(&dir, name, false).await?;
                Ok(directory_meta())
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn lock(&self, path: &VPath, mode: LockMode) -> Result<WebLock, FsError> {
        let full = self.full(path);
        let parts: Vec<&str> = full.components().collect();
        let Some((name, parents)) = parts.split_last() else {
            return Err(FsError::InvalidPath(crate::BELOW_ROOT));
        };
        file_handle(&self.dir(parents, true).await?, name, true).await?;
        acquire(&format!("tilo-files:{}/{full}", self.profile), mode).await
    }
}

fn directory_meta() -> Meta {
    Meta {
        len: 0,
        modified: tilo_clock::UNIX_EPOCH,
        kind: Kind::Dir,
        mode: None,
        symlink: false,
    }
}

/// A non-negative whole number of bytes or milliseconds from JavaScript.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "file sizes and timestamps are whole, non-negative and far below 2^53"
)]
fn whole(value: f64) -> u64 {
    if value.is_finite() && value > 0.0 {
        value as u64
    } else {
        0
    }
}

fn property(target: &JsValue, name: &str) -> Option<JsValue> {
    Reflect::get(target, &JsValue::from_str(name))
        .ok()
        .filter(|value| !value.is_undefined() && !value.is_null())
}

/// The origin's OPFS root.
async fn storage_root() -> Result<FileSystemDirectoryHandle, FsError> {
    let storage = property(&js_sys::global(), "navigator")
        .and_then(|navigator| property(&navigator, "storage"))
        .filter(|storage| property(storage, "getDirectory").is_some_and(|get| get.is_function()));
    let Some(storage) = storage else {
        return Err(FsError::Unsupported {
            feature: Feature::Files,
            reason: NO_OPFS,
        });
    };
    let storage: StorageManager = storage.unchecked_into();
    Ok(await_js(storage.get_directory(), FsError::NotADirectory)
        .await?
        .unchecked_into())
}

async fn child_dir(
    dir: &FileSystemDirectoryHandle,
    name: &str,
    create: bool,
) -> Result<FileSystemDirectoryHandle, FsError> {
    let options = FileSystemGetDirectoryOptions::new();
    options.set_create(create);
    Ok(await_js(
        dir.get_directory_handle_with_options(name, &options),
        FsError::NotADirectory,
    )
    .await?
    .unchecked_into())
}

async fn file_handle(
    dir: &FileSystemDirectoryHandle,
    name: &str,
    create: bool,
) -> Result<FileSystemFileHandle, FsError> {
    let options = FileSystemGetFileOptions::new();
    options.set_create(create);
    Ok(await_js(
        dir.get_file_handle_with_options(name, &options),
        FsError::IsADirectory,
    )
    .await?
    .unchecked_into())
}

/// Every entry of `dir`, in the browser's order.
async fn children(dir: &FileSystemDirectoryHandle) -> Result<Vec<FileSystemHandle>, FsError> {
    let values = dir.values();
    let mut handles = Vec::new();
    loop {
        let next = values
            .next()
            .map_err(|error| js_error(&error, FsError::NotADirectory))?;
        let step = await_js(next, FsError::NotADirectory).await?;
        if property(&step, "done").is_some_and(|done| done.is_truthy()) {
            break;
        }
        let Some(value) = property(&step, "value") else {
            break;
        };
        handles.push(value.unchecked_into());
    }
    Ok(handles)
}

fn kind_of(handle: &FileSystemHandle) -> Kind {
    match handle.kind() {
        FileSystemHandleKind::File => Kind::File,
        FileSystemHandleKind::Directory => Kind::Dir,
        _ => Kind::Other,
    }
}

/// Copies every entry of `from` into the empty directory `to`.
async fn copy_tree(
    from: &FileSystemDirectoryHandle,
    to: &FileSystemDirectoryHandle,
) -> Result<(), FsError> {
    let mut pending = vec![(from.clone(), to.clone())];
    while let Some((from, to)) = pending.pop() {
        for handle in children(&from).await? {
            let name = handle.name();
            match kind_of(&handle) {
                Kind::File => {
                    let bytes = read_file(handle.unchecked_ref()).await?;
                    write_file(&file_handle(&to, &name, true).await?, &bytes).await?;
                }
                Kind::Dir => {
                    let copy = child_dir(&to, &name, true).await?;
                    pending.push((handle.unchecked_into(), copy));
                }
                Kind::Other => {}
            }
        }
    }
    Ok(())
}

/// Removes `name` from `parent` with everything below it.
async fn remove_tree(parent: &FileSystemDirectoryHandle, name: &str) -> Result<(), FsError> {
    let options = FileSystemRemoveOptions::new();
    options.set_recursive(true);
    await_js(
        parent.remove_entry_with_options(name, &options),
        FsError::NotADirectory,
    )
    .await
    .map(drop)
}

/// Whether `name` in `dir` is taken, by a file or a directory.
async fn exists(dir: &FileSystemDirectoryHandle, name: &str) -> Result<bool, FsError> {
    match file_handle(dir, name, false).await {
        Ok(_) | Err(FsError::IsADirectory) => Ok(true),
        Err(FsError::NotFound) => Ok(false),
        Err(error) => Err(error),
    }
}

async fn read_file(file: &FileSystemFileHandle) -> Result<Vec<u8>, FsError> {
    let file: File = await_js(file.get_file(), FsError::IsADirectory)
        .await?
        .unchecked_into();
    let buffer = await_js(file.array_buffer(), FsError::IsADirectory).await?;
    Ok(Uint8Array::new(&buffer).to_vec())
}

/// Replaces the file's contents through a swap file the browser commits on
/// `close()`. The bytes are copied into JavaScript first: the stream reads
/// them after this call yields, when wasm memory may have moved.
async fn write_file(file: &FileSystemFileHandle, bytes: &[u8]) -> Result<(), FsError> {
    let options = FileSystemCreateWritableOptions::new();
    options.set_keep_existing_data(false);
    let stream: FileSystemWritableFileStream = await_js(
        file.create_writable_with_options(&options),
        FsError::IsADirectory,
    )
    .await?
    .unchecked_into();
    let written = match stream.write_with_js_u8_array(&Uint8Array::from(bytes)) {
        Ok(promise) => await_js(promise, FsError::IsADirectory).await.map(drop),
        Err(error) => Err(js_error(&error, FsError::IsADirectory)),
    };
    if let Err(error) = written {
        let _ = JsFuture::from(stream.abort()).await;
        return Err(error);
    }
    await_js(stream.close(), FsError::IsADirectory)
        .await
        .map(drop)
}

/// Waits for the Web Lock `name` in `mode`. The lock is held until the
/// returned guard drops; dropping the future before the grant releases the
/// lock as soon as it is granted.
async fn acquire(name: &str, mode: LockMode) -> Result<WebLock, FsError> {
    let Some(locks) = property(&js_sys::global(), "navigator")
        .and_then(|navigator| property(&navigator, "locks"))
    else {
        return Err(FsError::Unsupported {
            feature: Feature::Locks,
            reason: NO_WEB_LOCKS,
        });
    };
    let locks: LockManager = locks.unchecked_into();

    let mut release = None;
    let held = Promise::new(&mut |resolve, _reject| release = Some(resolve));
    let mut settle = None;
    let granted = Promise::new(&mut |resolve, reject| settle = Some((resolve, reject)));
    let (Some(release), Some((on_granted, on_refused))) = (release, settle) else {
        return Err(FsError::Io(
            "the browser did not run a promise executor".into(),
        ));
    };
    // From here the guard exists, so any early return or a dropped future
    // lets the lock go the moment it is granted.
    let guard = WebLock { release };

    let callback = Closure::once_into_js(move |_lock: JsValue| -> JsValue {
        let _ = on_granted.call0(&JsValue::UNDEFINED);
        held.into()
    });
    let options = Object::new();
    let mode = match mode {
        LockMode::Shared => "shared",
        LockMode::Exclusive => "exclusive",
    };
    Reflect::set(
        &options,
        &JsValue::from_str("mode"),
        &JsValue::from_str(mode),
    )
    .map_err(|error| js_error(&error, FsError::NotFound))?;
    let request = locks
        .request(name, &options, &callback)
        .map_err(|error| js_error(&error, FsError::NotFound))?;
    request.then_or_else(&JsValue::UNDEFINED, &on_refused);
    await_js(granted, FsError::NotFound).await?;
    Ok(guard)
}

async fn await_js(promise: Promise, mismatch: FsError) -> Result<JsValue, FsError> {
    JsFuture::from(promise)
        .await
        .map_err(|error| js_error(&error, mismatch))
}

/// The [`FsError`] for a rejected storage call. `mismatch` is what a
/// `TypeMismatchError` means at the call site: a file where a directory was
/// expected, or the reverse.
fn js_error(error: &JsValue, mismatch: FsError) -> FsError {
    let name = property(error, "name")
        .and_then(|name| name.as_string())
        .unwrap_or_default();
    match name.as_str() {
        "NotFoundError" => FsError::NotFound,
        "TypeMismatchError" => mismatch,
        "InvalidModificationError" => FsError::DirectoryNotEmpty,
        "NotAllowedError" | "SecurityError" => FsError::PermissionDenied,
        "TypeError" => FsError::InvalidPath("the browser refused the name"),
        _ => {
            let message = property(error, "message")
                .and_then(|message| message.as_string())
                .or_else(|| error.as_string())
                .unwrap_or_else(|| format!("{error:?}"));
            FsError::Io(if name.is_empty() {
                message
            } else {
                format!("{name}: {message}")
            })
        }
    }
}
