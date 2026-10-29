//! One profile's settings, kept as two Automerge documents.
//!
//! A [`Store`] holds the person's document (the leaves
//! [`crate::continuity::classify`] calls personal) and this device's
//! document (every other leaf), in `<root>/records/person.automerge` and
//! `<root>/records/device.automerge`. Each section is a map at the document's
//! root and each leaf a key in it; a value is stored as what it is (an object
//! a map, an array a list, a string, number, boolean or `null` a scalar), the
//! way `tilo_resource` stores JSON, so concurrent edits of different keys
//! merge and a typed model can read the same document. Readers see the
//! device document with the person's leaves laid over it.
//!
//! A write changes the documents in memory, commits it with its
//! [`ChangeOrigin`] as the change's message, and shows it to readers at once;
//! one background thread saves the documents. No write waits for the disk,
//! so a thread that must keep answering input may write. A save that fails
//! is reported ([`crate::take_deferred_failures`]) and retried with the next
//! change: the documents keep the change.
//!
//! One process holds a profile at a time (the session, for the person's):
//! it takes `<root>/records/settings.lock` for as long as the store is open.
//! Any other process opening the profile reads the same documents and
//! follows the holder's saves; its writes go to the holder over
//! `<root>/records/settings.sock`, which applies them as its own and saves
//! before it answers. Nothing else stands in for the documents: there is no
//! copy to keep in step.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};

use automerge::transaction::{CommitOptions, Transactable as _};
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc as _, ScalarValue};
use serde_json::{Map, Value};
use tilo_lock::MutexRecover as _;

use crate::continuity::{SettingScope, classify};
use crate::{ChangeOrigin, ConditionalUpdate, SectionChange, SettingsSource, merge_section};

/// The directory below a profile's root holding its documents.
pub const RECORDS_DIR: &str = "records";
const PERSON_FILE: &str = "person.automerge";
const DEVICE_FILE: &str = "device.automerge";
const LOCK_FILE: &str = "settings.lock";
const SOCKET_FILE: &str = "settings.sock";

/// What a write makes of the section it found: the leaves to merge, or
/// `None` when the writer's intent was superseded.
type Decide<'a> = dyn FnMut(&Map<String, Value>) -> Option<Map<String, Value>> + 'a;

/// What a write makes of the whole document: the sections to merge, or
/// `None` when the writer's intent was superseded.
type DecideSections<'a> =
    dyn FnMut(&Map<String, Value>) -> Option<Vec<(String, Map<String, Value>)>> + 'a;

/// Which of the two documents a leaf lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Holder {
    Person,
    Device,
}

fn holder_of(section: &str, leaf: &str) -> Holder {
    if classify(&format!("tilo.{section}.{leaf}")) == Some(SettingScope::Personal) {
        Holder::Person
    } else {
        Holder::Device
    }
}

/// What tells two saves of a document file apart without reading it.
type FileIdentity = Option<(u64, std::time::SystemTime)>;

fn file_identity(path: &Path) -> FileIdentity {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

struct Docs {
    person: AutoCommit,
    device: AutoCommit,
    /// The sections changed since the documents were last saved.
    unsaved: std::collections::BTreeSet<String>,
    /// What a follower last loaded, to notice the holder's next save.
    seen: (FileIdentity, FileIdentity),
}

impl Docs {
    fn doc(&mut self, holder: Holder) -> &mut AutoCommit {
        match holder {
            Holder::Person => &mut self.person,
            Holder::Device => &mut self.device,
        }
    }

    fn projection(&self) -> Map<String, Value> {
        let mut document = read_root(&self.device);
        for (section, leaves) in read_root(&self.person) {
            if let Value::Object(leaves) = leaves {
                merge_section(&mut document, &section, leaves);
            }
        }
        document
    }
}

struct Snapshot {
    document: Arc<Map<String, Value>>,
}

/// One profile's settings: see the module docs.
pub struct Store {
    me: Weak<Store>,
    root: PathBuf,
    /// Another process holds the profile: this one follows its saves.
    follower: std::sync::atomic::AtomicBool,
    /// Opened to hold the profile: takes it over when its holder is gone.
    may_hold: bool,
    /// The holder's lock; `None` for a follower.
    lock: Mutex<Option<tilo_files::Lock>>,
    /// Why this process may not write (a follower, an unisolated test).
    refused: Mutex<Option<String>>,
    docs: Mutex<Docs>,
    snapshot: Mutex<Snapshot>,
    revision: AtomicU64,
    changes: crate::ChangeSignal,
    failures: Mutex<Vec<crate::DeferredFailure>>,
    #[cfg(feature = "watch")]
    watch: Mutex<Option<tilo_files::native::ChangeWatch>>,
    /// The holder takes other processes' writes on this socket.
    #[cfg(unix)]
    serving: Mutex<Option<PathBuf>>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("root", &self.root)
            .field("holder", &self.lock.lock_recover().is_some())
            .finish_non_exhaustive()
    }
}

