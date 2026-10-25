//! Where an app keeps what is not a document: one seam for app files,
//! preferences and secrets, the same on Linux, macOS and the web.
//!
//! | What | Seam | Backed by |
//! |---|---|---|
//! | Documents (collaborative, synced) | resources and records; [`crate::persist`] for a single-document app | Automerge records |
//! | App files (caches, imports, databases) | [`AppFiles`], [`HostContext::files`] | `tilo_files::Dir` in `tilo_paths::AppPaths`: the per-user directories natively, the origin-private file system in a page |
//! | Preferences | typed [`Settings`] with a declared [`Scope`], [`HostContext::settings`] | the desktop settings document (`settings-store`), or the host's settings source: in a sandboxed app the shell's records, reached through the app's collaboration channel; personal sections roam between the person's devices |
//! | Secrets | [`secrets`], [`HostContext::secrets`]; one per document item with `SecretStore::item` | `tilo-secrets`: the Secret Service, the Keychain, the page's wrapped store |
//!
//! [`AppStorage`] bundles the three for one app; framework views and
//! surfaces get it from their context instead of building paths.
//!
//! Code with a [`HostContext`] uses its methods, which are scoped to the app
//! the host prepared. Code without one (a model loading its preferences
//! before the first frame) names the app or section itself. The older
//! entry points (`settings_store`, `tilo-paths`, `tilo_files::native::atomic`, `app-state`
//! from an app) are internals of these; apps other than Settings, which owns
//! the settings document, have no use for them.
//!
//! [`HostContext`]: crate::application::HostContext
//! [`HostContext::files`]: crate::application::HostContext::files
//! [`HostContext::settings`]: crate::application::HostContext::settings
//! [`HostContext::secrets`]: crate::application::HostContext::secrets

use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use tilo_lock::MutexRecover as _;

/// Whether this thread may wait on storage. The compositor's frame loop,
/// where an in-process app runs, may not: there an app reads and lists on a
/// worker and shows the result on a later frame.
pub use durable_store::waiting;
pub use settings_store::{ChangeOrigin, DeviceClass, InvokingAction};
pub use tilo_files::{Dir, FsError};
pub use tilo_paths::{AppPaths, Location};

/// One app's private file areas.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppFiles {
    paths: AppPaths,
}

impl AppFiles {
    /// The areas of the app with this id (its reverse-DNS id).
    #[must_use]
    pub fn for_app(app_id: &str) -> Self {
        Self {
            paths: AppPaths::for_app(app_id),
        }
    }

    /// Areas below `paths` instead (tests, hosts that sandbox each app).
    #[must_use]
    pub fn at(paths: AppPaths) -> Self {
        Self { paths }
    }

    /// Where each area is, for code that has to show or hand one on.
    #[must_use]
    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    /// Durable files the app owns: imports, databases, downloads it manages.
    ///
    /// # Errors
    ///
    /// When the area cannot be opened or created.
    pub fn data(&self) -> Result<Dir, FsError> {
        Dir::open(self.paths.data())
    }

    /// Configuration files. Preferences belong in [`Settings`].
    ///
    /// # Errors
    ///
    /// When the area cannot be opened or created.
    pub fn config(&self) -> Result<Dir, FsError> {
        Dir::open(self.paths.config())
    }

    /// Files the app can rebuild, which the system may delete.
    ///
    /// # Errors
    ///
    /// When the area cannot be opened or created.
    pub fn cache(&self) -> Result<Dir, FsError> {
        Dir::open(self.paths.cache())
    }

    /// State worth keeping across restarts but not worth backing up.
    ///
    /// # Errors
    ///
    /// When the area cannot be opened or created.
    pub fn state(&self) -> Result<Dir, FsError> {
        Dir::open(self.paths.state())
    }
}

/// Where an app's typed settings live and how far they travel. Settings
/// follow a person or a device, never a room's members; they are never
/// visible to collaborators. Content shared with a room is a record instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Follows the person to every device they sign in on: the device fabric
    /// carries the app's whole personal section between the person's own
    /// devices, the most recent write winning. Keep it small (at most
    /// [`settings_store::continuity::MAX_PERSONAL_VALUE_BYTES`] serialized):
    /// a confirmation preference, a refresh interval, a default sort.
    Personal,
    /// Kept on this device and never sent anywhere: a hardware choice, a
    /// path, a window placement.
    DeviceLocal,
    /// A per-user section shared across apps that another owner declares
    /// (`locale`, `text_assistance`). How each of its keys travels is that
    /// section's, reviewed in `settings_store::continuity`. Only its owner
    /// writes it: a sandboxed app reads it and nothing more.
    Shared(&'static str),
}

impl Scope {
    /// The document section holding the settings of this scope.
    #[must_use]
    pub const fn section(self) -> &'static str {
        match self {
            Self::Personal => settings_store::continuity::APP_PERSONAL_SECTION,
            Self::DeviceLocal => settings_store::continuity::APP_DEVICE_LOCAL_SECTION,
            Self::Shared(name) => name,
        }
    }
}

