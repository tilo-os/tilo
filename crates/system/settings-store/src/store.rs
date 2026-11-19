//! One profile's settings, kept as two Automerge documents: the person's in
//! `<root>/documents/person/` and this device's in `<root>/documents/device/`.
//!
//! The documents are kept in the device's document store (`app_state::FsStore` over
//! `<root>/documents`), the same layer an app's own documents live in: one process holds a profile
//! at a time as the documents' only writer, and its writes append what changed, which the store
//! commits on its own thread, so no write waits for the disk; a failed save is reported and
//! retried with the next change. Another process opening the profile follows the holder's saves,
//! and its writes, conditional ones included, go to the holder over `<root>/documents/settings.sock`,
//! so one process decides every change.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use automerge::transaction::{CommitOptions, Transactable as _};
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc as _};
use serde_json::{Map, Value};
use tilo_automerge::{ApplyIncoming as _, json};
use tilo_lock::MutexRecover as _;

use app_state::held::{Follow, Socket, Watch};
use app_state::{FsStore, Held, Save, SavePlan};
use automerge::ChangeHash;
use durable_store::{DurableStore as _, StorageKey, WriteOp};
use tilo_paths::{PROFILE_DIR, PROFILE_SOCKET};

use crate::placement::{self, Holder};
use crate::{ChangeOrigin, ConditionalUpdate, SectionChange, SettingsSource, merge_section};

/// The prefix of the root keys that keep a document's history (what was
/// undone) rather than a section.
pub const BOOKKEEPING: &str = "tilo:";

/// The two documents, as keys of the device's document store: the person's is leased first, and
/// its lock is the profile's ([`tilo_paths::PROFILE_LOCK`]).
const PERSON: &str = "person";
const DEVICE: &str = "device";

fn key(holder: Holder) -> StorageKey {
    StorageKey::app(match holder {
        Holder::Person => PERSON,
        Holder::Device => DEVICE,
    })
}

/// The documents in the order they are leased: the person's first, whose lock is the profile's.
fn keys() -> [StorageKey; 2] {
    [key(Holder::Person), key(Holder::Device)]
}

/// What a write makes of the section it found: the leaves to merge, or
/// `None` when the writer's intent was superseded.
type Decide<'a> = dyn FnMut(&Map<String, Value>) -> Option<Map<String, Value>> + 'a;

/// What a write makes of the whole document: the sections to merge, or
/// `None` when the writer's intent was superseded.
type DecideSections<'a> =
    dyn FnMut(&Map<String, Value>) -> Option<Vec<(String, Map<String, Value>)>> + 'a;

struct Docs {
    person: AutoCommit,
    device: AutoCommit,
    unsaved: std::collections::BTreeSet<String>,
    /// What the documents' files stood at when they were last read, for a follower.
    seen: u64,
    /// Each document's heads as last saved, and how its saves stand since it was last whole.
    saved: [(Vec<ChangeHash>, SavePlan); 2],
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

    fn loaded(mut person: AutoCommit, mut device: AutoCommit) -> Self {
        let saved = [
            (person.get_heads(), SavePlan::default()),
            (device.get_heads(), SavePlan::default()),
        ];
        Self {
            person,
            device,
            unsaved: std::collections::BTreeSet::new(),
            seen: 0,
            saved,
        }
    }

    fn keep(&mut self, sections: Vec<String>) -> Arc<Map<String, Value>> {
        self.unsaved.extend(sections);
        Arc::new(self.projection())
    }
}

struct Snapshot {
    document: Arc<Map<String, Value>>,
}

pub struct Store {
    me: Weak<Store>,
    root: PathBuf,
    /// The directory the documents live in, which the holder's socket opens in.
    dir: PathBuf,
    /// The device's document store the documents live in.
    kept: FsStore,
    /// The documents as a follower notices the holder's saves.
    follow: Follow,
    follower: std::sync::atomic::AtomicBool,
    may_hold: bool,
    /// The leases of both documents while this process holds the profile.
    lock: Mutex<Option<[Held; 2]>>,
    refused: Mutex<Option<String>>,
    docs: Mutex<Docs>,
    snapshot: Mutex<Snapshot>,
    revision: AtomicU64,
    changes: crate::ChangeSignal,
    failures: Mutex<Vec<crate::DeferredFailure>>,
    watch: Mutex<Option<Watch>>,
    socket: Mutex<Option<Socket>>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("root", &self.root)
            .field("holder", &self.lock.lock_recover().is_some())
            .finish_non_exhaustive()
    }
}

/// What holding a profile came to.
enum Hold {
    Held([Held; 2]),
    /// Another process holds it.
    Busy,
    /// Nothing here can be held (no files, or a target without locks): nobody else can hold it,
    /// so this process does, in memory.
    Unlockable,
}