fn load_doc(path: &Path) -> std::io::Result<AutoCommit> {
    match std::fs::read(path) {
        Ok(bytes) => AutoCommit::load(&bytes).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{}: {error}", path.display()),
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(AutoCommit::new()),
        Err(error) => Err(error),
    }
}

impl Store {
    /// Open the profile at `root`: as its holder when no other process holds
    /// it, else read-only, following the holder.
    ///
    /// # Errors
    /// A document exists and cannot be read.
    pub fn open(root: &Path) -> std::io::Result<Arc<Self>> {
        Self::open_as(root, true)
    }

    /// Open the profile at `root` read-only, following whoever holds it, for
    /// a process that only reads settings (a service the session starts)
    /// and must never keep the session from holding them.
    ///
    /// # Errors
    /// A document exists and cannot be read.
    pub fn follow(root: &Path) -> std::io::Result<Arc<Self>> {
        Self::open_as(root, false)
    }

    fn open_as(root: &Path, hold: bool) -> std::io::Result<Arc<Self>> {
        let records = root.join(RECORDS_DIR);
        let lock_path = records.join(LOCK_FILE);
        let lock = if hold {
            Self::try_hold(&lock_path)
        } else {
            Locked::Busy
        };
        Self::finish_open(root, &records, lock, hold)
    }

    /// The lock, once the process holding it lets it go: for a holder whose
    /// door closed, which it does only as it ends.
    fn wait_to_hold(lock_path: &Path) -> Locked {
        match tilo_files::file_at(lock_path).and_then(|(dir, name)| {
            tilo_files::blocking::lock(&dir, &name, tilo_files::LockMode::Exclusive)
        }) {
            Ok(lock) => Locked::Held(lock),
            Err(_) => Locked::Busy,
        }
    }

    fn try_hold(lock_path: &Path) -> Locked {
        match tilo_files::native::lock::prepare_parent(lock_path)
            .ok()
            .and_then(|()| tilo_files::file_at(lock_path).ok())
        {
            Some((dir, name)) => {
                match tilo_files::blocking::try_lock(&dir, &name, tilo_files::LockMode::Exclusive) {
                    Ok(Some(lock)) => Locked::Held(lock),
                    Ok(None) => Locked::Busy,
                    // No locks here (the web): nobody else can hold it.
                    Err(_) => Locked::Unlockable,
                }
            }
            // No directory to keep it in: in memory, held.
            None => Locked::Unlockable,
        }
    }

    fn finish_open(
        root: &Path,
        records: &Path,
        lock: Locked,
        may_hold: bool,
    ) -> std::io::Result<Arc<Self>> {
        let person_path = records.join(PERSON_FILE);
        let device_path = records.join(DEVICE_FILE);
        let docs = Docs {
            person: load_doc(&person_path)?,
            device: load_doc(&device_path)?,
            unsaved: std::collections::BTreeSet::new(),
            seen: (file_identity(&person_path), file_identity(&device_path)),
        };
        let document = Arc::new(docs.projection());
        let (lock, refused) = match lock {
            Locked::Held(lock) => (Some(lock), None),
            Locked::Unlockable => (None, None),
            Locked::Busy => (
                None,
                Some("these settings are held by the session".to_owned()),
            ),
        };
        let follower = refused.is_some();
        let store = Arc::new_cyclic(|me| Self {
            me: me.clone(),
            root: root.to_path_buf(),
            follower: std::sync::atomic::AtomicBool::new(follower),
            may_hold,
            lock: Mutex::new(lock),
            refused: Mutex::new(refused),
            docs: Mutex::new(docs),
            snapshot: Mutex::new(Snapshot { document }),
            revision: AtomicU64::new(1),
            changes: crate::ChangeSignal::default(),
            failures: Mutex::new(Vec::new()),
            #[cfg(feature = "watch")]
            watch: Mutex::new(None),
            #[cfg(unix)]
            serving: Mutex::new(None),
        });
        #[cfg(unix)]
        if store.lock.lock_recover().is_some() {
            serve::start(&store, &records.join(SOCKET_FILE));
        }
        #[cfg(feature = "watch")]
        if follower {
            let weak = Arc::downgrade(&store);
            if let Ok(watch) = tilo_files::native::watch_changes(
                records,
                |path| {
                    path.file_name()
                        .is_some_and(|name| name == PERSON_FILE || name == DEVICE_FILE)
                },
                move || {
                    if let Some(store) = weak.upgrade() {
                        store.take_in_saves();
                    }
                },
            ) {
                *store.watch.lock_recover() = Some(watch);
            }
        }
        Ok(store)
    }

    fn is_follower(&self) -> bool {
        self.follower.load(Ordering::Acquire)
    }