/// A typed settings value with a declared [`Scope`].
///
/// Reads never fail: a missing document reads as [`Default`], and a key
/// that is missing or does not decode reads as its default while every
/// other key keeps its stored value. An app's types of one scope share
/// its section, so their field names must not collide.
///
/// ```ignore
/// #[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
/// struct Preferences { confirm_kill: bool }
/// impl SettingsSchema for Preferences {
///     const SCOPE: Scope = Scope::Personal;
/// }
/// let prefs = context.settings::<Preferences>().get();
/// ```
pub trait SettingsSchema: Serialize + DeserializeOwned + Default + 'static {
    const SCOPE: Scope;

    /// What a person who changed nothing has on a `device`: [`Default`]
    /// everywhere, unless the setting costs what one class of device should
    /// not spend unasked (a copy kept on the device, work in the
    /// background).
    #[must_use]
    fn defaults_on(device: DeviceClass) -> Self {
        let _ = device;
        Self::default()
    }

    /// Whether the setting means anything on a `device`: one whose effect a
    /// class of device cannot have (a copy kept on a device that keeps
    /// none) is not offered there, neither shown nor published.
    #[must_use]
    fn available_on(device: DeviceClass) -> bool {
        let _ = device;
        true
    }

    /// The setting `value` holds, as JSON stores it. Generic code reading
    /// a setting compiles this, and serde's whole decoder for the type,
    /// into each crate that names the type; a large type's own crate can
    /// compile it once instead with [`settings_codec!`](crate::settings_codec).
    ///
    /// # Errors
    ///
    /// When `value` is not one.
    fn from_settings_value(value: Value) -> serde_json::Result<Self> {
        serde_json::from_value(value)
    }

    /// The setting as JSON stores it; see [`Self::from_settings_value`].
    ///
    /// # Errors
    ///
    /// When it does not serialize.
    fn to_settings_value(&self) -> serde_json::Result<Value> {
        serde_json::to_value(self)
    }
}

#[doc(hidden)]
pub use serde_json as __serde_json;

/// [`SettingsSchema::from_settings_value`] and
/// [`SettingsSchema::to_settings_value`] compiled where the impl is, never in
/// the crates that read the setting: for a large settings type whose crate
/// is in the shared runtime or an engine, every app or pane reading it
/// otherwise carries its own copy of serde's decoder. Use it inside the
/// `impl SettingsSchema`.
#[macro_export]
macro_rules! settings_codec {
    () => {
        #[inline(never)]
        fn from_settings_value(
            value: $crate::storage::__serde_json::Value,
        ) -> $crate::storage::__serde_json::Result<Self> {
            $crate::storage::__serde_json::from_value(value)
        }

        #[inline(never)]
        fn to_settings_value(
            &self,
        ) -> $crate::storage::__serde_json::Result<$crate::storage::__serde_json::Value> {
            $crate::storage::__serde_json::to_value(self)
        }
    };
}

/// Which settings document a host gives its apps.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum SettingsDocument {
    /// The person's session document: the one the host installed
    /// (`settings_store::install_source`), else the local `settings.json`.
    #[default]
    Session,
    /// A document file of the host's own: a test or capture profile.
    File(PathBuf),
}

/// How many writes through this module each document has taken, so a
/// reader in the same process sees a write to its document even within one
/// file-time tick, and is not sent to read it again by another document's.
static WRITES: std::sync::Mutex<std::collections::BTreeMap<SettingsDocument, u64>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

fn writes() -> std::sync::MutexGuard<'static, std::collections::BTreeMap<SettingsDocument, u64>> {
    WRITES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Count a write to `document` ([`WRITES`]).
fn note_write(document: &SettingsDocument) {
    let mut writes = writes();
    let count = writes.entry(document.clone()).or_default();
    *count = count.wrapping_add(1);
}

/// One version of a settings document, for readers that follow it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettingsRevision {
    stamp: Option<settings_store::SettingsStamp>,
    writes: u64,
}

impl SettingsDocument {
    /// Where the document is, when it is a file.
    #[must_use]
    pub fn path(&self) -> Option<PathBuf> {
        match self {
            Self::Session => settings_store::settings_path(),
            Self::File(path) => Some(path.clone()),
        }
    }

    /// The whole document, with the changes still on their way to it
    /// ([`flush_settings_writes`]).
    #[must_use]
    pub fn load(&self) -> Map<String, Value> {
        self.read_with_pending(None, || match self {
            Self::Session => settings_store::load(),
            Self::File(path) => settings_store::load_at(path),
        })
    }

    /// One section's keys, with the changes still on their way to it.
    #[must_use]
    pub fn section(&self, name: &str) -> Map<String, Value> {
        self.read_with_pending(Some(name), || match self {
            Self::Session => settings_store::load_section(name),
            Self::File(path) => settings_store::load_section_at(path, name),
        })
    }

    fn read_with_pending(
        &self,
        name: Option<&str>,
        mut read: impl FnMut() -> Map<String, Value>,
    ) -> Map<String, Value> {
        loop {
            let retired = lock_queue().retired;
            let mut values = read();
            let queue = lock_queue();
            // A writer may have replaced the file and removed its queued
            // changes after we read the old file. Retry rather than lose
            // those changes; never hold the queue lock during file I/O.
            if queue.retired != retired {
                continue;
            }
            for change in queue.pending.iter().filter(|c| c.document == *self) {
                match name {
                    Some(name) if change.name == name => values.extend(change.values.clone()),
                    None => settings_store::merge_section(
                        &mut values,
                        &change.name,
                        change.values.clone(),
                    ),
                    Some(_) => {}
                }
            }
            return values;
        }
    }

    /// Whether this process writes the document's file itself: a file, or
    /// the session's when the host installed no source of its own (the
    /// Settings app, which the launcher hands the session document).
    fn is_file(&self) -> bool {
        matches!(self, Self::File(_)) || settings_store::installed_source().is_none()
    }

    /// Merge `values` into section `name`, keeping every other section and
    /// key.
    ///
    /// # Errors
    ///
    /// When there is no document to write or the write fails.
    pub fn update_section(&self, name: &str, values: Map<String, Value>) -> std::io::Result<()> {
        self.update_section_by(name, values, None)
    }