fn try_hold(kept: &FsStore) -> Hold {
    let lease = |holder| kept.lease(&key(holder));
    match lease(Holder::Person) {
        Ok(person) => match lease(Holder::Device) {
            Ok(device) => Hold::Held([person, device]),
            Err(durable_store::StoreError::Busy(_)) => Hold::Busy,
            Err(_) => Hold::Unlockable,
        },
        Err(durable_store::StoreError::Busy(_)) => Hold::Busy,
        Err(_) => Hold::Unlockable,
    }
}

fn load_doc(kept: &FsStore, holder: Holder) -> std::io::Result<AutoCommit> {
    let key = key(holder);
    let stored = match kept.read(&key) {
        Ok(stored) => stored,
        // A target without files keeps nothing to read.
        Err(_) if !tilo_files::available(tilo_files::Feature::NativePaths) => None,
        Err(error) => return Err(std::io::Error::other(error.to_string())),
    };
    match stored {
        // A save the holder is appending as this reads leaves a partial last chunk: what came
        // before it is the save before, whole.
        Some(stored) => AutoCommit::load_with_options(
            &stored.bytes,
            automerge::LoadOptions::new().on_partial_load(automerge::OnPartialLoad::Ignore),
        )
        .map_err(|error| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{key}: {error}"))
        }),
        None => Ok(AutoCommit::new()),
    }
}

/// The documents as their files hold them.
fn load_docs(kept: &FsStore) -> std::io::Result<Docs> {
    Ok(Docs::loaded(
        load_doc(kept, Holder::Person)?,
        load_doc(kept, Holder::Device)?,
    ))
}

impl Store {
    /// Open the profile at `root`: as its holder when no other process holds
    /// it, else read-only, following the holder.
    pub fn open(root: &Path) -> std::io::Result<Arc<Self>> {
        Self::open_as(root, true)
    }

    /// Open the profile at `root` read-only, following whoever holds it, for
    /// a process that only reads settings (a service the session starts)
    /// and must never keep the session from holding them.
    pub fn follow(root: &Path) -> std::io::Result<Arc<Self>> {
        Self::open_as(root, false)
    }

    fn open_as(root: &Path, hold: bool) -> std::io::Result<Arc<Self>> {
        let dir = root.join(PROFILE_DIR);
        let kept = FsStore::new(&dir);
        let follow = Follow::of(&kept, &keys());
        let taken = if hold { try_hold(&kept) } else { Hold::Busy };
        let mut docs = load_docs(&kept)?;
        docs.seen = follow.stamp();
        let document = Arc::new(docs.projection());
        let (lock, refused) = match taken {
            Hold::Held(lock) => (Some(lock), None),
            Hold::Unlockable => (None, None),
            Hold::Busy => (
                None,
                Some("these settings are held by the session".to_owned()),
            ),
        };
        let follower = refused.is_some();
        let holds = lock.is_some();
        let store = Arc::new_cyclic(|me| Self {
            me: me.clone(),
            root: root.to_path_buf(),
            dir,
            kept,
            follow,
            follower: std::sync::atomic::AtomicBool::new(follower),
            may_hold: hold,
            lock: Mutex::new(lock),
            refused: Mutex::new(refused),
            docs: Mutex::new(docs),
            snapshot: Mutex::new(Snapshot { document }),
            revision: AtomicU64::new(1),
            changes: crate::ChangeSignal::default(),
            failures: Mutex::new(Vec::new()),
            watch: Mutex::new(None),
            socket: Mutex::new(None),
        });
        if holds {
            store.open_socket();
        }
        if follower {
            let weak = Arc::downgrade(&store);
            *store.watch.lock_recover() = store.follow.watch(move || {
                if let Some(store) = weak.upgrade() {
                    store.take_in_saves();
                }
            });
        }
        Ok(store)
    }