    /// A follower opened to hold the profile takes it over when its holder
    /// is gone (a session that ended): from what the holder saved, as the
    /// holder from now on. Whether this process now holds it.
    fn take_over(&self, wait: bool) -> bool {
        if !self.may_hold || !self.is_follower() {
            return false;
        }
        let records = self.records();
        let lock_path = records.join(LOCK_FILE);
        let held = if wait {
            Self::wait_to_hold(&lock_path)
        } else {
            Self::try_hold(&lock_path)
        };
        let Locked::Held(lock) = held else {
            return false;
        };
        let (person_path, device_path) = (records.join(PERSON_FILE), records.join(DEVICE_FILE));
        let (Ok(person), Ok(device)) = (load_doc(&person_path), load_doc(&device_path)) else {
            return false;
        };
        let mut docs = self.docs.lock_recover();
        docs.person = person;
        docs.device = device;
        docs.seen = (file_identity(&person_path), file_identity(&device_path));
        let document = Arc::new(docs.projection());
        drop(docs);
        *self.lock.lock_recover() = Some(lock);
        self.follower.store(false, Ordering::Release);
        *self.refused.lock_recover() = None;
        self.publish(document);
        #[cfg(unix)]
        if let Some(me) = self.me.upgrade() {
            serve::start(&me, &records.join(SOCKET_FILE));
        }
        true
    }

    /// The profile's root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether this process holds the profile (and may write it).
    #[must_use]
    pub fn is_holder(&self) -> bool {
        self.refused.lock_recover().is_none()
    }

    /// Refuse every write from now on, saying `why`.
    pub(crate) fn refuse_writes(&self, why: &str) {
        *self.refused.lock_recover() = Some(why.to_owned());
    }

    fn records(&self) -> PathBuf {
        self.root.join(RECORDS_DIR)
    }

    /// The whole document as readers see it.
    #[must_use]
    pub fn document(&self) -> Arc<Map<String, Value>> {
        self.take_in_saves();
        Arc::clone(&self.snapshot.lock_recover().document)
    }

    /// A follower takes in what the holder saved since it last looked.
    fn take_in_saves(&self) {
        if !self.is_follower() {
            return;
        }
        let records = self.records();
        let (person_path, device_path) = (records.join(PERSON_FILE), records.join(DEVICE_FILE));
        let now = (file_identity(&person_path), file_identity(&device_path));
        let mut docs = self.docs.lock_recover();
        if docs.seen == now {
            return;
        }
        let (Ok(person), Ok(device)) = (load_doc(&person_path), load_doc(&device_path)) else {
            // A save half seen: the holder's rename lands whole on the next look.
            return;
        };
        // A follower writes nothing of its own: the holder's save is the
        // whole of both documents.
        docs.person = person;
        docs.device = device;
        docs.seen = now;
        let document = Arc::new(docs.projection());
        drop(docs);
        self.publish(document);
    }

    fn publish(&self, document: Arc<Map<String, Value>>) {
        let mut snapshot = self.snapshot.lock_recover();
        if *snapshot.document == *document {
            return;
        }
        snapshot.document = document;
        self.revision.fetch_add(1, Ordering::AcqRel);
        drop(snapshot);
        self.changes.ring();
    }

    /// Under the documents' lock: let `decide` look at section `name` as it
    /// stands and write what it returns, as one change made by `origin`.
    fn write_where(
        &self,
        name: &str,
        origin: &ChangeOrigin,
        decide: &mut Decide<'_>,
    ) -> std::io::Result<ConditionalUpdate> {
        self.write_sections(origin, &mut |document| {
            let empty = Map::new();
            let current = match document.get(name) {
                Some(Value::Object(section)) => section,
                _ => &empty,
            };
            decide(current).map(|values| vec![(name.to_owned(), values)])
        })
    }