    /// [`Self::update_section`], recording who and what made the change
    /// with every key it changes, where the document keeps a history of
    /// its changes (the session's).
    ///
    /// # Errors
    ///
    /// As [`Self::update_section`].
    pub fn update_section_by(
        &self,
        name: &str,
        values: Map<String, Value>,
        origin: Option<&settings_store::ChangeOrigin>,
    ) -> std::io::Result<()> {
        let file = self.is_file();
        let written = if file && crate::rt::is_ui_thread() {
            defer(Change {
                document: self.clone(),
                name: name.to_owned(),
                values,
            });
            Ok(())
        } else {
            if file {
                // After what a UI thread changed in this document before, in
                // the order made. Other documents' changes keep their own
                // order and need not wait for this write.
                store_pending(Some(self));
            }
            match (self, origin) {
                (Self::Session, Some(origin)) => {
                    settings_store::update_section_by(name, values, origin)
                }
                (Self::Session, None) => settings_store::update_section(name, values),
                (Self::File(path), _) => settings_store::update_section_at(path, name, values),
            }
        };
        note_write(self);
        written
    }

    /// The document's current version.
    #[must_use]
    pub fn revision(&self) -> SettingsRevision {
        SettingsRevision {
            stamp: match self {
                Self::Session => settings_store::stamp(),
                Self::File(path) => settings_store::stamp_at(path),
            },
            writes: writes().get(self).copied().unwrap_or_default(),
        }
    }
}

/// A change to a settings file a UI thread made, not yet stored.
#[derive(Clone)]
struct Change {
    document: SettingsDocument,
    name: String,
    values: Map<String, Value>,
}

/// The settings changes UI threads made, stored in order off them.
///
/// Storing a change takes the document's lock, rewrites the file and syncs
/// it: on a busy disk that took hundreds of milliseconds, and as long as
/// another writer held the lock, and the window waited for it on every
/// toggle and every step of a slider. A UI thread's change is queued
/// instead and shows at once to every reader in the process (reads apply
/// the queue over the file, retrying if a batch was retired during the
/// read). A background job stores the queue, oldest
/// first: every queued change to a file under one lock and one atomic
/// replace, each merged into the file as it is then. The last change to a
/// key wins and what other processes stored meanwhile is kept. A writer on
/// any other thread stores the queue first, so changes land in the order
/// they were made in that document.
///
/// Each document has a job of its own, scheduled on the background of the
/// thread that queued its first change, and that job stores that document's
/// changes alone: a document's changes never wait for, nor are stored by,
/// the job of another (another window's, or another test's profile).
struct Queue {
    pending: Vec<Change>,
    /// Batches removed after a store, so readers can detect a stale file.
    retired: u64,
    /// The documents a background job is to store.
    scheduled: Vec<SettingsDocument>,
    /// A thread is writing the oldest changes.
    writing: bool,
    /// The window to wake when a change could not be stored, so it shows
    /// the document as it is.
    wake: Option<crate::WakeHandle>,
}

static QUEUE: std::sync::Mutex<Queue> = std::sync::Mutex::new(Queue {
    pending: Vec::new(),
    retired: 0,
    scheduled: Vec::new(),
    writing: false,
    wake: None,
});
static STORED: std::sync::Condvar = std::sync::Condvar::new();

fn lock_queue() -> std::sync::MutexGuard<'static, Queue> {
    QUEUE.lock_recover()
}

fn wait(queue: std::sync::MutexGuard<'static, Queue>) -> std::sync::MutexGuard<'static, Queue> {
    STORED
        .wait(queue)
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn defer(change: Change) {
    let document = change.document.clone();
    let mut queue = lock_queue();
    queue.pending.push(change);
    queue.wake = Some(crate::WakeHandle::new());
    if queue.scheduled.contains(&document) {
        return;
    }
    queue.scheduled.push(document.clone());
    drop(queue);
    // A host with no background work stores it here rather than lose it.
    let job = document.clone();
    if crate::background::Background::current()
        .spawn("settings writes", move || store_queue(&job))
        .is_err()
    {
        store_queue(&document);
    }
}

/// The background job of `document`: store its queued changes until there
/// are none.
fn store_queue(document: &SettingsDocument) {
    loop {
        store_pending(Some(document));
        let mut queue = lock_queue();
        // A change queued after the last batch found this job scheduled.
        if !queue
            .pending
            .iter()
            .any(|change| change.document == *document)
        {
            queue.scheduled.retain(|scheduled| scheduled != document);
            STORED.notify_all();
            return;
        }
    }
}

/// Store the queued changes to `only` (every queued change when `None`) on
/// this thread, oldest first, after the batch another thread may be
/// writing.
fn store_pending(only: Option<&SettingsDocument>) {
    let wanted = |change: &Change| only.is_none_or(|document| change.document == *document);
    let mut queue = lock_queue();
    loop {
        if queue.writing {
            queue = wait(queue);
            continue;
        }
        // While `writing` is set no other thread takes changes off the
        // queue, and new ones are only appended, so these positions hold
        // until the batch is stored.
        let taken: Vec<usize> = (0..queue.pending.len())
            .filter(|&at| wanted(&queue.pending[at]))
            .collect();
        if taken.is_empty() {
            return;
        }
        queue.writing = true;
        let batch: Vec<Change> = taken.iter().map(|&at| queue.pending[at].clone()).collect();
        drop(queue);
        debug_assert!(
            !crate::rt::is_ui_thread(),
            "a settings file was written on the UI thread"
        );
        let mut failed = false;
        for (at, change) in batch.iter().enumerate() {
            let document = &change.document;
            if batch[..at].iter().any(|c| c.document == *document) {
                continue;
            }
            let path = match document {
                SettingsDocument::File(path) => Ok(path.clone()),
                SettingsDocument::Session => settings_store::file_write_path(),
            };
            let stored = path.and_then(|path| {
                settings_store::update_document_at(&path, |doc| {
                    for c in batch.iter().filter(|c| c.document == *document) {
                        settings_store::merge_section(doc, &c.name, c.values.clone());
                    }
                })
            });
            if let Err(error) = stored {
                tracing::warn!(?document, %error, "settings changes not stored");
                failed = true;
            }
        }
        queue = lock_queue();
        for &at in taken.iter().rev() {
            queue.pending.remove(at);
        }
        queue.retired = queue.retired.wrapping_add(1);
        queue.writing = false;
        for (at, change) in batch.iter().enumerate() {
            if !batch[..at].iter().any(|c| c.document == change.document) {
                note_write(&change.document);
            }
        }
        if failed && let Some(wake) = queue.wake.take() {
            wake.wake();
        }
        STORED.notify_all();
    }
}

/// Store every settings change a UI thread made before going on: off a UI
/// thread here, on one by waiting for the background, which the native
/// runner does before the process ends.
pub fn flush_settings_writes() {
    if !crate::rt::is_ui_thread() {
        store_pending(None);
        return;
    }
    let mut queue = lock_queue();
    while !queue.scheduled.is_empty() {
        queue = wait(queue);
    }
}

/// An app's typed settings of one [`SettingsSchema`] type, in the settings
/// document its host gave it. Get one from
/// [`HostContext::settings`](crate::application::HostContext::settings),
/// [`AppStorage::settings`], or a framework view's `cx.settings::<T>()`.
pub struct Settings<T> {
    app_id: String,
    document: SettingsDocument,
    /// The class of device whose defaults unset keys read as; the
    /// installed one ([`DeviceClass::current`]) unless pinned.
    device: Option<DeviceClass>,
    schema: PhantomData<fn() -> T>,
}

impl<T> Clone for Settings<T> {
    fn clone(&self) -> Self {
        Self {
            app_id: self.app_id.clone(),
            document: self.document.clone(),
            device: self.device,
            schema: PhantomData,
        }
    }
}

impl<T> std::fmt::Debug for Settings<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings")
            .field("app_id", &self.app_id)
            .field("type", &std::any::type_name::<T>())
            .field("document", &self.document)
            .field("device", &self.device)
            .finish()
    }
}