    fn open_socket(&self) {
        let weak = self.me.clone();
        let opened = Socket::open(self.dir.join(PROFILE_SOCKET), move |request| {
            let answer = match weak.upgrade() {
                Some(store) => socket::answer(&store, request),
                None => socket::Answer::Refused("the settings were closed".to_owned()),
            };
            match serde_json::to_vec(&answer) {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::error!(%error, "the settings socket could not encode its answer");
                    Vec::new()
                }
            }
        });
        *self.socket.lock_recover() = match opened {
            Ok(socket) => Some(socket),
            Err(error) if error.kind() == std::io::ErrorKind::Unsupported => None,
            Err(error) => {
                tracing::error!(
                    root = %self.root.display(),
                    %error,
                    "the settings socket did not open: other processes' settings writes fail"
                );
                None
            }
        };
    }

    fn is_follower(&self) -> bool {
        self.follower.load(Ordering::Acquire)
    }

    /// A follower opened to hold the profile takes it over when its holder
    /// is gone (a session that ended): from what the holder saved, as the
    /// holder from now on. Whether this process now holds it; never waits
    /// for a holder that is still there.
    fn take_over(&self) -> bool {
        if !self.may_hold || !self.is_follower() {
            return false;
        }
        let Hold::Held(lock) = try_hold(&self.kept) else {
            return false;
        };
        let Ok(loaded) = load_docs(&self.kept) else {
            return false;
        };
        let mut docs = self.docs.lock_recover();
        *docs = loaded;
        let document = Arc::new(docs.projection());
        drop(docs);
        *self.lock.lock_recover() = Some(lock);
        self.follower.store(false, Ordering::Release);
        *self.refused.lock_recover() = None;
        *self.watch.lock_recover() = None;
        self.publish(document);
        self.open_socket();
        true
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn is_holder(&self) -> bool {
        self.refused.lock_recover().is_none()
    }

    pub(crate) fn refuse_writes(&self, why: &str) {
        *self.refused.lock_recover() = Some(why.to_owned());
    }

    #[must_use]
    pub fn document(&self) -> Arc<Map<String, Value>> {
        self.take_in_saves();
        Arc::clone(&self.snapshot.lock_recover().document)
    }

    fn take_in_saves(&self) {
        if !self.is_follower() {
            return;
        }
        let now = self.follow.stamp();
        let mut docs = self.docs.lock_recover();
        if docs.seen == now {
            return;
        }
        let Ok(loaded) = load_docs(&self.kept) else {
            return;
        };
        *docs = loaded;
        docs.seen = now;
        // Shown before the save is let go: a reader that finds the save already taken in — one
        // that looks after `seen` became `now` — reads what this look loaded, never the save
        // before it. Readers are told only after: one that reads the settings back would wait on
        // the lock this thread holds.
        let changed = self.set_shown(Arc::new(docs.projection()));
        drop(docs);
        if changed {
            self.changes.ring();
        }
    }

    fn publish(&self, document: Arc<Map<String, Value>>) {
        if self.set_shown(document) {
            self.changes.ring();
        }
    }

    /// Whether `document` differs from what was shown.
    fn set_shown(&self, document: Arc<Map<String, Value>>) -> bool {
        let mut snapshot = self.snapshot.lock_recover();
        if *snapshot.document == *document {
            return false;
        }
        snapshot.document = document;
        drop(snapshot);
        self.revision.fetch_add(1, Ordering::AcqRel);
        true
    }

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

    fn write_sections(
        &self,
        origin: &ChangeOrigin,
        decide: &mut DecideSections<'_>,
    ) -> std::io::Result<ConditionalUpdate> {
        let refused = self.refused.lock_recover().clone();
        if let Some(why) = refused
            && !self.take_over()
        {
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
        make_containers(&mut docs, &sections).map_err(std::io::Error::other)?;
        let mut touched: Vec<(Holder, String, String, Value)> = Vec::new();
        for (name, values) in sections {
            let current = document.get(&name).and_then(Value::as_object);
            for (leaf, value) in values {
                if current.and_then(|section| section.get(&leaf)) == Some(&value) {
                    continue;
                }
                let holder = placement::holder(&name);
                let doc = docs.doc(holder);
                let section =
                    json::map_at(doc, &ROOT, name.as_str()).map_err(std::io::Error::other)?;
                json::write(doc, &section, leaf.as_str(), &value).map_err(std::io::Error::other)?;
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
        let sections = touched
            .into_iter()
            .map(|(_, section, ..)| section)
            .collect();
        let document = docs.keep(sections);
        drop(docs);
        self.show(document);
        Ok(ConditionalUpdate::Stored)
    }

    /// Hand what changed to the document store, which commits it on its own thread: the changes
    /// since each document's last save, as the document's [`SavePlan`] decides, appended or whole.
    /// Only the holder writes; a profile held in memory keeps what it has.
    fn save(&self) {
        let mut docs = self.docs.lock_recover();
        if docs.unsaved.is_empty() || self.lock.lock_recover().is_none() {
            return;
        }
        let sections = std::mem::take(&mut docs.unsaved);
        let mut saved = Ok(());
        for (index, holder) in [Holder::Person, Holder::Device].into_iter().enumerate() {
            let Some((heads, plan)) = docs.saved.get(index).cloned() else {
                continue;
            };
            let doc = docs.doc(holder);
            let now = doc.get_heads();
            if now == heads {
                continue;
            }
            let op = match plan.next() {
                Save::Full => WriteOp::Replace(doc.save()),
                Save::Append => WriteOp::Append(doc.save_after(&heads)),
            };
            let written = op.bytes().len();
            if let Err(error) = self.kept.write(&key(holder), op.clone(), None) {
                saved = Err(error);
                break;
            }
            let mut after = plan;
            match op {
                WriteOp::Replace(_) => after.stored_whole(written),
                WriteOp::Append(_) => after.appended(written),
            }
            if let Some(last) = docs.saved.get_mut(index) {
                *last = (now, after);
            }
        }
        drop(docs);
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
    /// with `at_ms`, `section`, `leaf`, `after`, `action`, `actor` and
    /// `invoked_by`, read from the documents' history.
    #[must_use]
    pub fn change_log(&self) -> Vec<Value> {
        let mut docs = self.docs.lock_recover();
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
                    "actor": field(message, "actor"),
                    "invoked_by": field(message, "invoked_by"),
                }));
            }
        }
        entries
    }
}

