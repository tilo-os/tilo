//! Where a [`crate::Store`] sends its commands: the device's one store, over the app's
//! connection ([`Port`]), or a store held in this process for a device of its own
//! ([`LocalPort`]): a window that runs with no workspace, and the tests.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use collab_types::store::wire::{Bytes, Command, Reply, Turn};

use crate::wire;
use crate::{Schema, Store};

/// The device's store, as an app asks it. Each command is answered before the next is sent.
pub trait Port {
    /// Carries out `command`: a write, or a read as the person the app acts for may read.
    ///
    /// # Errors
    /// Why the store refused it, or why it could not be asked.
    fn command(&self, command: Command) -> Result<Reply, String>;

    /// Takes in what `schema` declares of the kinds the store holds: who reads each that says.
    /// A store the workspace keeps has every published surface's already.
    fn declare(&self, _schema: Schema) {}

    /// Who places this store's reader fields, as the workspace's one read decision answers. A
    /// port that keeps its store elsewhere (a device's) places them there already.
    fn set_record_reader(&self, _reader: std::sync::Arc<dyn crate::RecordReader>) {}

    /// The resources the store document `document` holds now, by kind and id: what reads one
    /// document of the store at a time, without reading the store (search). `None` for a port
    /// that keeps no document of the workspace's store (a window's).
    fn held(&self, _document: &str) -> Option<Vec<(String, String)>> {
        None
    }
}

/// A store held in this process: everything in it is the device's person's to read. Kept on
/// disk under `dir` when given, each document's changes appended to a file of its own, and read
/// back whole when the store opens again.
///
/// It keeps an app's records beside its resources, as a window with no workspace does: each by its
/// app and its id, read and written with the record commands a workspace answers, so the app's
/// records open from it as they do from a workspace. Those records are this process's alone: a
/// record is the workspace's, so one kept here lives no longer than the window that reads it.
pub struct LocalPort {
    app: String,
    store: RefCell<Store>,
    documents: RefCell<BTreeMap<(String, String), automerge::Automerge>>,
    /// The runs of each record's write still to come.
    runs: RefCell<BTreeMap<String, Vec<u8>>>,
    dir: Option<PathBuf>,
}

impl Default for LocalPort {
    fn default() -> Self {
        Self::in_memory()
    }
}

impl LocalPort {
    /// The workspace a local store keeps its resources in: none of a real workspace's, since what
    /// it keeps is this process's alone.
    pub const WORKSPACE: &str = "local";