impl<T: SettingsSchema> Settings<T> {
    /// The settings of the app with this id, in the session document.
    #[must_use]
    pub fn for_app(app_id: &str) -> Self {
        Self {
            app_id: app_id.to_owned(),
            document: SettingsDocument::Session,
            device: None,
            schema: PhantomData,
        }
    }

    /// A [`Scope::Shared`] section, for code that serves every app (the
    /// toolkit's text fields reading `text_assistance`).
    #[must_use]
    pub fn shared() -> Self {
        debug_assert!(
            matches!(T::SCOPE, Scope::Shared(_)),
            "only a shared section is read without an app"
        );
        Self::for_app("")
    }

    /// The same settings in `document` instead.
    #[must_use]
    pub fn in_document(mut self, document: SettingsDocument) -> Self {
        self.document = document;
        self
    }

    /// The same settings in the document file at `path` instead.
    #[must_use]
    pub fn in_file(self, path: impl Into<PathBuf>) -> Self {
        self.in_document(SettingsDocument::File(path.into()))
    }

    /// The same settings read with `device`'s defaults, whatever device
    /// the process runs on.
    #[must_use]
    pub fn on_device(mut self, device: DeviceClass) -> Self {
        self.device = Some(device);
        self
    }

    /// Whether these settings mean anything on the device they are read on
    /// ([`SettingsSchema::available_on`]).
    #[must_use]
    pub fn available(&self) -> bool {
        T::available_on(self.device.unwrap_or_else(DeviceClass::current))
    }

    /// How far these settings travel.
    #[must_use]
    pub const fn scope(&self) -> Scope {
        T::SCOPE
    }

    /// The document they live in.
    #[must_use]
    pub fn document(&self) -> &SettingsDocument {
        &self.document
    }

    /// The app whose settings these are (empty for a shared section).
    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// The stored keys of this scope's object for the app.
    fn stored(&self) -> Map<String, Value> {
        stored_in(&self.document, &self.app_id, T::SCOPE)
    }

    /// The current value.
    #[must_use]
    pub fn get(&self) -> T {
        decode(
            self.stored(),
            self.device.unwrap_or_else(DeviceClass::current),
        )
    }

    /// Store `value`. Keys of the section that `T` does not name are kept.
    ///
    /// # Errors
    ///
    /// When `value` does not serialize to an object, a personal section
    /// would outgrow what the device fabric carries, there is no document to
    /// write, or the write fails.
    pub fn set(&self, value: &T) -> std::io::Result<()> {
        self.set_by(value, None)
    }

    /// [`Self::set`], recording `origin` — the action and who invoked it —
    /// in the document's history of who changed which setting.
    ///
    /// # Errors
    ///
    /// As [`Self::set`].
    pub fn set_by(
        &self,
        value: &T,
        origin: Option<&settings_store::ChangeOrigin>,
    ) -> std::io::Result<()> {
        let value = value.to_settings_value().map_err(std::io::Error::other)?;
        let current = self.get().to_settings_value().ok();
        store_changed(
            &self.document,
            &self.app_id,
            T::SCOPE,
            value,
            current,
            origin,
        )
    }

    /// Change the current value with `change` and store it, returning what
    /// was stored.
    ///
    /// # Errors
    ///
    /// As [`Self::set`].
    pub fn update(&self, change: impl FnOnce(&mut T)) -> std::io::Result<T> {
        let mut value = self.get();
        change(&mut value);
        self.set(&value)?;
        Ok(value)
    }

    /// The document's current version: when it changes, [`Self::get`] may
    /// answer differently.
    #[must_use]
    pub fn revision(&self) -> SettingsRevision {
        self.document.revision()
    }
}