    /// Under the documents' lock: let `decide` look at the whole document
    /// and merge the sections it returns, as one change made by `origin`.
    fn write_sections(
        &self,
        origin: &ChangeOrigin,
        decide: &mut DecideSections<'_>,
    ) -> std::io::Result<ConditionalUpdate> {
        let refused = self.refused.lock_recover().clone();
        if let Some(why) = refused
            && !self.take_over(false)
        {
            #[cfg(unix)]
            if self.is_follower() {
                return self.forward(origin, decide, &why);
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                why,
            ));
        }
        let mut docs = self.docs.lock_recover();
        let document = docs.projection();
        let Some(sections) = decide(&document) else {
            return Ok(ConditionalUpdate::Superseded);
        };
        let mut touched: Vec<(Holder, String, String, Value)> = Vec::new();
        for (name, values) in sections {
            let current = document.get(&name).and_then(Value::as_object);
            for (leaf, value) in values {
                if current.and_then(|section| section.get(&leaf)) == Some(&value) {
                    continue;
                }
                let holder = holder_of(&name, &leaf);
                let doc = docs.doc(holder);
                let section = map_at(doc, &ROOT, &name).map_err(std::io::Error::other)?;
                write_value(doc, &section, &leaf, &value).map_err(std::io::Error::other)?;
                touched.push((holder, name.clone(), leaf, value));
            }
        }
        if touched.is_empty() {
            return Ok(ConditionalUpdate::Stored);
        }
        let message = change_message(origin, &touched, CHANGES.fetch_add(1, Ordering::Relaxed));
        let time = i64::try_from(tilo_clock::unix_now_ms() / 1000).unwrap_or(i64::MAX);
        for holder in [Holder::Person, Holder::Device] {
            if touched.iter().any(|(h, ..)| *h == holder) {
                docs.doc(holder).commit_with(
                    CommitOptions::default()
                        .with_message(message.clone())
                        .with_time(time),
                );
            }
        }
        docs.unsaved
            .extend(touched.iter().map(|(_, section, ..)| section.clone()));
        let document = Arc::new(docs.projection());
        drop(docs);
        self.publish(document);
        writer().schedule(&self.me);
        Ok(ConditionalUpdate::Stored)
    }

    /// Save the documents if they changed since they were last saved.
    fn save(&self) {
        let mut docs = self.docs.lock_recover();
        if docs.unsaved.is_empty() {
            return;
        }
        let sections = std::mem::take(&mut docs.unsaved);
        let person = docs.person.save();
        let device = docs.device.save();
        drop(docs);
        let records = self.records();
        let saved = tilo_files::native::atomic::write(&records.join(PERSON_FILE), &person)
            .and_then(|()| tilo_files::native::atomic::write(&records.join(DEVICE_FILE), &device));
        if let Err(error) = saved {
            tracing::warn!(root = %self.root.display(), %error, "settings not saved");
            self.docs
                .lock_recover()
                .unsaved
                .extend(sections.iter().cloned());
            self.failures.lock_recover().push(crate::DeferredFailure {
                sections: sections.into_iter().collect(),
                error: error.to_string(),
            });
            crate::note_deferred_failure();
        }
    }

    /// Who changed which leaf, oldest first: one object per changed leaf
    /// with `at_ms`, `section`, `leaf`, `after`, `action`, `principal` and
    /// `invoked_by`, read from the documents' history.
    #[must_use]
    pub fn change_log(&self) -> Vec<Value> {
        let mut docs = self.docs.lock_recover();
        // A change to both documents is recorded in each: once here.
        let mut changes: BTreeMap<(u64, u64), Value> = BTreeMap::new();
        for holder in [Holder::Person, Holder::Device] {
            for change in docs.doc(holder).get_changes_meta(&[]) {
                let Some(message) = change
                    .message
                    .as_deref()
                    .and_then(|m| serde_json::from_str::<Value>(m).ok())
                else {
                    continue;
                };
                let number = |key: &str| message.get(key).and_then(Value::as_u64).unwrap_or(0);
                changes.insert((number("at_ms"), number("sequence")), message);
            }
        }
        let field = |value: &Value, key: &str| value.get(key).cloned().unwrap_or_default();
        let mut entries = Vec::new();
        for message in changes.values() {
            let leaves = message.get("leaves").and_then(Value::as_array);
            for leaf in leaves.into_iter().flatten() {
                entries.push(serde_json::json!({
                    "at_ms": field(message, "at_ms"),
                    "section": field(leaf, "section"),
                    "leaf": field(leaf, "leaf"),
                    "after": field(leaf, "after"),
                    "action": field(message, "action"),
                    "principal": field(message, "principal"),
                    "invoked_by": field(message, "invoked_by"),
                }));
            }
        }
        entries
    }
}

#[cfg(unix)]
impl Store {
    /// A follower's write: decided over what it reads now, made by the
    /// holder, and read back from the holder's save.
    fn forward(
        &self,
        origin: &ChangeOrigin,
        decide: &mut DecideSections<'_>,
        why: &str,
    ) -> std::io::Result<ConditionalUpdate> {
        let document = self.document();
        let Some(sections) = decide(&document) else {
            return Ok(ConditionalUpdate::Superseded);
        };
        let expected: Vec<(String, Option<Value>)> = sections
            .iter()
            .map(|(name, _)| (name.clone(), document.get(name).cloned()))
            .collect();
        let request = serve::Request {
            origin: origin.clone(),
            expected,
            sections,
        };
        let answer = match serve::send(&self.records().join(SOCKET_FILE), &request) {
            Ok(answer) => answer,
            // The holder's door closes only as the holder ends: once it has
            // let the settings go, this process holds them and makes the
            // change itself.
            // A holder with no door at all (`NotFound`) is not waited for.
            Err(error) if error.kind() != std::io::ErrorKind::NotFound && self.take_over(true) => {
                let serve::Request {
                    expected, sections, ..
                } = request;
                let mut sections = Some(sections);
                return self.write_sections(origin, &mut |document| {
                    expected
                        .iter()
                        .all(|(name, value)| document.get(name) == value.as_ref())
                        .then(|| sections.take().unwrap_or_default())
                });
            }
            Err(error) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("{why}, which did not take the change: {error}"),
                ));
            }
        };
        self.take_in_saves();
        Ok(answer)
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        #[cfg(unix)]
        let serving = self.serving.lock_recover().take();
        if let Some(socket) = serving {
            // Wakes the serving thread, which finds the store gone and ends.
            drop(std::os::unix::net::UnixStream::connect(&socket));
            let _ = std::fs::remove_file(socket);
        }
    }
}

/// The holder's door for other processes' writes.
#[cfg(unix)]
mod serve {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;
    use std::sync::{Arc, Weak};

    use serde_json::{Map, Value};
    use tilo_lock::MutexRecover as _;

    use super::Store;
    use crate::{ChangeOrigin, ConditionalUpdate};