    /// A store kept in memory only.
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            app: String::new(),
            store: RefCell::new(Store::new(Self::WORKSPACE)),
            documents: RefCell::default(),
            runs: RefCell::default(),
            dir: None,
        }
    }

    /// The port answering for the app `app`: the records its record commands read and write are
    /// this one's.
    #[must_use]
    pub fn named(mut self, app: impl Into<String>) -> Self {
        self.app = app.into();
        self
    }

    /// A store kept under `dir`, with what an earlier one kept there.
    ///
    /// # Errors
    /// When `dir` cannot be made or read, or holds a document that does not read.
    pub fn at(dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let mut store = Store::new(Self::WORKSPACE);
        let entries = std::fs::read_dir(&dir).map_err(|error| error.to_string())?;
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(doc) = name.strip_suffix(".changes").and_then(unescape) else {
                continue;
            };
            if !path.is_file() {
                continue;
            }
            let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
            store
                .load(&doc, &bytes)
                .map_err(|error| format!("{doc} does not read: {error}"))?;
        }
        Ok(Self {
            app: String::new(),
            store: RefCell::new(store),
            documents: RefCell::default(),
            runs: RefCell::default(),
            dir: Some(dir),
        })
    }

    /// The documents the store holds.
    #[must_use]
    pub fn documents(&self) -> Vec<String> {
        self.store
            .borrow()
            .index()
            .documents()
            .map(str::to_owned)
            .collect()
    }

    /// Who reads the document `doc`: its reader set's text; `None` for a document the store
    /// placed no resource in.
    #[must_use]
    pub fn readers(&self, doc: &str) -> Option<String> {
        self.store.borrow().index().readers(doc).map(str::to_owned)
    }

    /// The heads of the document `doc`: they move with every write to it, here or merged in.
    #[must_use]
    pub fn heads(&self, doc: &str) -> Vec<automerge::ChangeHash> {
        self.store
            .borrow()
            .document(doc)
            .map(automerge::Automerge::get_heads)
            .unwrap_or_default()
    }

    /// The document `doc` whole, as another replica of it takes it in.
    #[must_use]
    pub fn save(&self, doc: &str) -> Option<Vec<u8>> {
        self.store
            .borrow()
            .document(doc)
            .map(automerge::Automerge::save)
    }

    /// The changes of the document `doc` past `heads`: what a replica holding `heads` lacks.
    #[must_use]
    pub fn save_after(&self, doc: &str, heads: &[automerge::ChangeHash]) -> Option<Vec<u8>> {
        self.store
            .borrow()
            .document(doc)
            .map(|held| held.save_after(heads))
    }

    /// Merge `theirs`, another replica's copy of the document `doc` (whole, as [`Self::save`]
    /// writes it), into this store's, and keep the result. Whether the document changed.
    ///
    /// # Errors
    /// When `theirs` is no document, or the merged one cannot be kept.
    pub fn merge(&self, doc: &str, theirs: &[u8]) -> Result<bool, String> {
        let mut theirs = automerge::Automerge::load(theirs).map_err(|error| error.to_string())?;
        let mut merged = self
            .store
            .borrow()
            .document(doc)
            .cloned()
            .unwrap_or_default();
        let before = merged.get_heads();
        merged
            .merge(&mut theirs)
            .map_err(|error| error.to_string())?;
        if merged.get_heads() == before {
            return Ok(false);
        }
        let bytes = merged.save();
        self.store
            .borrow_mut()
            .load(doc, &bytes)
            .map_err(|error| error.to_string())?;
        if let Some(dir) = &self.dir {
            std::fs::write(dir.join(format!("{}.changes", escape(doc))), &bytes)
                .map_err(|error| error.to_string())?;
        }
        Ok(true)
    }

    /// Hold `theirs` as the document `doc` (whole, as [`Self::save`] writes it), in place of
    /// what this store held of it: for a store that only shows another replica's copy, whose own
    /// writes (the defaults it drew before that copy came) are not to be merged into it. Whether
    /// the document changed.
    ///
    /// # Errors
    /// When `theirs` is no document, or it cannot be kept.
    pub fn replace(&self, doc: &str, theirs: &[u8]) -> Result<bool, String> {
        let held = automerge::Automerge::load(theirs).map_err(|error| error.to_string())?;
        if self.heads(doc) == held.get_heads() {
            return Ok(false);
        }
        self.store
            .borrow_mut()
            .load(doc, theirs)
            .map_err(|error| error.to_string())?;
        if let Some(dir) = &self.dir {
            std::fs::write(dir.join(format!("{}.changes", escape(doc))), theirs)
                .map_err(|error| error.to_string())?;
        }
        Ok(true)
    }

    fn keep(&self, written: &[crate::Written]) -> Result<(), String> {
        use std::io::Write as _;
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        for write in written {
            let path = dir.join(format!("{}.changes", escape(&write.doc)));
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|error| error.to_string())?;
            let mut change = write.change.clone();
            file.write_all(change.bytes().as_ref())
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    /// The page from byte `from` of the changes of this app's record `id` past the heads `after`
    /// (hex), and the heads they bring it to: none of a record this port holds nothing of.
    fn record_changes(&self, id: &str, after: &[String], from: u64) -> Result<Reply, String> {
        let documents = self.documents.borrow();
        let Some(doc) = documents.get(&(self.app.clone(), id.to_owned())) else {
            return Ok(Reply::RecordChanges {
                changes: Bytes::default(),
                heads: Vec::new(),
                next: None,
                pending: false,
            });
        };
        let known: Vec<automerge::ChangeHash> = after
            .iter()
            .filter_map(|head| head.parse().ok())
            .filter(|head| doc.get_change_meta_by_hash(head).is_some())
            .collect();
        let changes: Vec<u8> = doc
            .get_changes(&known)
            .iter()
            .flat_map(|change| change.raw_bytes().iter().copied())
            .collect();
        let heads = doc.get_heads().iter().map(ToString::to_string).collect();
        Reply::record_page(&changes, heads, from)
            .ok_or_else(|| "the page starts past the record's changes".to_owned())
    }

    /// Takes `changes`, a run of this app's own changes of its record `id`: kept until the last
    /// run, then the record takes them all, founded by its first.
    fn record_change(&self, id: &str, changes: &[u8], more: bool) -> Result<(), String> {
        let mut runs = self.runs.borrow_mut();
        runs.entry(id.to_owned())
            .or_default()
            .extend_from_slice(changes);
        if more {
            return Ok(());
        }
        let changes = runs.remove(id).unwrap_or_default();
        self.documents
            .borrow_mut()
            .entry((self.app.clone(), id.to_owned()))
            .or_default()
            .load_incremental(&changes)
            .map(drop)
            .map_err(|error| error.to_string())
    }
}

