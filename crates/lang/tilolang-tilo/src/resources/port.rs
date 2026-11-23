//! Where a [`super::ResourceStore`] sends its commands: the device's one store, over the app's
//! connection ([`Port`]), or a store held in this process for a device of its own
//! ([`LocalPort`]): a window that runs with no workspace, and the tests.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::path::PathBuf;

use tilo_resource_store::wire::{self, Command, Reply};
use tilo_resource_store::{Schema, Store};

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
}

/// A store held in this process: everything in it is the device's person's to read. Kept on
/// disk under `dir` when given, each document's changes appended to a file of its own, and read
/// back whole when the store opens again.
pub struct LocalPort {
    store: RefCell<Store>,
    dir: Option<PathBuf>,
}

impl Default for LocalPort {
    fn default() -> Self {
        Self::in_memory()
    }
}

impl LocalPort {
    /// A store kept in memory only.
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            store: RefCell::new(Store::new()),
            dir: None,
        }
    }

    /// A store kept under `dir`, with what an earlier one kept there.
    ///
    /// # Errors
    /// When `dir` cannot be made or read, or holds a document that does not read.
    pub fn at(dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let mut store = Store::new();
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
            let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
            store
                .load(&doc, &bytes)
                .map_err(|error| format!("{doc} does not read: {error}"))?;
        }
        Ok(Self {
            store: RefCell::new(store),
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

    fn keep(&self, written: &[tilo_resource_store::Written]) -> Result<(), String> {
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
}

impl Port for LocalPort {
    fn declare(&self, schema: Schema) {
        self.store.borrow_mut().declare(schema);
    }

    fn command(&self, command: Command) -> Result<Reply, String> {
        // A workspace on this device alone: every turn is this device's.
        if let Command::Turn { answer, .. } = command {
            return Ok(Reply::Turn(
                answer.map_or(wire::Turn::Take, wire::Turn::Done),
            ));
        }
        if command.writes() {
            let written = wire::write(&mut self.store.borrow_mut(), command);
            return match written {
                Ok(written) => {
                    self.keep(&written)?;
                    Ok(Reply::Written)
                }
                Err(refused) => {
                    self.keep(&refused.written)?;
                    Err(refused.error.to_string())
                }
            };
        }
        wire::read(&self.store.borrow(), &command, &|_| true)
            .ok_or_else(|| "the store answered no read".to_owned())
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
    tilo_failure::ignored!(
        "escapes that decode to no text name no id",
        String::from_utf8(bytes).ok()
    )
}
