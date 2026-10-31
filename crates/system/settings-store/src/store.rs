//! One profile's settings, kept as two Automerge documents.
//!
//! A [`Store`] holds the person's document (the sections
//! [`crate::placement`] gives the person) and this device's document (every
//! other section), in `<root>/records/person.automerge` and
//! `<root>/records/device.automerge`. Each section is a map at the document's
//! root and each leaf a key in it; a value is stored as what it is (an object
//! a map, an array a list, a string, number, boolean or `null` a scalar), as
//! every Tilo document stores JSON ([`tilo_automerge::json`]), so concurrent
//! edits of different keys merge and a typed model can read the same
//! document. Readers see the device document with the person's leaves laid
//! over it.
//!
//! A write changes the documents in memory, commits it with its
//! [`ChangeOrigin`] as the change's message, and shows it to readers at once;
//! one that needs the document itself (a section's undo, which reads its
//! history) changes it through [`Store::edit_document`], committed, shown and
//! saved the same way. Root keys under [`BOOKKEEPING`] are that history's
//! bookkeeping (what was undone), never a section;
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
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc as _};
use serde_json::{Map, Value};
use tilo_automerge::{ApplyIncoming as _, json};
use tilo_lock::MutexRecover as _;

use tilo_settings_records::{Door, Hold, Holding, Records, Stamp, Watch};

use crate::placement::{self, Holder};
use crate::{ChangeOrigin, ConditionalUpdate, SectionChange, SettingsSource, merge_section};

pub use tilo_settings_records::RECORDS_DIR;

/// The prefix of the root keys that keep a document's history (what was
/// undone) rather than a section.
pub const BOOKKEEPING: &str = "tilo:";

const PERSON_FILE: &str = "person.automerge";
const DEVICE_FILE: &str = "device.automerge";
const DOCUMENTS: &[&str] = &[PERSON_FILE, DEVICE_FILE];

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
    /// The sections changed since the documents were last saved.
    unsaved: std::collections::BTreeSet<String>,
    /// What a follower last loaded, to notice the holder's next save.
    seen: (Stamp, Stamp),
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

    /// Note `sections` changed and unsaved: the documents as readers now
    /// see them.
    fn keep(&mut self, sections: Vec<String>) -> Arc<Map<String, Value>> {
        self.unsaved.extend(sections);
        Arc::new(self.projection())
    }
}

struct Snapshot {
    document: Arc<Map<String, Value>>,
}

/// One profile's settings: see the module docs.
pub struct Store {
    me: Weak<Store>,
    root: PathBuf,
    records: Records,
    /// Another process holds the profile: this one follows its saves.
    follower: std::sync::atomic::AtomicBool,
    /// Opened to hold the profile: takes it over when its holder is gone.
    may_hold: bool,
    /// The holder's lock; `None` for a follower.
    lock: Mutex<Option<Holding>>,
    /// Why this process may not write (a follower, an unisolated test).
    refused: Mutex<Option<String>>,
    docs: Mutex<Docs>,
    snapshot: Mutex<Snapshot>,
    revision: AtomicU64,
    changes: crate::ChangeSignal,
    failures: Mutex<Vec<crate::DeferredFailure>>,
    /// A follower's watch on the holder's saves.
    watch: Mutex<Option<Watch>>,
    /// The holder's door for other processes' writes.
    door: Mutex<Option<Door>>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("root", &self.root)
            .field("holder", &self.lock.lock_recover().is_some())
            .finish_non_exhaustive()
    }
}

fn load_doc(records: &Records, name: &str) -> std::io::Result<AutoCommit> {
    match records.read(name)? {
        Some(bytes) => AutoCommit::load(&bytes).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{}: {error}", records.dir().join(name).display()),
            )
        }),
        None => Ok(AutoCommit::new()),
    }
}