    /// The largest request a holder reads: settings are small.
    const MAX_REQUEST: u64 = 4 * 1024 * 1024;

    /// One write: merge `sections`, provided each section still reads as
    /// `expected` (what the writer decided over).
    #[derive(serde::Serialize, serde::Deserialize)]
    pub(super) struct Request {
        pub(super) origin: ChangeOrigin,
        pub(super) expected: Vec<(String, Option<Value>)>,
        pub(super) sections: Vec<(String, Map<String, Value>)>,
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    enum Answer {
        Stored,
        Superseded,
        Refused(String),
    }

    pub(super) fn start(store: &Arc<Store>, socket: &Path) {
        let _ = std::fs::remove_file(socket);
        let Ok(listener) = UnixListener::bind(socket) else {
            tracing::warn!(socket = %socket.display(), "settings take no writes from other processes");
            return;
        };
        let weak = Arc::downgrade(store);
        let started = std::thread::Builder::new()
            .name("tilo-settings-door".into())
            .spawn(move || accept(&listener, &weak));
        if started.is_ok() {
            *store.serving.lock_recover() = Some(socket.to_path_buf());
        }
    }

    fn accept(listener: &UnixListener, store: &Weak<Store>) {
        for stream in listener.incoming() {
            let Some(store) = store.upgrade() else {
                return;
            };
            if let Ok(stream) = stream {
                answer(&store, &stream);
            }
        }
    }

    fn answer(store: &Store, stream: &UnixStream) {
        let mut line = String::new();
        let mut reader = BufReader::new(std::io::Read::take(stream, MAX_REQUEST));
        if reader.read_line(&mut line).is_err() {
            return;
        }
        let answer = match serde_json::from_str::<Request>(&line) {
            Ok(request) => apply(store, request),
            Err(error) => Answer::Refused(error.to_string()),
        };
        if let Ok(mut body) = serde_json::to_vec(&answer) {
            body.push(b'\n');
            let _ = (&*stream).write_all(&body);
        }
    }

    fn apply(store: &Store, request: Request) -> Answer {
        let Request {
            origin,
            expected,
            sections,
        } = request;
        let mut sections = Some(sections);
        let stored = store.write_sections(&origin, &mut |document| {
            expected
                .iter()
                .all(|(name, value)| document.get(name) == value.as_ref())
                .then(|| sections.take().unwrap_or_default())
        });
        // Saved before the answer, so the writer reads its change back.
        crate::SettingsSource::flush_pending(store);
        match stored {
            Ok(ConditionalUpdate::Stored) => Answer::Stored,
            Ok(ConditionalUpdate::Superseded) => Answer::Superseded,
            Err(error) => Answer::Refused(error.to_string()),
        }
    }

    pub(super) fn send(socket: &Path, request: &Request) -> std::io::Result<ConditionalUpdate> {
        let stream = UnixStream::connect(socket)?;
        let mut body = serde_json::to_vec(request).map_err(std::io::Error::other)?;
        body.push(b'\n');
        (&stream).write_all(&body)?;
        let mut line = String::new();
        BufReader::new(std::io::Read::take(&stream, MAX_REQUEST)).read_line(&mut line)?;
        match serde_json::from_str::<Answer>(&line).map_err(std::io::Error::other)? {
            Answer::Stored => Ok(ConditionalUpdate::Stored),
            Answer::Superseded => Ok(ConditionalUpdate::Superseded),
            Answer::Refused(why) => Err(std::io::Error::other(why)),
        }
    }
}

enum Locked {
    Held(tilo_files::Lock),
    Busy,
    Unlockable,
}

/// What a change records of itself: the leaves it set, their new values and
/// what made it.
/// Orders the changes of one process across both documents.
static CHANGES: AtomicU64 = AtomicU64::new(0);

fn change_message(
    origin: &ChangeOrigin,
    touched: &[(Holder, String, String, Value)],
    sequence: u64,
) -> String {
    serde_json::json!({
        "at_ms": tilo_clock::unix_now_ms(),
        "sequence": sequence,
        "leaves": touched
            .iter()
            .map(|(_, section, leaf, after)| {
                serde_json::json!({"section": section, "leaf": leaf, "after": after})
            })
            .collect::<Vec<_>>(),
        "action": origin.action,
        "principal": origin.principal,
        "invoked_by": origin.invoked_by,
    })
    .to_string()
}

impl SettingsSource for Store {
    fn load(&self) -> Map<String, Value> {
        (*self.document()).clone()
    }

    fn update_section(&self, name: &str, values: Map<String, Value>) -> std::io::Result<()> {
        self.update_section_by(name, values, &ChangeOrigin::default())
    }

    fn update_section_by(
        &self,
        name: &str,
        values: Map<String, Value>,
        origin: &ChangeOrigin,
    ) -> std::io::Result<()> {
        let mut values = Some(values);
        self.write_where(name, origin, &mut |_| values.take())
            .map(drop)
    }

    fn update_section_with(
        &self,
        name: &str,
        origin: &ChangeOrigin,
        change: &mut SectionChange<'_>,
    ) -> std::io::Result<()> {
        self.write_where(name, origin, &mut |current| Some(change(current)))
            .map(drop)
    }