impl Store {
    /// Merge `sections` into the document as one change by `origin`: every
    /// section's leaves, wherever each is held (this device's document or
    /// the person's), committed under one message so they are one step
    /// history takes back together, and one that [`Self::edit_documents`]
    /// undoes across both. A section held by this device and one by the
    /// person are written by one call here, which is what a choice spanning
    /// both (which model the agent answers with, kept half on each) needs.
    ///
    /// # Errors
    /// The document cannot be written, as [`Self::edit_document`].
    pub fn update_sections(
        &self,
        sections: Vec<(String, Map<String, Value>)>,
        origin: &ChangeOrigin,
    ) -> std::io::Result<()> {
        let mutate = std::cell::Cell::new(Some(sections));
        self.write_sections(origin, &mut |_| mutate.take())
            .map(drop)
    }

    pub fn edit_document(
        &self,
        section: &str,
        origin: &ChangeOrigin,
        edit: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
    ) -> std::io::Result<()> {
        let holder = placement::holder(section);
        let refused = self.refused.lock_recover().clone();
        if let Some(why) = refused
            && !self.take_over()
        {
            if !self.is_follower() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    why,
                ));
            }
            self.take_in_saves();
            let mut copy = self.docs.lock_recover().doc(holder).fork();
            let before = copy.get_heads();
            if commit_edit(&mut copy, origin, edit)?.is_none() {
                return Ok(());
            }
            let bytes: Vec<u8> = copy
                .get_changes(&before)
                .iter()
                .flat_map(|change| change.raw_bytes().to_vec())
                .collect();
            let changes = socket::Changes {
                person: holder == Holder::Person,
                section: section.to_owned(),
                bytes,
            };
            return self.forward_changes(changes, &why);
        }
        let mut docs = self.docs.lock_recover();
        let Some(mut touched) = commit_edit(docs.doc(holder), origin, edit)? else {
            return Ok(());
        };
        if touched.is_empty() {
            touched.push(section.to_owned());
        }
        let document = docs.keep(touched);
        drop(docs);
        self.show(document);
        Ok(())
    }

    fn show(&self, document: Arc<Map<String, Value>>) {
        self.publish(document);
        self.save();
    }

    /// Carry `edit` out on the person's document and on this device's,
    /// committing what each made under one message for `origin`: one step
    /// history takes back across both, which [`Self::update_sections`]'s
    /// choice is undone with. Each document's edit is committed only when it
    /// wrote anything; the change carries the leaves both wrote, so a reader
    /// of either document's history finds the whole step.
    ///
    /// # Errors
    /// Either document cannot be read or written, as [`Self::edit_document`].
    pub fn edit_documents(
        &self,
        origin: &ChangeOrigin,
        edit: impl Fn(&mut AutoCommit, Holder) -> Result<(), String>,
    ) -> std::io::Result<()> {
        let refused = self.refused.lock_recover().clone();
        if let Some(why) = refused
            && !self.take_over()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                why,
            ));
        }
        let mut docs = self.docs.lock_recover();
        let mut written: Vec<(Holder, Vec<String>)> = Vec::new();
        let mut touched: Vec<(Holder, String, String, Value)> = Vec::new();
        for holder in [Holder::Person, Holder::Device] {
            let before = read_root(docs.doc(holder));
            if let Err(reason) = edit(docs.doc(holder), holder) {
                // Neither document takes the change: what the other edit
                // wrote is rolled back with it, so no transaction is left
                // open on either.
                docs.doc(Holder::Person).rollback();
                docs.doc(Holder::Device).rollback();
                return Err(std::io::Error::other(reason));
            }
            if docs.doc(holder).pending_ops() == 0 {
                continue;
            }
            let after = read_root(docs.doc(holder));
            let mut sections = Vec::new();
            for name in after
                .keys()
                .chain(before.keys().filter(|name| !after.contains_key(*name)))
            {
                let section_of = |document: &Map<String, Value>| -> Map<String, Value> {
                    document
                        .get(name)
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default()
                };
                let (was, now) = (section_of(&before), section_of(&after));
                if was == now {
                    continue;
                }
                sections.push(name.clone());
                for (leaf, value) in &now {
                    if was.get(leaf) != Some(value) {
                        touched.push((holder, name.clone(), leaf.clone(), value.clone()));
                    }
                }
                for leaf in was.keys().filter(|leaf| !now.contains_key(*leaf)) {
                    touched.push((holder, name.clone(), leaf.clone(), Value::Null));
                }
            }
            written.push((holder, sections));
        }
        if touched.is_empty() {
            return Ok(());
        }
        let message = change_message(origin, &touched, CHANGES.fetch_add(1, Ordering::Relaxed));
        let time = i64::try_from(tilo_clock::unix_now_ms() / 1000).unwrap_or(i64::MAX);
        let mut sections = Vec::new();
        for (holder, names) in written {
            docs.doc(holder).commit_with(
                CommitOptions::default()
                    .with_message(message.clone())
                    .with_time(time),
            );
            sections.extend(names);
        }
        let document = docs.keep(sections);
        drop(docs);
        self.show(document);
        Ok(())
    }

    fn take_changes(&self, changes: &socket::Changes) -> std::io::Result<()> {
        let holder = if changes.person {
            Holder::Person
        } else {
            Holder::Device
        };
        let mut docs = self.docs.lock_recover();
        docs.doc(holder)
            .load_incoming_whole(&changes.bytes)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let document = docs.keep(vec![changes.section.clone()]);
        drop(docs);
        self.show(document);
        Ok(())
    }

    fn forward_changes(&self, changes: socket::Changes, why: &str) -> std::io::Result<()> {
        let request = socket::Request {
            origin: ChangeOrigin::default(),
            expected: Vec::new(),
            sections: Vec::new(),
            changes: Some(changes),
        };
        match socket::send(&self.dir, &request) {
            Ok(_) => {
                self.take_in_saves();
                Ok(())
            }
            Err(_) if self.take_over() => {
                let Some(changes) = request.changes else {
                    return Ok(());
                };
                self.take_changes(&changes)
            }
            Err(error) => Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{why}, which did not take the change: {error}"),
            )),
        }
    }
}