/// Store the keys of `value` that differ from `current`, what is read now:
/// [`Settings::set_by`] once both are JSON. Not generic, so the toolkit
/// compiles it once rather than every app once per settings type.
fn store_changed(
    document: &SettingsDocument,
    app_id: &str,
    scope: Scope,
    value: Value,
    current: Option<Value>,
    origin: Option<&settings_store::ChangeOrigin>,
) -> std::io::Result<()> {
    // Only what differs from what is read now is written. A key the
    // person never set reads as `T`'s default; writing the whole value
    // would store that default as their choice, and a field edit in one
    // place (a text size) would pin every other field of the section to
    // whatever default this build has (the theme mode among them),
    // replacing what another writer meant by leaving it unset.
    let value = match (value, current) {
        (Value::Object(values), Some(Value::Object(current))) => Value::Object(
            values
                .into_iter()
                .filter(|(key, value)| current.get(key) != Some(value))
                .collect(),
        ),
        (value, _) => value,
    };
    if value.as_object().is_some_and(Map::is_empty) {
        return Ok(());
    }
    store_in(document, app_id, scope, value, origin)
}

/// Decode `stored` over `T`'s defaults on `device` one key at a time, so a key that
/// does not decode (an older shape, a hand edit) reads as its default
/// instead of discarding every other key.
fn decode<T: SettingsSchema>(stored: Map<String, Value>, device: DeviceClass) -> T {
    let defaults = T::defaults_on(device).to_settings_value();
    let decodes = |value: &Value| T::from_settings_value(value.clone()).is_ok();
    T::from_settings_value(decoded(defaults.ok(), stored, &decodes))
        .unwrap_or_else(|_| T::defaults_on(device))
}

/// The object [`decode`] reads: `stored` over `defaults`, keeping each
/// stored key only where the result still `decodes`. Without defaults, the
/// stored object as it is.
fn decoded(
    defaults: Option<Value>,
    stored: Map<String, Value>,
    decodes: &dyn Fn(&Value) -> bool,
) -> Value {
    let Some(Value::Object(mut current)) = defaults else {
        return Value::Object(stored);
    };
    let whole = Value::Object(merged(&current, &stored));
    if decodes(&whole) {
        return whole;
    }
    for (key, value) in stored {
        let mut candidate = current.clone();
        candidate.insert(key, value);
        if decodes(&Value::Object(candidate.clone())) {
            current = candidate;
        }
    }
    Value::Object(current)
}

/// The stored keys of `scope`'s object for the app `app_id` in `document`.
fn stored_in(document: &SettingsDocument, app_id: &str, scope: Scope) -> Map<String, Value> {
    let section = document.section(scope.section());
    match scope {
        Scope::Shared(_) => section,
        Scope::Personal | Scope::DeviceLocal => match section.get(app_id) {
            Some(Value::Object(object)) => object.clone(),
            _ => Map::new(),
        },
    }
}

/// Store `value`, a section's value as JSON, in `scope`'s object for the app
/// `app_id` in `document`, recording `origin`: [`Settings::set_by`].
fn store_in(
    document: &SettingsDocument,
    app_id: &str,
    scope: Scope,
    value: Value,
    origin: Option<&settings_store::ChangeOrigin>,
) -> std::io::Result<()> {
    let Value::Object(values) = value else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "settings must serialize to an object",
        ));
    };
    match scope {
        Scope::Shared(name) => document.update_section_by(name, values, origin),
        scope @ (Scope::Personal | Scope::DeviceLocal) => {
            let mut object = stored_in(document, app_id, scope);
            object.extend(values);
            if scope == Scope::Personal {
                let size = serde_json::to_vec(&object)
                    .map_err(std::io::Error::other)?
                    .len();
                if size > settings_store::continuity::MAX_PERSONAL_VALUE_BYTES {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!(
                            "{app_id}'s personal settings would be {size} bytes, more than a \
                             person's devices carry"
                        ),
                    ));
                }
            }
            let mut entry = Map::new();
            entry.insert(app_id.to_owned(), Value::Object(object));
            document.update_section_by(scope.section(), entry, origin)
        }
    }
}

fn merged(base: &Map<String, Value>, over: &Map<String, Value>) -> Map<String, Value> {
    let mut out = base.clone();
    out.extend(over.iter().map(|(key, value)| (key.clone(), value.clone())));
    out
}

/// What an app keeps outside its documents, scoped to the app and the
/// settings document its host gave it: its files, its secrets and its typed
/// settings. Framework views and surfaces reach it through their context.
#[derive(Clone, Debug)]
pub struct AppStorage {
    app_id: String,
    secrets: crate::secrets::SecretStore,
    settings: SettingsDocument,
    files: AppFiles,
}

impl AppStorage {
    /// Storage of the app `app_id` with these secrets and settings.
    #[must_use]
    pub fn new(
        app_id: &str,
        secrets: crate::secrets::SecretStore,
        settings: SettingsDocument,
    ) -> Self {
        Self {
            app_id: app_id.to_owned(),
            secrets,
            settings,
            files: AppFiles::for_app(app_id),
        }
    }

    /// The same storage with the app's file areas at `files`: a test or
    /// capture profile's own instead of the person's.
    #[must_use]
    pub fn with_files(mut self, files: AppFiles) -> Self {
        self.files = files;
        self
    }

    /// Storage of the app `app_id` in the current host's secret store and
    /// the session settings document.
    #[must_use]
    pub fn for_app(app_id: &str) -> Self {
        Self::new(app_id, secrets(app_id), SettingsDocument::Session)
    }

    /// The app's id.
    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// The app's private file areas.
    #[must_use]
    pub fn files(&self) -> AppFiles {
        self.files.clone()
    }

    /// The app's secrets. A secret that belongs to one document item is
    /// [`SecretStore::item`](crate::secrets::SecretStore::item).
    #[must_use]
    pub fn secrets(&self) -> crate::secrets::SecretStore {
        self.secrets.clone()
    }

    /// The app's typed settings of type `T`.
    #[must_use]
    pub fn settings<T: SettingsSchema>(&self) -> Settings<T> {
        Settings::for_app(&self.app_id).in_document(self.settings.clone())
    }