    fn compare_update_section(
        &self,
        name: &str,
        expected: &Map<String, Value>,
        values: Map<String, Value>,
    ) -> std::io::Result<ConditionalUpdate> {
        let mut values = Some(values);
        self.write_where(name, &ChangeOrigin::default(), &mut |current| {
            (current == expected).then(|| values.take().unwrap_or_default())
        })
    }

    fn compare_display_update(
        &self,
        update: crate::DisplayUpdate,
    ) -> std::io::Result<ConditionalUpdate> {
        let sections = update.clone().sections();
        if sections.iter().any(|(section, values)| {
            values
                .keys()
                .any(|leaf| holder_of(section, leaf) == Holder::Person)
        }) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "display intent must stay in one device record",
            ));
        }
        let mut sections = Some(sections);
        self.write_sections(&ChangeOrigin::default(), &mut |document| {
            update.matches(document).then(|| {
                sections
                    .take()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(name, values)| (name.to_owned(), values))
                    .collect()
            })
        })
    }

    fn revision(&self) -> u64 {
        self.take_in_saves();
        self.revision.load(Ordering::Acquire)
    }

    fn flush_pending(&self) {
        writer().flush(self);
    }

    fn take_deferred_failures(&self) -> Vec<crate::DeferredFailure> {
        std::mem::take(&mut *self.failures.lock_recover())
    }

    fn changes(&self) -> &crate::ChangeSignal {
        &self.changes
    }
}

/// The one thread that saves every store of the process.
struct Writer {
    queue: Mutex<WriterQueue>,
    wake: Condvar,
}

#[derive(Default)]
struct WriterQueue {
    scheduled: Vec<Weak<Store>>,
    /// A store is being saved.
    busy: bool,
    /// The thread is running; `false` where threads cannot start (the web),
    /// which saves on the writing thread instead.
    running: bool,
}

fn writer() -> &'static Writer {
    static WRITER: OnceLock<Writer> = OnceLock::new();
    WRITER.get_or_init(|| Writer {
        queue: Mutex::new(WriterQueue::default()),
        wake: Condvar::new(),
    })
}

impl Writer {
    fn schedule(&'static self, store: &Weak<Store>) {
        let mut queue = self.queue.lock_recover();
        if !queue.scheduled.iter().any(|known| known.ptr_eq(store)) {
            queue.scheduled.push(store.clone());
        }
        if !queue.running {
            let started = std::thread::Builder::new()
                .name("tilo-settings-writer".into())
                .spawn(move || self.run());
            if started.is_err() {
                queue.scheduled.retain(|known| !known.ptr_eq(store));
                drop(queue);
                if let Some(store) = store.upgrade() {
                    store.save();
                }
                return;
            }
            queue.running = true;
        }
        drop(queue);
        self.wake.notify_all();
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the queue's lock is let go around each save and held only to take the next"
    )]
    fn run(&self) {
        let mut queue = self.queue.lock_recover();
        loop {
            if queue.scheduled.is_empty() {
                queue = self
                    .wake
                    .wait(queue)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                continue;
            }
            let next = queue.scheduled.remove(0);
            queue.busy = true;
            drop(queue);
            if let Some(store) = next.upgrade() {
                store.save();
            }
            queue = self.queue.lock_recover();
            queue.busy = false;
            self.wake.notify_all();
        }
    }

    /// Wait until `store` has nothing left to save.
    fn flush(&self, store: &Store) {
        let mut queue = self.queue.lock_recover();
        while queue.busy || queue.scheduled.iter().any(|known| known.ptr_eq(&store.me)) {
            queue = self
                .wake
                .wait(queue)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        drop(queue);
        // A save that failed is still unsaved: try once more here.
        store.save();
    }
}

/// The profile at `root`, opened once per process.
///
/// # Panics
/// Never: a profile whose documents cannot be read opens empty and refuses
/// writes, so nothing overwrites what could not be read.
#[must_use]
pub fn profile(root: &Path) -> Arc<Store> {
    static PROFILES: Mutex<BTreeMap<PathBuf, Arc<Store>>> = Mutex::new(BTreeMap::new());
    let mut profiles = PROFILES.lock_recover();
    if let Some(store) = profiles.get(root) {
        return Arc::clone(store);
    }
    let store = Store::open(root).unwrap_or_else(|error| {
        tracing::warn!(root = %root.display(), %error, "settings unreadable; not writing them");
        let store = empty(root);
        store.refuse_writes(&format!("the settings could not be read: {error}"));
        store
    });
    profiles.insert(root.to_path_buf(), Arc::clone(&store));
    store
}