impl Port for LocalPort {
    fn declare(&self, schema: Schema) {
        // A local store keeps the types of the surfaces its own process names. A refusal is a
        // new version that drops what an older one holds: the store keeps the type it has, and
        // the workspace's store, which declares the surfaces, says why.
        let _ = self.store.borrow_mut().declare(schema);
    }

    fn set_record_reader(&self, reader: std::sync::Arc<dyn crate::RecordReader>) {
        self.store.borrow_mut().set_record_reader(reader);
    }

    fn held(&self, document: &str) -> Option<Vec<(String, String)>> {
        let store = self.store.borrow();
        Some(
            store
                .index()
                .held(document)
                .map(|(kind, id)| (kind.to_owned(), id.to_owned()))
                .collect(),
        )
    }

    fn command(&self, command: Command) -> Result<Reply, String> {
        match command {
            // A workspace on this device alone: every turn is this device's.
            Command::Turn { answer, .. } => Ok(Reply::Turn(answer.map_or(Turn::Take, Turn::Done))),
            Command::RecordChanges {
                record: Some(id),
                after,
                from,
            } => self.record_changes(&id, &after, from),
            Command::RecordChange {
                record: Some(id),
                changes,
                more,
                ..
            } => {
                self.record_change(&id, &changes.0, more)?;
                Ok(Reply::Written)
            }
            Command::RecordChanges { record: None, .. }
            | Command::RecordChange { record: None, .. } => {
                Err("a store held in this process opened no record of its own".to_owned())
            }
            Command::Records => Ok(Reply::Ids(
                self.documents
                    .borrow()
                    .range((self.app.clone(), String::new())..)
                    .take_while(|((of, _), _)| *of == self.app)
                    .map(|((_, id), _)| id.clone())
                    .collect(),
            )),
            Command::Steps {
                steps,
                invoker,
                kinds,
                redo,
            } => Ok(wire::steps(
                &mut self.store.borrow_mut(),
                steps,
                &invoker,
                &kinds,
                redo,
            )),
            command if command.writes() => {
                let written = wire::write(&mut self.store.borrow_mut(), command);
                match written {
                    Ok(written) => {
                        self.keep(&written)?;
                        Ok(Reply::Written)
                    }
                    Err(refused) => {
                        self.keep(&refused.written)?;
                        Err(refused.error.to_string())
                    }
                }
            }
            command => wire::read(&self.store.borrow(), &command, &|_| true)
                .ok_or_else(|| "the store answered no read".to_owned()),
        }
    }
}

/// A document id as a file name: every byte but a letter, a digit, `-` and `_` as `%xx`.
fn escape(doc: &str) -> String {
    let mut out = String::with_capacity(doc.len());
    for byte in doc.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02x}");
        }
    }
    out
}

fn unescape(name: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(name.len());
    let mut rest = name.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        if byte == b'%' {
            let hex = std::str::from_utf8(tail.get(..2)?).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            rest = tail.get(2..)?;
        } else {
            bytes.push(byte);
            rest = tail;
        }
    }
    String::from_utf8(bytes).ok()
}