fn make_containers(
    docs: &mut Docs,
    sections: &[(String, Map<String, Value>)],
) -> Result<(), automerge::AutomergeError> {
    let is_map = |doc: &AutoCommit, obj: &ObjId, key: &str| {
        matches!(
            doc.get(obj, key),
            Ok(Some((automerge::Value::Object(ObjType::Map), _)))
        )
    };
    let mut made = false;
    for (name, values) in sections {
        let doc = docs.doc(placement::holder(name));
        made |= !is_map(doc, &ROOT, name);
        let section = json::map_at(doc, &ROOT, name.as_str())?;
        for (leaf, value) in values {
            if value.is_object() && !is_map(doc, &section, leaf) {
                json::map_at(doc, &section, leaf.as_str())?;
                made = true;
            }
        }
    }
    if made {
        let message = change_message(
            &ChangeOrigin::default(),
            &[],
            CHANGES.fetch_add(1, Ordering::Relaxed),
        );
        for holder in [Holder::Person, Holder::Device] {
            docs.doc(holder)
                .commit_with(CommitOptions::default().with_message(message.clone()));
        }
    }
    Ok(())
}

/// Run `edit` on `doc` and commit what it made as one change by `origin`,
/// recording the leaves it set. The sections whose leaves it changed;
/// `None`, and nothing committed, when it changed nothing at all.
fn commit_edit(
    doc: &mut AutoCommit,
    origin: &ChangeOrigin,
    edit: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
) -> std::io::Result<Option<Vec<String>>> {
    let before = read_root(doc);
    if let Err(error) = edit(doc) {
        doc.rollback();
        return Err(std::io::Error::other(error));
    }
    if doc.pending_ops() == 0 {
        return Ok(None);
    }
    let after = read_root(doc);
    let section_of = |document: &Map<String, Value>, name: &str| -> Map<String, Value> {
        document
            .get(name)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    };
    let mut touched: Vec<(Holder, String, String, Value)> = Vec::new();
    let mut sections: Vec<String> = Vec::new();
    for name in after
        .keys()
        .chain(before.keys().filter(|name| !after.contains_key(*name)))
    {
        let (was, now) = (section_of(&before, name), section_of(&after, name));
        if was == now {
            continue;
        }
        sections.push(name.clone());
        for (leaf, value) in &now {
            if was.get(leaf) != Some(value) {
                touched.push((
                    placement::holder(name),
                    name.clone(),
                    leaf.clone(),
                    value.clone(),
                ));
            }
        }
        for leaf in was.keys().filter(|leaf| !now.contains_key(*leaf)) {
            touched.push((
                placement::holder(name),
                name.clone(),
                leaf.clone(),
                Value::Null,
            ));
        }
    }
    let message = change_message(origin, &touched, CHANGES.fetch_add(1, Ordering::Relaxed));
    let time = i64::try_from(tilo_clock::unix_now_ms() / 1000).unwrap_or(i64::MAX);
    doc.commit_with(
        CommitOptions::default()
            .with_message(message)
            .with_time(time),
    );
    Ok(Some(sections))
}