/// Both documents as saved now, with what tells this save from the next.
fn load_docs(records: &Records) -> std::io::Result<(AutoCommit, AutoCommit, (Stamp, Stamp))> {
    let seen = (records.stamp(PERSON_FILE), records.stamp(DEVICE_FILE));
    Ok((
        load_doc(records, PERSON_FILE)?,
        load_doc(records, DEVICE_FILE)?,
        seen,
    ))
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
        let records = Records::of(root);
        let taken = if hold { records.try_hold() } else { Hold::Busy };
        let (person, device, seen) = load_docs(&records)?;
        let docs = Docs {
            person,
            device,
            unsaved: std::collections::BTreeSet::new(),
            seen,
        };
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
            records,
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
            door: Mutex::new(None),
        });
        if holds {
            store.open_door();
        }
        if follower {
            let weak = Arc::downgrade(&store);
            *store.watch.lock_recover() = store.records.watch(DOCUMENTS, move || {
                if let Some(store) = weak.upgrade() {
                    store.take_in_saves();
                }
            });
        }
        Ok(store)
    }

    /// Take other processes' writes from now on.
    fn open_door(&self) {
        let weak = self.me.clone();
        *self.door.lock_recover() = self.records.open_door(move |request| {
            let answer = match weak.upgrade() {
                Some(store) => door::answer(&store, request),
                None => door::Answer::Refused("the settings were closed".to_owned()),
            };
            serde_json::to_vec(&answer).unwrap_or_default()
        });
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
        let held = if wait {
            self.records.wait_to_hold()
        } else {
            self.records.try_hold()
        };
        let Hold::Held(lock) = held else {
            return false;
        };
        let Ok((person, device, seen)) = load_docs(&self.records) else {
            return false;
        };
        let mut docs = self.docs.lock_recover();
        docs.person = person;
        docs.device = device;
        docs.seen = seen;
        let document = Arc::new(docs.projection());
        drop(docs);
        *self.lock.lock_recover() = Some(lock);
        self.follower.store(false, Ordering::Release);
        *self.refused.lock_recover() = None;
        *self.watch.lock_recover() = None;
        self.publish(document);
        self.open_door();
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
        let now = (
            self.records.stamp(PERSON_FILE),
            self.records.stamp(DEVICE_FILE),
        );
        let mut docs = self.docs.lock_recover();
        if docs.seen == now {
            return;
        }
        let (Ok(person), Ok(device)) = (
            load_doc(&self.records, PERSON_FILE),
            load_doc(&self.records, DEVICE_FILE),
        ) else {
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
        let saved = self
            .records
            .write(PERSON_FILE, &person)
            .and_then(|()| self.records.write(DEVICE_FILE, &device));
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

impl Store {
    /// Change the document holding section `section` (the person's or this
    /// device's, as [`placement`] places it) through `edit`, as one change
    /// made by `origin`: what a change that needs the document itself makes
    /// (a section's undo, which reads its history and marks what it took
    /// back). The change records the leaves it set, as every write does,
    /// and is shown and saved like one. `edit` refusing changes nothing.
    ///
    /// # Errors
    /// `edit` refused, or this process may not write the profile.
    pub fn edit_document(
        &self,
        section: &str,
        origin: &ChangeOrigin,
        edit: impl FnOnce(&mut AutoCommit) -> Result<(), String>,
    ) -> std::io::Result<()> {
        let holder = placement::holder(section);
        let refused = self.refused.lock_recover().clone();
        if let Some(why) = refused
            && !self.take_over(false)
        {
            if !self.is_follower() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    why,
                ));
            }
            // Made on a copy of what the holder saved, and sent to it whole.
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
            let changes = door::Changes {
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
        // The history moved even where no leaf did (an undo's mark).
        if touched.is_empty() {
            touched.push(section.to_owned());
        }
        let document = docs.keep(touched);
        drop(docs);
        self.show(document);
        Ok(())
    }

    /// Show `document`, the documents as changed, and save them.
    fn show(&self, document: Arc<Map<String, Value>>) {
        self.publish(document);
        writer().schedule(&self.me);
    }

    /// Merge `changes`, which another process made to a copy of one of the
    /// documents, as the holder.
    fn take_changes(&self, changes: &door::Changes) -> std::io::Result<()> {
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

    /// A follower's change to a copy of one of the documents: made by the
    /// holder, and read back from its save.
    fn forward_changes(&self, changes: door::Changes, why: &str) -> std::io::Result<()> {
        let request = door::Request {
            origin: ChangeOrigin::default(),
            expected: Vec::new(),
            sections: Vec::new(),
            changes: Some(changes),
        };
        match door::send(&self.records, &request) {
            Ok(_) => {
                self.take_in_saves();
                Ok(())
            }
            Err(error) if error.kind() != std::io::ErrorKind::NotFound && self.take_over(true) => {
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

/// Make the maps `sections` are written into that do not exist yet (each
/// section's, and each leaf's that is an object), as a change of its own
/// that nobody made: a change of a leaf, and its undo, then never removes
/// the map holding it, which a redo of a later change writes into again.
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

/// Whether the change whose message is `message` was made by an action
/// whose id starts with `action_prefix` (`settings.appearance/`), invoked
/// by `principal`: what tells a section's steps by one invoker apart in a
/// document's history.
#[must_use]
pub fn made_by(message: &str, action_prefix: &str, principal: &str) -> bool {
    serde_json::from_str::<Value>(message).is_ok_and(|message| {
        message
            .get("action")
            .and_then(Value::as_str)
            .is_some_and(|action| action.starts_with(action_prefix))
            && message.get("principal").and_then(Value::as_str) == Some(principal)
    })
}

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
        let request = door::Request {
            origin: origin.clone(),
            expected,
            sections,
            changes: None,
        };
        let answer = match door::send(&self.records, &request) {
            Ok(answer) => answer,
            // The holder's door closes only as the holder ends: once it has
            // let the settings go, this process holds them and makes the
            // change itself.
            // A holder with no door at all (`NotFound`) is not waited for.
            Err(error) if error.kind() != std::io::ErrorKind::NotFound && self.take_over(true) => {
                let door::Request {
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

/// What crosses the holder's door: another process's write and the
/// holder's answer.
mod door {
    use serde_json::{Map, Value};
    use tilo_settings_records::Records;

    use super::Store;
    use crate::{ChangeOrigin, ConditionalUpdate};

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

    /// Changes another process made to a copy of one of the documents.
    #[derive(serde::Serialize, serde::Deserialize)]
    pub(super) struct Changes {
        /// The person's document; else the device's.
        pub(super) person: bool,
        /// The section they were made for.
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
        // Saved before the answer, so the writer reads its change back.
        crate::SettingsSource::flush_pending(store);
        match stored {
            Ok(ConditionalUpdate::Stored) => Answer::Stored,
            Ok(ConditionalUpdate::Superseded) => Answer::Superseded,
            Err(error) => Answer::Refused(error.to_string()),
        }
    }

    pub(super) fn send(records: &Records, request: &Request) -> std::io::Result<ConditionalUpdate> {
        let body = serde_json::to_vec(request).map_err(std::io::Error::other)?;
        let line = records.knock(&body)?;
        match serde_json::from_slice::<Answer>(&line).map_err(std::io::Error::other)? {
            Answer::Stored => Ok(ConditionalUpdate::Stored),
            Answer::Superseded => Ok(ConditionalUpdate::Superseded),
            Answer::Refused(why) => Err(std::io::Error::other(why)),
        }
    }
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
            let started = tilo_rt::spawn_thread("tilo-settings-writer", move || self.run());
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
        records: Records::of(root),
        follower: std::sync::atomic::AtomicBool::new(false),
        may_hold: false,
        lock: Mutex::new(None),
        refused: Mutex::new(None),
        docs: Mutex::new(Docs {
            person: AutoCommit::new(),
            device: AutoCommit::new(),
            unsaved: std::collections::BTreeSet::new(),
            seen: (Stamp::default(), Stamp::default()),
        }),
        snapshot: Mutex::new(Snapshot {
            document: Arc::new(Map::new()),
        }),
        revision: AtomicU64::new(1),
        changes: crate::ChangeSignal::default(),
        failures: Mutex::new(Vec::new()),
        watch: Mutex::new(None),
        door: Mutex::new(None),
    })
}

// ----- the sections as JSON -----

/// The sections of `doc`: its root, without the history's bookkeeping.
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
    fn the_persons_and_the_devices_sections_go_to_their_documents() {
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

    /// Put `value` at `leaf` of the section `section` of `doc`, and mark
    /// the history's bookkeeping beside it.
    fn put_leaf(doc: &mut AutoCommit, section: &str, leaf: &str, value: i64) {
        let map = json::map_at(doc, &ROOT, section).unwrap();
        doc.put(&map, leaf, value).unwrap();
        doc.put(&ROOT, format!("{BOOKKEEPING}mark"), true).unwrap();
    }

    #[test]
    fn a_document_edit_is_one_change_by_its_origin_and_its_bookkeeping_is_no_section() {
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

    #[test]
    fn a_followers_document_edit_is_made_by_the_holder() {
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

    #[test]
    fn a_change_names_the_section_action_and_who_made_it() {
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

    impl Store {
        fn load_section_value(&self, name: &str) -> Value {
            self.load().remove(name).unwrap_or(Value::Null)
        }
    }
}