    /// The settings document the host gave the app.
    #[must_use]
    pub fn settings_document(&self) -> &SettingsDocument {
        &self.settings
    }
}

/// One untyped section of the desktop settings document, for the owner of a
/// shared section (Settings, the theme store) and readers of one that has
/// no [`SettingsSchema`] type yet. Apps keep their own preferences in a
/// typed [`Settings`]. Reads never fail; a missing, unreadable or malformed
/// document reads as empty. Writes merge the given keys into the section and
/// keep every other section and key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsSection {
    section: String,
    document: SettingsDocument,
}

impl SettingsSection {
    /// The section named `name`.
    #[must_use]
    pub fn named(name: &str) -> Self {
        Self {
            section: name.to_owned(),
            document: SettingsDocument::Session,
        }
    }

    /// The same section in the document at `path` instead of the user's
    /// (tests, tools that edit another profile).
    #[must_use]
    pub fn in_file(self, path: impl Into<PathBuf>) -> Self {
        self.in_document(SettingsDocument::File(path.into()))
    }

    /// The same section in `document`: the one the host gave the app
    /// ([`AppStorage::settings_document`]).
    #[must_use]
    pub fn in_document(mut self, document: SettingsDocument) -> Self {
        self.document = document;
        self
    }

    /// The section's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.section
    }

    /// Where the document is, when it is a file.
    #[must_use]
    pub fn path(&self) -> Option<PathBuf> {
        self.document.path()
    }

    /// The whole desktop settings document.
    #[must_use]
    pub fn document(&self) -> Map<String, Value> {
        self.document.load()
    }

    /// The section's keys.
    #[must_use]
    pub fn load(&self) -> Map<String, Value> {
        self.document.section(&self.section)
    }

    /// One key, when present and of this type.
    #[must_use]
    pub fn bool(&self, key: &str) -> Option<bool> {
        self.load().get(key).and_then(Value::as_bool)
    }

    /// One string key, when present.
    #[must_use]
    pub fn string(&self, key: &str) -> Option<String> {
        self.load()
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    /// Merge `values` into the section.
    ///
    /// # Errors
    ///
    /// When there is no document to write or the write fails.
    pub fn update(&self, values: Map<String, Value>) -> std::io::Result<()> {
        self.document.update_section(&self.section, values)
    }
}

/// Parse settings-document text the way every reader does: anything
/// malformed reads as an empty document.
#[must_use]
pub fn parse_settings(text: &str) -> Map<String, Value> {
    settings_store::parse(text)
}

/// The document at `path`, for code that compares two profiles.
#[must_use]
pub fn settings_at(path: &Path) -> Map<String, Value> {
    settings_store::load_at(path)
}