#[must_use]
pub fn made_by(message: &str, action_prefix: &str, actor: &str) -> bool {
    serde_json::from_str::<Value>(message).is_ok_and(|message| {
        message
            .get("action")
            .and_then(Value::as_str)
            .is_some_and(|action| action.starts_with(action_prefix))
            && message.get("actor").and_then(Value::as_str) == Some(actor)
    })
}

impl Store {
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
        let request = socket::Request {
            origin: origin.clone(),
            expected,
            sections,
            changes: None,
        };
        let answer = match socket::send(&self.dir, &request) {
            Ok(answer) => answer,
            // The holder lets the settings go before it closes its socket:
            // where the socket no longer answers and the lock is free, this
            // process holds them and makes the change itself. A holder that
            // is still there is never waited for.
            Err(_) if self.take_over() => {
                let socket::Request {
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

mod socket {
    use serde_json::{Map, Value};

    use super::{PROFILE_SOCKET, Socket, Store};
    use crate::{ChangeOrigin, ConditionalUpdate};
    use std::path::Path;

    /// One write: merge `sections`, provided each section still reads as
    /// `expected` (what the writer decided over); or take `changes`, made
    /// to a copy of the document itself.
    #[derive(serde::Serialize, serde::Deserialize)]
    pub(super) struct Request {
        pub(super) origin: ChangeOrigin,
        pub(super) expected: Vec<(String, Option<Value>)>,
        pub(super) sections: Vec<(String, Map<String, Value>)>,
        #[serde(default)]
        pub(super) changes: Option<Changes>,
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    pub(super) struct Changes {
        pub(super) person: bool,
        pub(super) section: String,
        pub(super) bytes: Vec<u8>,
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    pub(super) enum Answer {
        Stored,
        Superseded,
        Refused(String),
    }

    pub(super) fn answer(store: &Store, request: &[u8]) -> Answer {
        let Request {
            origin,
            expected,
            sections,
            changes,
        } = match serde_json::from_slice(request) {
            Ok(request) => request,
            Err(error) => return Answer::Refused(error.to_string()),
        };
        if let Some(changes) = changes {
            let taken = store.take_changes(&changes);
            crate::SettingsSource::flush_pending(store);
            return match taken {
                Ok(()) => Answer::Stored,
                Err(error) => Answer::Refused(error.to_string()),
            };
        }
        let mut sections = Some(sections);
        let stored = store.write_sections(&origin, &mut |document| {
            expected
                .iter()
                .all(|(name, value)| document.get(name) == value.as_ref())
                .then(|| sections.take().unwrap_or_default())
        });
        crate::SettingsSource::flush_pending(store);
        match stored {
            Ok(ConditionalUpdate::Stored) => Answer::Stored,
            Ok(ConditionalUpdate::Superseded) => Answer::Superseded,
            Err(error) => Answer::Refused(error.to_string()),
        }
    }

    pub(super) fn send(dir: &Path, request: &Request) -> std::io::Result<ConditionalUpdate> {
        let body = serde_json::to_vec(request).map_err(std::io::Error::other)?;
        let line = Socket::send(&dir.join(PROFILE_SOCKET), &body)?;
        match serde_json::from_slice::<Answer>(&line).map_err(std::io::Error::other)? {
            Answer::Stored => Ok(ConditionalUpdate::Stored),
            Answer::Superseded => Ok(ConditionalUpdate::Superseded),
            Answer::Refused(why) => Err(std::io::Error::other(why)),
        }
    }
}

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
        "actor": origin.actor,
        "invoked_by": origin.invoked_by,
    })
    .to_string()
}

impl SettingsSource for Store {
    fn load(&self) -> Map<String, Value> {
        (*self.document()).clone()
    }

    fn store(&self) -> Option<&Store> {
        Some(self)
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
        if sections
            .iter()
            .any(|(section, _)| placement::holder(section) == Holder::Person)
        {
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
        self.save();
        if self.lock.lock_recover().is_some()
            && let Err(error) = self.kept.flush(None)
        {
            tracing::warn!(root = %self.root.display(), %error, "settings not made durable");
        }
    }

    fn take_deferred_failures(&self) -> Vec<crate::DeferredFailure> {
        std::mem::take(&mut *self.failures.lock_recover())
    }

    fn changes(&self) -> &crate::ChangeSignal {
        &self.changes
    }
}

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

fn empty(root: &Path) -> Arc<Store> {
    let dir = root.join(PROFILE_DIR);
    let kept = FsStore::new(&dir);
    let follow = Follow::of(&kept, &keys());
    Arc::new_cyclic(|me| Store {
        me: me.clone(),
        root: root.to_path_buf(),
        dir,
        kept,
        follow,
        follower: std::sync::atomic::AtomicBool::new(false),
        may_hold: false,
        lock: Mutex::new(None),
        refused: Mutex::new(None),
        docs: Mutex::new(Docs::loaded(AutoCommit::new(), AutoCommit::new())),
        snapshot: Mutex::new(Snapshot {
            document: Arc::new(Map::new()),
        }),
        revision: AtomicU64::new(1),
        changes: crate::ChangeSignal::default(),
        failures: Mutex::new(Vec::new()),
        watch: Mutex::new(None),
        socket: Mutex::new(None),
    })
}

fn read_root(doc: &AutoCommit) -> Map<String, Value> {
    doc.keys(&ROOT)
        .filter(|key| !key.starts_with(BOOKKEEPING))
        .filter_map(|key| {
            let (value, id) = doc.get(&ROOT, key.as_str()).ok()??;
            Some((key, json::read_value(doc, &value, &id).ok()?))
        })
        .collect()
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

    fn promptly<T: Send + 'static>(write: impl FnOnce() -> T + Send + 'static) -> T {
        let (done, result) = std::sync::mpsc::channel();
        std::thread::spawn(move || done.send(write()));
        result
            .recv_timeout(std::time::Duration::from_secs(20))
            .expect("the write returned")
    }

    fn deep_profile() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("a".repeat(60)).join("b".repeat(60));
        let socket = root.join(PROFILE_DIR).join(PROFILE_SOCKET);
        assert!(socket.as_os_str().len() > 108, "{}", socket.display());
        (dir, root)
    }

    #[cfg(unix)]
    #[test]
    fn a_follower_never_waits_for_a_live_holder_whose_socket_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let holder = Store::open(dir.path()).unwrap();
        holder.update_section("s", object(json!({"x": 1}))).unwrap();
        holder.flush_pending();
        let follower = Store::open(dir.path()).unwrap();
        *holder.socket.lock_recover() = None;
        let socket = holder.dir.join(PROFILE_SOCKET);
        drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());

        let writer = Arc::clone(&follower);
        let refused = promptly(move || writer.update_section("s", object(json!({"x": 2}))));
        assert!(refused.is_err(), "{refused:?}");
        let writer = Arc::clone(&follower);
        let refused = promptly(move || {
            writer.edit_document("power", &ChangeOrigin::default(), |doc| {
                put_leaf(doc, "power", "x", 2);
                Ok(())
            })
        });
        assert!(refused.is_err(), "{refused:?}");
        assert!(!follower.is_holder());
        assert_eq!(holder.load_section_value("s"), json!({"x": 1}));
    }