/// An empty profile at `root` kept in memory only.
fn empty(root: &Path) -> Arc<Store> {
    Arc::new_cyclic(|me| Store {
        me: me.clone(),
        root: root.to_path_buf(),
        follower: std::sync::atomic::AtomicBool::new(false),
        may_hold: false,
        lock: Mutex::new(None),
        refused: Mutex::new(None),
        docs: Mutex::new(Docs {
            person: AutoCommit::new(),
            device: AutoCommit::new(),
            unsaved: std::collections::BTreeSet::new(),
            seen: (None, None),
        }),
        snapshot: Mutex::new(Snapshot {
            document: Arc::new(Map::new()),
        }),
        revision: AtomicU64::new(1),
        changes: crate::ChangeSignal::default(),
        failures: Mutex::new(Vec::new()),
        #[cfg(feature = "watch")]
        watch: Mutex::new(None),
        #[cfg(unix)]
        serving: Mutex::new(None),
    })
}

// ----- JSON values as the document's own maps, lists and scalars -----

fn scalar_of(value: &Value) -> ScalarValue {
    match value {
        Value::Bool(flag) => ScalarValue::Boolean(*flag),
        Value::String(text) => ScalarValue::Str(text.as_str().into()),
        Value::Number(number) => number.as_i64().map_or_else(
            || {
                number.as_u64().map_or_else(
                    || ScalarValue::F64(number.as_f64().unwrap_or_default()),
                    ScalarValue::Uint,
                )
            },
            ScalarValue::Int,
        ),
        Value::Null | Value::Array(_) | Value::Object(_) => ScalarValue::Null,
    }
}

fn json_of_scalar(scalar: &ScalarValue) -> Value {
    match scalar {
        ScalarValue::Boolean(flag) => Value::Bool(*flag),
        ScalarValue::Str(text) => Value::String(text.to_string()),
        ScalarValue::Int(int) => Value::from(*int),
        ScalarValue::Uint(uint) => Value::from(*uint),
        ScalarValue::F64(float) => {
            serde_json::Number::from_f64(*float).map_or(Value::Null, Value::Number)
        }
        ScalarValue::Counter(counter) => Value::from(i64::from(counter)),
        ScalarValue::Timestamp(at) => Value::from(*at),
        _ => Value::Null,
    }
}

fn read_value(doc: &AutoCommit, value: &automerge::Value<'_>, id: &ObjId) -> Value {
    match value {
        automerge::Value::Scalar(scalar) => json_of_scalar(scalar),
        automerge::Value::Object(ObjType::Text) => doc.text(id).map_or(Value::Null, Value::String),
        automerge::Value::Object(ObjType::Map | ObjType::Table) => Value::Object(read_map(doc, id)),
        automerge::Value::Object(ObjType::List) => Value::Array(
            (0..doc.length(id))
                .map(|at| match doc.get(id, at) {
                    Ok(Some((value, child))) => read_value(doc, &value, &child),
                    _ => Value::Null,
                })
                .collect(),
        ),
    }
}

fn read_map(doc: &AutoCommit, obj: &ObjId) -> Map<String, Value> {
    doc.keys(obj)
        .filter_map(|key| {
            let (value, id) = doc.get(obj, key.as_str()).ok()??;
            Some((key, read_value(doc, &value, &id)))
        })
        .collect()
}

fn read_root(doc: &AutoCommit) -> Map<String, Value> {
    read_map(doc, &ROOT)
}

/// The map at `key` of `obj`, made where something else or nothing is there.
fn map_at(
    doc: &mut AutoCommit,
    obj: &ObjId,
    key: &str,
) -> Result<ObjId, automerge::AutomergeError> {
    match doc.get(obj, key)? {
        Some((automerge::Value::Object(ObjType::Map), id)) => Ok(id),
        _ => doc.put_object(obj, key, ObjType::Map),
    }
}

/// Make `key` of the map `obj` hold `value`, changing only what differs.
fn write_value(
    doc: &mut AutoCommit,
    obj: &ObjId,
    key: &str,
    value: &Value,
) -> Result<(), automerge::AutomergeError> {
    match value {
        Value::Object(fields) => {
            let map = map_at(doc, obj, key)?;
            let stale: Vec<String> = doc.keys(&map).filter(|k| !fields.contains_key(k)).collect();
            for gone in stale {
                doc.delete(&map, gone.as_str())?;
            }
            for (field, value) in fields {
                write_value(doc, &map, field, value)?;
            }
        }
        Value::Array(items) => {
            let same = match doc.get(obj, key)? {
                Some((found, id)) => read_value(doc, &found, &id) == *value,
                None => false,
            };
            if !same {
                let list = doc.put_object(obj, key, ObjType::List)?;
                for (at, item) in items.iter().enumerate() {
                    insert_value(doc, &list, at, item)?;
                }
            }
        }
        scalar => {
            let same = match doc.get(obj, key)? {
                Some((automerge::Value::Scalar(found), _)) => json_of_scalar(&found) == *scalar,
                _ => false,
            };
            if !same {
                doc.put(obj, key, scalar_of(scalar))?;
            }
        }
    }
    Ok(())
}