/// This app's secrets in the store the current host provides.
#[must_use]
pub fn secrets(app_id: &str) -> crate::secrets::SecretStore {
    crate::host::current().secrets.store(app_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Queue and retire an update precisely after the reader has loaded
    /// the old file, before it can overlay the queue. No timing or disk
    /// speed assumption is needed to reproduce the background-writer race.
    fn read_while_the_background_retires_a_change(name: Option<&str>) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        std::fs::write(
            &file,
            r#"{"playback":{"autoplay":true},"other":{"kept":1}}"#,
        )
        .unwrap();
        let document = SettingsDocument::File(file.clone());
        let work = crate::background::Pumped::new();
        let _work = work.background().enter();
        crate::rt::mark_ui_thread();
        let mut reads = 0;
        let values = document.read_with_pending(name, || {
            let values = match name {
                Some(name) => settings_store::load_section_at(&file, name),
                None => settings_store::load_at(&file),
            };
            reads += 1;
            if reads == 1 {
                document
                    .update_section(
                        "playback",
                        [("autoplay".into(), Value::Bool(false))]
                            .into_iter()
                            .collect(),
                    )
                    .unwrap();
                assert_eq!(document.section("playback")["autoplay"], false);
                std::thread::scope(|scope| {
                    scope.spawn(|| {
                        // Off the UI thread, this document's changes
                        // first; then its own job, which finds none.
                        store_pending(Some(&document));
                        work.run_pending();
                    });
                });
                assert_eq!(
                    settings_store::load_section_at(&file, "playback")["autoplay"],
                    false
                );
                assert!(lock_queue().pending.iter().all(|c| c.document != document));
            }
            values
        });
        let playback = if name.is_some() {
            Value::Object(values)
        } else {
            assert_eq!(values["other"]["kept"], 1);
            values["playback"].clone()
        };
        assert_eq!(
            playback["autoplay"], false,
            "a persisted update must not disappear from a read"
        );
        assert!(reads > 1, "the stale file read must be retried");
        assert_eq!(document.section("playback")["autoplay"], false);
        assert_eq!(document.load()["playback"]["autoplay"], false);
    }

    #[test]
    fn a_document_read_keeps_a_change_retired_during_the_file_read() {
        read_while_the_background_retires_a_change(None);
    }

    #[test]
    fn a_section_read_keeps_a_change_retired_during_the_file_read() {
        read_while_the_background_retires_a_change(Some("playback"));
    }

    #[test]
    fn a_section_update_keeps_other_sections_and_keys() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        let appearance = SettingsSection::named("appearance").in_file(&file);
        appearance
            .update(
                [("theme".to_owned(), Value::from("dark"))]
                    .into_iter()
                    .collect(),
            )
            .unwrap();
        let locale = SettingsSection::named("locale").in_file(&file);
        locale
            .update(
                [("measurement".to_owned(), Value::from("Metric"))]
                    .into_iter()
                    .collect(),
            )
            .unwrap();
        locale
            .update(
                [("first_day_of_week".to_owned(), Value::from(1))]
                    .into_iter()
                    .collect(),
            )
            .unwrap();
        assert_eq!(appearance.string("theme").as_deref(), Some("dark"));
        assert_eq!(locale.string("measurement").as_deref(), Some("Metric"));
        assert_eq!(
            locale.load().get("first_day_of_week"),
            Some(&Value::from(1))
        );
        assert_eq!(locale.path(), Some(file.clone()));
        assert!(settings_at(&file).contains_key("appearance"));
    }

    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Prefs {
        confirm: bool,
        interval_ms: u64,
    }

    impl Default for Prefs {
        fn default() -> Self {
            Self {
                confirm: true,
                interval_ms: 1500,
            }
        }
    }

    impl SettingsSchema for Prefs {
        const SCOPE: Scope = Scope::Personal;
    }

    #[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Placement {
        x: i32,
    }

    impl SettingsSchema for Placement {
        const SCOPE: Scope = Scope::DeviceLocal;
    }

    #[test]
    fn typed_settings_live_in_the_section_of_their_scope() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        let storage = AppStorage::new(
            "so.tilo.example",
            crate::secrets::SecretStore::memory("so.tilo.example"),
            SettingsDocument::File(file.clone()),
        );
        let prefs = storage.settings::<Prefs>();
        assert_eq!(prefs.get(), Prefs::default());
        let before = prefs.revision();
        prefs.update(|prefs| prefs.confirm = false).unwrap();
        assert_ne!(prefs.revision(), before);
        storage
            .settings::<Placement>()
            .set(&Placement { x: 40 })
            .unwrap();
        Settings::<Prefs>::for_app("so.tilo.other")
            .in_file(&file)
            .set(&Prefs {
                confirm: true,
                interval_ms: 1000,
            })
            .unwrap();

        let document = settings_at(&file);
        // Only what was changed is stored: the interval nobody chose stays
        // unset and keeps following the app's default.
        assert_eq!(
            Value::Object(document.clone()),
            serde_json::json!({
                "app_personal": {
                    "so.tilo.example": {"confirm": false},
                    "so.tilo.other": {"interval_ms": 1000}
                },
                "app_device_local": {"so.tilo.example": {"x": 40}}
            })
        );
        // The device fabric carries the personal sections and nothing else.
        assert_eq!(
            settings_store::continuity::personal_values(&document)
                .keys()
                .collect::<Vec<_>>(),
            [
                "tilo.app_personal.so.tilo.example",
                "tilo.app_personal.so.tilo.other"
            ]
        );
        assert_eq!(
            storage.settings::<Prefs>().get(),
            Prefs {
                confirm: false,
                interval_ms: 1500
            }
        );
    }

    #[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
    struct Copies {
        keep: bool,
    }

    impl SettingsSchema for Copies {
        const SCOPE: Scope = Scope::DeviceLocal;

        fn defaults_on(device: DeviceClass) -> Self {
            Self {
                keep: device == DeviceClass::Native,
            }
        }

        fn available_on(device: DeviceClass) -> bool {
            device == DeviceClass::Native
        }
    }

    #[test]
    fn a_setting_nobody_changed_reads_as_the_default_of_the_device_it_is_read_on() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        let on = |device| {
            Settings::<Copies>::for_app("so.tilo.example")
                .in_file(&file)
                .on_device(device)
        };
        assert!(on(DeviceClass::Native).get().keep);
        assert!(!on(DeviceClass::Web).get().keep);
        assert!(on(DeviceClass::Native).available());
        assert!(!on(DeviceClass::Web).available());
        assert!(
            Settings::<Prefs>::for_app("so.tilo.example")
                .on_device(DeviceClass::Web)
                .available(),
            "a setting is available everywhere unless it says otherwise"
        );

        // Choosing the default stores nothing, so the choice keeps following
        // the device; choosing otherwise is the person's on every device.
        on(DeviceClass::Native).set(&Copies { keep: true }).unwrap();
        assert!(!settings_at(&file).contains_key("app_device_local"));
        on(DeviceClass::Native)
            .set(&Copies { keep: false })
            .unwrap();
        assert!(!on(DeviceClass::Native).get().keep);
        on(DeviceClass::Web).set(&Copies { keep: true }).unwrap();
        assert!(on(DeviceClass::Web).get().keep);
    }

    #[test]
    fn changing_one_field_never_stores_the_others_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        std::fs::write(&file, r#"{"app_personal": {"so.tilo.example": {}}}"#).unwrap();
        let prefs = Settings::<Prefs>::for_app("so.tilo.example").in_file(&file);
        prefs.update(|prefs| prefs.confirm = false).unwrap();
        let stored = || {
            SettingsSection::named("app_personal")
                .in_file(&file)
                .load()
                .get("so.tilo.example")
                .cloned()
        };
        assert_eq!(stored(), Some(serde_json::json!({"confirm": false})));
        // Another writer sets the interval; a later change of `confirm` here
        // leaves it as that writer left it.
        Settings::<Prefs>::for_app("so.tilo.example")
            .in_file(&file)
            .update(|prefs| prefs.interval_ms = 900)
            .unwrap();
        prefs.update(|prefs| prefs.confirm = true).unwrap();
        assert_eq!(
            stored(),
            Some(serde_json::json!({"confirm": true, "interval_ms": 900}))
        );
        // Setting what is already there writes nothing, and a write to
        // another document is not a change of this one.
        let revision = prefs.revision();
        prefs.set(&prefs.get()).unwrap();
        let elsewhere = dir.path().join("other.json");
        Settings::<Prefs>::for_app("so.tilo.example")
            .in_file(&elsewhere)
            .update(|prefs| prefs.interval_ms = 700)
            .unwrap();
        assert_eq!(prefs.revision(), revision);
    }

    #[test]
    fn a_key_that_does_not_decode_reads_as_its_default_alone() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        std::fs::write(
            &file,
            r#"{"app_personal": {"so.tilo.example": {"confirm": false, "interval_ms": "soon", "retired": 1}}}"#,
        )
        .unwrap();
        let prefs = Settings::<Prefs>::for_app("so.tilo.example").in_file(&file);
        assert_eq!(
            prefs.get(),
            Prefs {
                confirm: false,
                interval_ms: 1500
            }
        );
        // A write keeps keys this type does not name.
        prefs.update(|prefs| prefs.interval_ms = 2000).unwrap();
        assert_eq!(
            SettingsSection::named("app_personal")
                .in_file(&file)
                .load()
                .get("so.tilo.example"),
            Some(&serde_json::json!({"confirm": false, "interval_ms": 2000, "retired": 1}))
        );
    }

    #[test]
    fn a_personal_section_too_large_to_roam_is_refused() {
        #[derive(Default, serde::Serialize, serde::Deserialize)]
        struct Big {
            words: Vec<String>,
        }
        impl SettingsSchema for Big {
            const SCOPE: Scope = Scope::Personal;
        }
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        let big = Settings::<Big>::for_app("so.tilo.example").in_file(&file);
        let words = vec!["x".repeat(1024); 80];
        let error = big.set(&Big { words }).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(!file.exists());
    }

    #[test]
    fn a_malformed_document_reads_as_empty() {
        assert!(parse_settings("{not json").is_empty());
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("settings.json");
        std::fs::write(&file, "[1, 2]").unwrap();
        assert!(
            SettingsSection::named("locale")
                .in_file(&file)
                .load()
                .is_empty()
        );
    }

    #[test]
    fn app_files_live_below_the_apps_paths() {
        let dir = tempfile::tempdir().unwrap();
        let files = AppFiles::at(AppPaths::under(dir.path(), "so.tilo.example"));
        let data = files.data().unwrap();
        tilo_files::blocking::write_atomic(&data, &tilo_files::VPath::new("a.txt").unwrap(), b"x")
            .unwrap();
        assert!(dir.path().join("so.tilo.example/data/a.txt").is_file());
        assert_eq!(
            AppFiles::for_app("so.tilo.example").paths().id(),
            "so.tilo.example"
        );
    }

    /// A write to one settings file from off a UI thread stores the changes
    /// a UI thread queued for that file first, and leaves another file's
    /// queued changes to the background that stores them.
    #[test]
    fn an_off_thread_write_stores_only_its_own_files_queued_changes() {
        fn values(key: &str, value: i64) -> Map<String, Value> {
            [(key.to_owned(), Value::from(value))].into_iter().collect()
        }
        let dir = tempfile::tempdir().unwrap();
        let (mine_path, other_path) = (dir.path().join("mine.json"), dir.path().join("other.json"));
        let mine = SettingsDocument::File(mine_path.clone());
        let other = SettingsDocument::File(other_path.clone());
        let work = crate::rt::background::Pumped::new();
        let background = work.background();
        let (queued_mine, queued_other) = (mine.clone(), other.clone());
        std::thread::spawn(move || {
            crate::rt::mark_ui_thread();
            let _work = background.enter();
            queued_mine
                .update_section("s", values("queued", 1))
                .unwrap();
            queued_other
                .update_section("s", values("queued", 2))
                .unwrap();
        })
        .join()
        .unwrap();

        other.update_section("s", values("direct", 3)).unwrap();
        let stored = settings_store::load_section_at(&other_path, "s");
        assert_eq!(
            stored.get("queued"),
            Some(&Value::from(2)),
            "its queued change first"
        );
        assert_eq!(stored.get("direct"), Some(&Value::from(3)));
        assert!(
            settings_at(&mine_path).is_empty(),
            "another file's queued change waits for its own background"
        );
        assert_eq!(
            mine.section("s").get("queued"),
            Some(&Value::from(1)),
            "and reads as made"
        );

        work.run_pending();
        assert_eq!(
            settings_store::load_section_at(&mine_path, "s").get("queued"),
            Some(&Value::from(1)),
            "the background stores it"
        );
    }
}