    tilo_test_env::scenario! {
        fn a_store_keeps_each_section_in_its_document;

        phase writes_show_at_once_and_survive_a_reopen;
        {
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

        phase the_persons_and_the_devices_sections_go_to_their_documents;
        {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(dir.path()).unwrap();
            store
                .update_section("appearance", object(json!({"text_scale": 1.5})))
                .unwrap();
            store
                .update_section("so.tilo.unknown", object(json!({"x": true})))
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
                json!({"so.tilo.unknown": {"x": true}})
            );
        }

        phase a_nested_value_loses_the_keys_it_no_longer_has;
        {
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

        phase a_compare_update_stores_only_over_what_it_read;
        {
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
    }

    fn put_leaf(doc: &mut AutoCommit, section: &str, leaf: &str, value: i64) {
        let map = json::map_at(doc, &ROOT, section).unwrap();
        doc.put(&map, leaf, value).unwrap();
        doc.put(&ROOT, format!("{BOOKKEEPING}mark"), true).unwrap();
    }

    tilo_test_env::scenario! {
        fn a_followers_writes_are_made_by_the_holder;

        phase a_second_opener_follows_and_its_writes_go_through_the_holder;
        {
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

        phase a_follower_reader_told_of_a_save_may_read_the_settings_back;
        {
            let dir = tempfile::tempdir().unwrap();
            let holder = Store::open(dir.path()).unwrap();
            holder.update_section("s", object(json!({"x": 1}))).unwrap();
            holder.flush_pending();
            let follower = Store::open(dir.path()).unwrap();
            let reader = Arc::downgrade(&follower);
            let read: Arc<crate::source::Listener> = Arc::new(move || {
                if let Some(follower) = reader.upgrade() {
                    let _ = follower.document();
                }
            });
            follower.changes.listen(&read);
            holder.update_section("s", object(json!({"x": 2}))).unwrap();
            holder.flush_pending();
            let looker = Arc::clone(&follower);
            let seen = promptly(move || looker.load_section_value("s"));
            assert_eq!(seen, json!({"x": 2}));
        }

        phase a_follower_opened_to_hold_takes_over_when_the_holder_is_gone;
        {
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

        phase a_profile_deeper_than_a_socket_path_still_takes_followers_writes;
        {
            let (_dir, root) = deep_profile();
            let holder = Store::open(&root).unwrap();
            holder.update_section("s", object(json!({"x": 1}))).unwrap();
            holder.flush_pending();
            let follower = Store::open(&root).unwrap();
            assert!(!follower.is_holder());
            let writer = Arc::clone(&follower);
            promptly(move || writer.update_section("s", object(json!({"x": 2})))).unwrap();
            assert_eq!(holder.load_section_value("s"), json!({"x": 2}));
            let writer = Arc::clone(&follower);
            promptly(move || {
                writer.edit_document(
                    "power",
                    &ChangeOrigin::local("settings.power/undo"),
                    |doc| {
                        put_leaf(doc, "power", "x", 2);
                        Ok(())
                    },
                )
            })
            .unwrap();
            assert_eq!(holder.load_section_value("power"), json!({"x": 2}));
            assert!(!follower.is_holder(), "the holder is still there");
        }

        phase a_followers_document_edit_is_made_by_the_holder;
        {
            let dir = tempfile::tempdir().unwrap();
            let holder = Store::open(dir.path()).unwrap();
            holder
                .update_section("power", object(json!({"x": 1})))
                .unwrap();
            holder.flush_pending();
            let follower = Store::open(dir.path()).unwrap();
            assert!(!follower.is_holder());
            follower
                .edit_document(
                    "power",
                    &ChangeOrigin::local("settings.power/undo"),
                    |doc| {
                        put_leaf(doc, "power", "x", 2);
                        Ok(())
                    },
                )
                .unwrap();
            assert_eq!(holder.load_section_value("power"), json!({"x": 2}));
            assert_eq!(follower.load_section_value("power"), json!({"x": 2}));
            assert_eq!(
                holder
                    .change_log()
                    .last()
                    .map(|change| change["action"].clone()),
                Some(json!("settings.power/undo"))
            );
        }
    }

    tilo_test_env::scenario! {
        fn every_change_is_logged_with_what_made_it;

        phase the_change_log_names_what_made_each_change;
        {
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
            assert_eq!(log[0]["actor"], ChangeOrigin::LOCAL_USER);
        }

        phase a_document_edit_is_one_change_by_its_origin_and_its_bookkeeping_is_no_section;
        {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(dir.path()).unwrap();
            store
                .edit_document(
                    "appearance",
                    &ChangeOrigin::local("settings.appearance/undo"),
                    |doc| {
                        put_leaf(doc, "appearance", "text_scale", 2);
                        Ok(())
                    },
                )
                .unwrap();
            assert_eq!(
                Value::Object(store.load()),
                json!({"appearance": {"text_scale": 2}})
            );
            let log = store.change_log();
            assert_eq!(log.len(), 1);
            assert_eq!(log[0]["action"], "settings.appearance/undo");
            assert_eq!(log[0]["leaf"], "text_scale");
            let docs = store.docs.lock_recover();
            assert!(
                docs.person.get(&ROOT, "tilo:mark").unwrap().is_some(),
                "in the person's document"
            );
            drop(docs);

            let refused = store.edit_document("appearance", &ChangeOrigin::default(), |doc| {
                put_leaf(doc, "appearance", "text_scale", 3);
                Err("no".into())
            });
            assert!(refused.is_err());
            assert_eq!(store.load_section_value("appearance")["text_scale"], 2);
        }

        phase a_change_names_the_section_action_and_who_made_it;
        {
            let message = change_message(&ChangeOrigin::local("settings.power/field-edit"), &[], 0);
            assert!(made_by(
                &message,
                "settings.power/",
                ChangeOrigin::LOCAL_USER
            ));
            assert!(!made_by(
                &message,
                "settings.appearance/",
                ChangeOrigin::LOCAL_USER
            ));
            assert!(!made_by(&message, "settings.power/", "someone-else"));
            assert!(!made_by(
                "tilo.entry/3 x",
                "settings.power/",
                ChangeOrigin::LOCAL_USER
            ));
        }
    }

    impl Store {
        fn load_section_value(&self, name: &str) -> Value {
            self.load().remove(name).unwrap_or(Value::Null)
        }
    }
}