fn insert_value(
    doc: &mut AutoCommit,
    list: &ObjId,
    at: usize,
    value: &Value,
) -> Result<(), automerge::AutomergeError> {
    match value {
        Value::Object(fields) => {
            let map = doc.insert_object(list, at, ObjType::Map)?;
            for (field, value) in fields {
                write_value(doc, &map, field, value)?;
            }
        }
        Value::Array(items) => {
            let inner = doc.insert_object(list, at, ObjType::List)?;
            for (index, item) in items.iter().enumerate() {
                insert_value(doc, &inner, index, item)?;
            }
        }
        scalar => doc.insert(list, at, scalar_of(scalar))?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => unreachable!(),
        }
    }

    #[test]
    fn writes_show_at_once_and_survive_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store
            .update_section(
                "power",
                object(json!({"blank_minutes": 5, "nested": {"a": [1, 2.5, "x"]}})),
            )
            .unwrap();
        store
            .update_section("appearance", object(json!({"text_scale": 1.25})))
            .unwrap();
        assert_eq!(store.load_section_value("power")["blank_minutes"], 5);
        store.flush_pending();
        drop(store);
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(
            Value::Object(store.load()),
            json!({
                "power": {"blank_minutes": 5, "nested": {"a": [1, 2.5, "x"]}},
                "appearance": {"text_scale": 1.25},
            })
        );
    }

    #[test]
    fn personal_and_device_leaves_go_to_their_documents() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store
            .update_section(
                "appearance",
                object(json!({"text_scale": 1.5, "not_a_reviewed_leaf": true})),
            )
            .unwrap();
        let (person, device) = {
            let docs = store.docs.lock_recover();
            (read_root(&docs.person), read_root(&docs.device))
        };
        assert_eq!(
            Value::Object(person),
            json!({"appearance": {"text_scale": 1.5}})
        );
        assert_eq!(
            Value::Object(device),
            json!({"appearance": {"not_a_reviewed_leaf": true}})
        );
    }

    #[test]
    fn a_nested_value_loses_the_keys_it_no_longer_has() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store
            .update_section("s", object(json!({"m": {"a": 1, "b": 2}})))
            .unwrap();
        store
            .update_section("s", object(json!({"m": {"b": 3}})))
            .unwrap();
        assert_eq!(store.load_section_value("s"), json!({"m": {"b": 3}}));
    }

    #[test]
    fn a_second_opener_follows_and_its_writes_go_through_the_holder() {
        let dir = tempfile::tempdir().unwrap();
        let holder = Store::open(dir.path()).unwrap();
        holder.update_section("s", object(json!({"x": 1}))).unwrap();
        holder.flush_pending();
        let follower = Store::open(dir.path()).unwrap();
        assert!(!follower.is_holder());
        assert_eq!(follower.load_section_value("s"), json!({"x": 1}));
        follower
            .update_section("s", object(json!({"x": 2})))
            .unwrap();
        assert_eq!(
            follower.load_section_value("s"),
            json!({"x": 2}),
            "read back at once"
        );
        assert_eq!(
            holder.load_section_value("s"),
            json!({"x": 2}),
            "made by the holder"
        );
        holder.update_section("s", object(json!({"x": 3}))).unwrap();
        holder.flush_pending();
        assert_eq!(follower.load_section_value("s"), json!({"x": 3}));
    }

    #[test]
    fn a_follower_opened_to_hold_takes_over_when_the_holder_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let holder = Store::open(dir.path()).unwrap();
        holder.update_section("s", object(json!({"x": 1}))).unwrap();
        holder.flush_pending();
        let follower = Store::open(dir.path()).unwrap();
        let reader = Store::follow(dir.path()).unwrap();
        drop(holder);
        follower
            .update_section("s", object(json!({"y": 2})))
            .unwrap();
        assert!(follower.is_holder());
        assert_eq!(follower.load_section_value("s"), json!({"x": 1, "y": 2}));
        follower.flush_pending();
        assert_eq!(reader.load_section_value("s"), json!({"x": 1, "y": 2}));
        reader.update_section("s", object(json!({"z": 3}))).unwrap();
        assert!(!reader.is_holder(), "a reader never holds");
        assert_eq!(follower.load_section_value("s")["z"], 3);
    }

    #[test]
    fn the_change_log_names_what_made_each_change() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store
            .update_section_by(
                "power",
                object(json!({"blank_minutes": 3})),
                &ChangeOrigin::local("settings.power/field-edit"),
            )
            .unwrap();
        let log = store.change_log();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0]["section"], "power");
        assert_eq!(log[0]["leaf"], "blank_minutes");
        assert_eq!(log[0]["action"], "settings.power/field-edit");
        assert_eq!(log[0]["principal"], ChangeOrigin::LOCAL_USER);
    }

    #[test]
    fn a_compare_update_stores_only_over_what_it_read() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let read = Map::new();
        assert!(matches!(
            store
                .compare_update_section("a", &read, object(json!({"x": 1})))
                .unwrap(),
            ConditionalUpdate::Stored
        ));
        assert!(matches!(
            store
                .compare_update_section("a", &read, object(json!({"x": 2})))
                .unwrap(),
            ConditionalUpdate::Superseded
        ));
        assert_eq!(store.load_section_value("a"), json!({"x": 1}));
    }

    impl Store {
        fn load_section_value(&self, name: &str) -> Value {
            self.load().remove(name).unwrap_or(Value::Null)
        }
    }
}