/// One open window of an app, as its other windows can tell, in every
/// process: each holds a shared lock on one file. An app whose windows are
/// processes of their own (each Browser tab) learns from it when its last
/// window closes, the moment "the app closes" means.
#[derive(Debug)]
pub struct OpenWindow {
    lock: advisory_lock::FileLock,
    path: PathBuf,
}

impl OpenWindow {
    /// Count this window among those counted in `dir`, which exists.
    ///
    /// # Errors
    /// Returns an error when the lock file cannot be opened or locked.
    pub fn open(dir: &Path) -> std::io::Result<Self> {
        let path = dir.join("open-windows.lock");
        let lock = advisory_lock::FileLock::open(&path, advisory_lock::LockMode::Shared)?;
        Ok(Self { lock, path })
    }

    /// Stop counting this window. Returns whether it was the last: no other
    /// window holds the lock once this one lets go of it.
    #[must_use]
    pub fn close(self) -> bool {
        let Self { lock, path } = self;
        drop(lock);
        advisory_lock::FileLock::try_open(&path, advisory_lock::LockMode::Exclusive).is_ok()
    }
}

#[cfg(test)]
mod open_window_tests {
    use super::*;

    #[test]
    fn the_last_window_to_close_is_the_app_closing() {
        let dir = tempfile::tempdir().unwrap();
        let first = OpenWindow::open(dir.path()).unwrap();
        let second = OpenWindow::open(dir.path()).unwrap();
        assert!(!first.close(), "a window closes, another stays open");
        let third = OpenWindow::open(dir.path()).unwrap();
        assert!(!second.close());
        assert!(third.close(), "the last one closes: the app is closing");
        let again = OpenWindow::open(dir.path()).unwrap();
        assert!(again.close(), "an app opened later is counted afresh");
    }
}
