//! The index: what the room record holds of one package's resources, kept on disk, paged, and
//! stamped with the record heads it reflects (docs/designs/app-language/search.md, section 4).
//!
//! The store reads lists, filters and the fields rows show from the index, not from the
//! record, so opening a window costs what it shows; search and publication read the same
//! index. It holds, per resource (a package's record, or an item in a collection of one):
//!
//! * every field a view reads but a collection's: registers, counters, texts, sets and maps,
//!   with the resource's type and, for an item, its order key;
//! * each collection's items in order ([`lists`]);
//! * per item field a filter compares (a register of a flag, a number or a text, or a
//!   counter), a list per value of the items holding it, in the collection's order: a filter
//!   or a count is answered from the lists of the values it holds of;
//! * the postings of every text: per word, the resources it is in ([`text`]).
//!
//! The record stays the source of truth. The index is derived from it ([`derive`]) and kept
//! up to date change by change, field by field; one that reflects heads the record does not
//! hold, or that fails a checksum, is built anew and never read.
//!
//! The format is [redb](https://docs.rs/redb): an embedded store of B-trees in one file that
//! the workspace already depends on. Each commit is one transaction, so the file always holds
//! one commit's whole state and its heads; a reader holds a snapshot that later commits do not
//! change, which is what a paged listing reads from.

pub mod codec;
pub mod derive;
pub mod lists;
pub mod merged;
pub mod overlay;
pub mod text;
pub mod view;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use automerge::ChangeHash;
use redb::{
    Database, Durability, ReadOnlyTable, ReadableDatabase as _, ReadableTable as _, Table,
    TableDefinition, WriteTransaction,
};
use tilolang_runtime::store::FieldValue;

use self::codec::Corrupt;
use self::lists::{Dir, Entry, ListEdits, ListSource, Pages};

/// The version of what the index holds and how: an index of another is built anew.
pub const FORMAT: u32 = 1;

/// How much of the file a process keeps in memory, at most: pages read again come from the
/// system's cache of the file, which is not the process's memory.
const CACHE_BYTES: usize = 4 << 20;

const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
/// Each record of the package: its type.
const DOCS: TableDefinition<&str, &[u8]> = TableDefinition::new("docs");
/// `(resource, field)`: the field's value; `$type` and `$key` hold the resource's type and,
/// for an item, its order key.
const FIELDS: TableDefinition<(&str, &str), &[u8]> = TableDefinition::new("fields");
const DIRS: TableDefinition<&str, &[u8]> = TableDefinition::new("dirs");
const PAGES: TableDefinition<(&str, u32), &[u8]> = TableDefinition::new("pages");
/// `(word, resource)`: how much the word weighs there.
const POSTINGS: TableDefinition<(&str, &str), u8> = TableDefinition::new("postings");

/// The pseudo-field holding a resource's type.
pub const TYPE: &str = "$type";
/// The pseudo-field holding an item's order key.
pub const KEY: &str = "$key";

const META_FORMAT: &str = "format";
const META_SCHEMA: &str = "schema";
const META_HEADS: &str = "heads";

/// What went wrong reading or writing the index.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IndexError {
    /// The index holds what it did not write: it is built anew from the record.
    #[error("the index is corrupt")]
    Corrupt,
    #[error("the index cannot be read or written: {0}")]
    Io(String),
}

impl From<Corrupt> for IndexError {
    fn from(_: Corrupt) -> Self {
        Self::Corrupt
    }
}

fn redb_error(error: impl Into<redb::Error>) -> IndexError {
    match error.into() {
        redb::Error::Corrupted(_) | redb::Error::TableTypeMismatch { .. } => IndexError::Corrupt,
        other => IndexError::Io(other.to_string()),
    }
}

macro_rules! from_redb {
    ($($ty:ty),*) => {$(
        impl From<$ty> for IndexError {
            fn from(error: $ty) -> Self {
                redb_error(error)
            }
        }
    )*};
}

from_redb!(
    redb::Error,
    redb::DatabaseError,
    redb::TransactionError,
    redb::TableError,
    redb::StorageError,
    redb::CommitError,
    redb::SetDurabilityError
);

/// What the index read, counted since it was opened: work that does not depend on the machine,
/// which tests hold to what a window shows.
#[derive(Debug, Default)]
pub struct IndexStats {
    /// List directories read.
    pub dirs: AtomicU64,
    /// List pages read.
    pub pages: AtomicU64,
    /// Field values read one by one.
    pub fields: AtomicU64,
    /// Whether a read found the index corrupt.
    broken: AtomicBool,
}

/// [`IndexStats`] at one moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IndexReads {
    pub dirs: u64,
    pub pages: u64,
    pub fields: u64,
}

thread_local! {
    /// Whether this thread reads off the frame: its reads are not the frame's work.
    static OFF_FRAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Mark the calling thread as reading off the frame (a build or a first publication on a
/// thread of its own): what it reads is not counted as the frame's reads. The corruption a read
/// finds is reported all the same.
pub(crate) fn read_off_frame() {
    OFF_FRAME.with(|off| off.set(true));
}

fn on_frame() -> bool {
    !OFF_FRAME.with(std::cell::Cell::get)
}

impl IndexStats {
    pub(crate) fn count_dir(&self) {
        if on_frame() {
            self.dirs.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn count_page(&self) {
        if on_frame() {
            self.pages.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn count_field(&self) {
        if on_frame() {
            self.fields.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Say that a read found the index corrupt.
    pub fn corrupt(&self) {
        self.broken.store(true, Ordering::Relaxed);
    }

    /// Whether a read found the index corrupt.
    pub fn is_corrupt(&self) -> bool {
        self.broken.load(Ordering::Relaxed)
    }

    pub fn reads(&self) -> IndexReads {
        IndexReads {
            dirs: self.dirs.load(Ordering::Relaxed),
            pages: self.pages.load(Ordering::Relaxed),
            fields: self.fields.load(Ordering::Relaxed),
        }
    }
}

impl IndexReads {
    /// These reads and `other`'s.
    #[must_use]
    pub fn plus(self, other: Self) -> Self {
        Self {
            dirs: self.dirs.saturating_add(other.dirs),
            pages: self.pages.saturating_add(other.pages),
            fields: self.fields.saturating_add(other.fields),
        }
    }
}

/// One package's index in one workspace: a file, or memory for a room nobody keeps.
pub struct Index {
    app: String,
    /// The schema of the package's records it was built for.
    schema: String,
    db: Arc<Database>,
    stats: Arc<IndexStats>,
    path: Option<PathBuf>,
}

impl std::fmt::Debug for Index {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Index")
            .field("app", &self.app)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// What opening an index found.
#[derive(Debug)]
pub enum Opened {
    /// An index of this format and schema, complete, reflecting these heads.
    Ready(Index, Vec<ChangeHash>),
    /// Nothing usable (no file, another format or schema, a broken file): an empty index to
    /// build.
    Empty(Index),
}

/// The file of the index of `app` in the directory `dir`.
#[must_use]
pub fn file_of(dir: &Path, app: &str) -> PathBuf {
    let name: String = app
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    dir.join(format!("{name}.index"))
}

/// The directory of the indexes of the room whose record is `doc`, under `base`: named by the
/// object every replica of the room keeps its packages' records in, which they agree on.
/// `None` for a room that holds no record yet.
#[must_use]
pub fn room_dir(base: &Path, doc: &tilo_resource::Doc) -> Option<PathBuf> {
    use automerge::ReadDoc as _;
    let (_, records) = doc
        .get(&automerge::ROOT, crate::action::RECORDS_KEY)
        .ok()
        .flatten()?;
    let name: String = records
        .to_string()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    Some(base.join(name))
}

fn create_tables(txn: &WriteTransaction) -> Result<(), IndexError> {
    txn.open_table(META)?;
    txn.open_table(DOCS)?;
    txn.open_table(FIELDS)?;
    txn.open_table(DIRS)?;
    txn.open_table(PAGES)?;
    txn.open_table(POSTINGS)?;
    Ok(())
}

fn encode_heads(heads: &[ChangeHash]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(heads.len().saturating_mul(32));
    for head in heads {
        bytes.extend_from_slice(&head.0);
    }
    codec::seal(bytes)
}

fn decode_heads(sealed: &[u8]) -> Result<Vec<ChangeHash>, Corrupt> {
    let bytes = codec::open(sealed)?;
    if bytes.len() % 32 != 0 {
        return Err(Corrupt);
    }
    bytes
        .chunks_exact(32)
        .map(|chunk| ChangeHash::try_from(chunk).map_err(|_| Corrupt))
        .collect()
}

impl Index {
    /// The index of `app` at `path`, if it holds a complete index of this format and of the
    /// package's schema `schema`; else the file emptied (or made anew when it cannot be read
    /// at all) for a build. An index whose file needs repairing (the process ended in the
    /// middle of a commit) is built anew too: repairing reads the whole file.
    ///
    /// # Errors
    /// The directory or file cannot be made.
    pub fn open(path: &Path, app: &str, schema: &str) -> Result<Opened, IndexError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| IndexError::Io(e.to_string()))?;
        }
        let opened = Database::builder()
            .set_cache_size(CACHE_BYTES)
            .set_repair_callback(redb::RepairSession::abort)
            .create(path);
        let db = match opened {
            Ok(db) => db,
            // Another index holds the file (a second window on the room): it is not this one's
            // to empty.
            Err(redb::DatabaseError::DatabaseAlreadyOpen) => {
                return Err(IndexError::Io("the index is open elsewhere".into()));
            }
            Err(_) => {
                // Unreadable, or it needed repairing: what it held is not trusted.
                let _ = std::fs::remove_file(path);
                Database::builder()
                    .set_cache_size(CACHE_BYTES)
                    .create(path)?
            }
        };
        let index = Self {
            app: app.to_owned(),
            schema: schema.to_owned(),
            db: Arc::new(db),
            stats: Arc::default(),
            path: Some(path.to_owned()),
        };
        if let Ok(Some(heads)) = index.valid(schema) {
            Ok(Opened::Ready(index, heads))
        } else {
            index.clear(schema)?;
            Ok(Opened::Empty(index))
        }
    }

    /// An empty index of `app` in memory, for a room that is not kept on disk.
    ///
    /// # Errors
    /// Memory cannot be had for it.
    pub fn in_memory(app: &str, schema: &str) -> Result<Self, IndexError> {
        let db = Database::builder().create_with_backend(redb::backends::InMemoryBackend::new())?;
        let index = Self {
            app: app.to_owned(),
            schema: schema.to_owned(),
            db: Arc::new(db),
            stats: Arc::default(),
            path: None,
        };
        index.clear(schema)?;
        Ok(index)
    }

    #[must_use]
    pub fn app(&self) -> &str {
        &self.app
    }

    /// Where the index is kept, when it is kept on disk.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    #[must_use]
    pub fn stats(&self) -> &Arc<IndexStats> {
        &self.stats
    }

    /// The heads the index reflects, when it is a complete index of this format and of the
    /// schema `schema`.
    fn valid(&self, schema: &str) -> Result<Option<Vec<ChangeHash>>, IndexError> {
        let txn = self.db.begin_read()?;
        let Ok(meta) = txn.open_table(META) else {
            return Ok(None);
        };
        let format = meta
            .get(META_FORMAT)?
            .map(|v| codec::get_text(v.value()))
            .transpose()?;
        let held = meta
            .get(META_SCHEMA)?
            .map(|v| codec::get_text(v.value()))
            .transpose()?;
        if format.as_deref() != Some(FORMAT.to_string().as_str()) || held.as_deref() != Some(schema)
        {
            return Ok(None);
        }
        meta.get(META_HEADS)?
            .map(|v| decode_heads(v.value()))
            .transpose()
            .map_err(Into::into)
    }

    /// Empty the index for a build: no heads, so it is not read until the build commits.
    fn clear(&self, schema: &str) -> Result<(), IndexError> {
        let txn = self.db.begin_write()?;
        txn.delete_table(META)?;
        txn.delete_table(DOCS)?;
        txn.delete_table(FIELDS)?;
        txn.delete_table(DIRS)?;
        txn.delete_table(PAGES)?;
        txn.delete_table(POSTINGS)?;
        create_tables(&txn)?;
        {
            let mut meta = txn.open_table(META)?;
            meta.insert(META_FORMAT, codec::put_text(&FORMAT.to_string()).as_slice())?;
            meta.insert(META_SCHEMA, codec::put_text(schema).as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    /// Empty the index for a build from the record: until the build commits it reflects no
    /// heads, so a process that ends in the middle of it finds nothing to trust.
    ///
    /// # Errors
    /// The index cannot be written.
    pub fn clear_for_build(&self) -> Result<(), IndexError> {
        self.clear(&self.schema)
    }

    /// The index as the last commit left it, to read: what later commits do not change.
    ///
    /// # Errors
    /// The index cannot be read.
    pub fn snapshot(&self) -> Result<Arc<Snapshot>, IndexError> {
        let txn = self.db.begin_read()?;
        Ok(Arc::new(Snapshot {
            docs: txn.open_table(DOCS)?,
            fields: txn.open_table(FIELDS)?,
            dirs: txn.open_table(DIRS)?,
            pages: txn.open_table(PAGES)?,
            postings: txn.open_table(POSTINGS)?,
            stats: Arc::clone(&self.stats),
        }))
    }

    /// Change the index in one commit: what `f` writes, all of it or none. The commit is not
    /// waited on to reach the disk ([`Self::persist`] does); the index is derived, and one that
    /// lost its last commits reflects older heads and catches up.
    ///
    /// # Errors
    /// What `f` returns, or the commit fails.
    pub fn write<T>(
        &self,
        f: impl FnOnce(&mut Writer<'_>) -> Result<T, IndexError>,
    ) -> Result<T, IndexError> {
        let mut txn = self.db.begin_write()?;
        txn.set_durability(Durability::None)?;
        let out = {
            let mut writer = Writer::new(&txn)?;
            let out = f(&mut writer)?;
            writer.flush()?;
            out
        };
        txn.commit()?;
        Ok(out)
    }

    /// Wait for the commits so far to reach the disk.
    ///
    /// # Errors
    /// The disk refuses the write.
    pub fn persist(&self) -> Result<(), IndexError> {
        let mut txn = self.db.begin_write()?;
        txn.set_durability(Durability::Immediate)?;
        txn.commit()?;
        Ok(())
    }
}

impl Drop for Index {
    fn drop(&mut self) {
        if self.path.is_some() {
            // What it holds reaches the disk: the next open catches up from here.
            let _ = self.persist();
        }
    }
}

type FieldsTable = ReadOnlyTable<(&'static str, &'static str), &'static [u8]>;

/// The index as one commit left it, read on any thread.
pub struct Snapshot {
    docs: ReadOnlyTable<&'static str, &'static [u8]>,
    fields: FieldsTable,
    dirs: ReadOnlyTable<&'static str, &'static [u8]>,
    pages: ReadOnlyTable<(&'static str, u32), &'static [u8]>,
    postings: ReadOnlyTable<(&'static str, &'static str), u8>,
    stats: Arc<IndexStats>,
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshot").finish_non_exhaustive()
    }
}

/// The last resource path that starts with `path/`: what a range over the resources inside
/// `path` stops before.
fn inside_end(path: &str) -> String {
    format!("{path}0")
}

impl Snapshot {
    fn checked<T>(&self, read: Result<T, IndexError>) -> Result<T, IndexError> {
        if matches!(read, Err(IndexError::Corrupt)) {
            self.stats.corrupt();
        }
        read
    }

    #[must_use]
    pub fn stats(&self) -> &Arc<IndexStats> {
        &self.stats
    }

    /// The value of `field` of the resource `resource`, if the index holds one.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn field(&self, resource: &str, field: &str) -> Result<Option<FieldValue>, IndexError> {
        self.stats.count_field();
        let read = (|| {
            let Some(value) = self.fields.get((resource, field))? else {
                return Ok(None);
            };
            Ok(Some(codec::get_value(value.value())?))
        })();
        self.checked(read)
    }

    /// The type (`$type`) or order key (`$key`) of the resource `resource`.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn text(&self, resource: &str, key: &str) -> Result<Option<String>, IndexError> {
        let read = (|| {
            let Some(value) = self.fields.get((resource, key))? else {
                return Ok(None);
            };
            Ok(Some(codec::get_text(value.value())?))
        })();
        self.checked(read)
    }

    /// The package's records and their types.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn docs(&self) -> Result<Vec<(String, String)>, IndexError> {
        let read = (|| {
            let mut out = Vec::new();
            for entry in self.docs.iter()? {
                let (doc, ty) = entry?;
                out.push((doc.value().to_owned(), codec::get_text(ty.value())?));
            }
            Ok(out)
        })();
        self.checked(read)
    }

    /// Every field the index holds of the resource `resource`, by field id (`$type` and
    /// `$key` apart).
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn fields_of(&self, resource: &str) -> Result<Vec<(String, FieldValue)>, IndexError> {
        let read = (|| {
            let mut out = Vec::new();
            for entry in self
                .fields
                .range((resource, "")..=(resource, "\u{10ffff}"))?
            {
                let (key, value) = entry?;
                let (_, field) = key.value();
                if field.starts_with('$') {
                    continue;
                }
                out.push((field.to_owned(), codec::get_value(value.value())?));
            }
            Ok(out)
        })();
        self.checked(read)
    }

    /// The resources inside the resource `path` (its items, theirs, and so on), in path order.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn inside(&self, path: &str) -> Result<Vec<String>, IndexError> {
        let read = (|| {
            let start = format!("{path}/");
            let end = inside_end(path);
            let mut out: Vec<String> = Vec::new();
            for entry in self
                .fields
                .range((start.as_str(), "")..(end.as_str(), ""))?
            {
                let (key, _) = entry?;
                let (resource, field) = key.value();
                if field == TYPE {
                    out.push(resource.to_owned());
                }
            }
            Ok(out)
        })();
        self.checked(read)
    }

    /// The list `list`, read page by page.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn pages(self: &Arc<Self>, list: &str) -> Result<Pages<Self>, IndexError> {
        let read = Pages::open(Arc::clone(self), list, Arc::clone(&self.stats));
        self.checked(read)
    }

    /// The lists whose names start with `prefix`, with their directories.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn lists_from(&self, prefix: &str) -> Result<Vec<(String, Dir)>, IndexError> {
        let read = (|| {
            let mut out = Vec::new();
            for entry in self.dirs.range(prefix..)? {
                let (list, dir) = entry?;
                let list = list.value();
                if !list.starts_with(prefix) {
                    break;
                }
                self.stats.count_dir();
                out.push((list.to_owned(), Dir::decode(dir.value())?));
            }
            Ok(out)
        })();
        self.checked(read)
    }

    /// The postings of the words starting with `prefix` (of the word itself, with `exact`):
    /// each resource holding one, and how much it weighs there.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn postings(&self, prefix: &str, exact: bool) -> Result<Vec<(String, u8)>, IndexError> {
        let read = (|| {
            let mut out = Vec::new();
            for entry in self.postings.range((prefix, "")..)? {
                let (key, weight) = entry?;
                let (word, resource) = key.value();
                if (exact && word != prefix) || !word.starts_with(prefix) {
                    break;
                }
                out.push((resource.to_owned(), weight.value()));
            }
            Ok(out)
        })();
        self.checked(read)
    }
}

/// Everything an index holds, as it stands for what it holds: lists by their entries, not by
/// how they are cut into pages. Two indexes of the same record hold the same.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Contents {
    pub docs: std::collections::BTreeMap<String, String>,
    pub fields: std::collections::BTreeMap<(String, String), String>,
    pub lists: std::collections::BTreeMap<String, Vec<Entry>>,
    pub postings: std::collections::BTreeMap<(String, String), u8>,
}

impl Snapshot {
    /// Everything it holds: what tests compare an index kept change by change with one built
    /// from the record whole.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn contents(&self) -> Result<Contents, IndexError> {
        let mut out = Contents::default();
        for (doc, ty) in self.docs()? {
            out.docs.insert(doc, ty);
        }
        for entry in self.fields.iter()? {
            let (key, value) = entry?;
            let (resource, field) = key.value();
            let shown = if field.starts_with('$') {
                codec::get_text(value.value())?
            } else {
                format!("{:?}", codec::get_value(value.value())?)
            };
            out.fields
                .insert((resource.to_owned(), field.to_owned()), shown);
        }
        let mut names = Vec::new();
        for entry in self.dirs.iter()? {
            let (list, _) = entry?;
            names.push(list.value().to_owned());
        }
        for name in names {
            let dir = lists::read_dir(self, &name)?;
            let mut entries = Vec::new();
            for page in &dir.pages {
                entries.extend(lists::read_page(self, &name, page.seq)?);
            }
            if !entries.is_empty() {
                out.lists.insert(name, entries);
            }
        }
        for entry in self.postings.iter()? {
            let (key, weight) = entry?;
            let (word, resource) = key.value();
            out.postings
                .insert((word.to_owned(), resource.to_owned()), weight.value());
        }
        Ok(out)
    }
}

impl ListSource for Snapshot {
    fn dir_bytes(&self, list: &str) -> Result<Option<Vec<u8>>, IndexError> {
        Ok(self.dirs.get(list)?.map(|v| v.value().to_vec()))
    }

    fn page_bytes(&self, list: &str, seq: u32) -> Result<Option<Vec<u8>>, IndexError> {
        Ok(self.pages.get((list, seq))?.map(|v| v.value().to_vec()))
    }
}

/// The tables of one write, read through by the lists being changed in it.
pub struct Tables<'t> {
    meta: Table<'t, &'static str, &'static [u8]>,
    docs: Table<'t, &'static str, &'static [u8]>,
    fields: Table<'t, (&'static str, &'static str), &'static [u8]>,
    dirs: Table<'t, &'static str, &'static [u8]>,
    pages: Table<'t, (&'static str, u32), &'static [u8]>,
    postings: Table<'t, (&'static str, &'static str), u8>,
}

impl ListSource for Tables<'_> {
    fn dir_bytes(&self, list: &str) -> Result<Option<Vec<u8>>, IndexError> {
        Ok(self.dirs.get(list)?.map(|v| v.value().to_vec()))
    }

    fn page_bytes(&self, list: &str, seq: u32) -> Result<Option<Vec<u8>>, IndexError> {
        Ok(self.pages.get((list, seq))?.map(|v| v.value().to_vec()))
    }
}

/// One commit being written: see [`Index::write`].
pub struct Writer<'t> {
    tables: Tables<'t>,
    lists: ListEdits,
}

impl<'t> Writer<'t> {
    fn new(txn: &'t WriteTransaction) -> Result<Self, IndexError> {
        Ok(Self {
            tables: Tables {
                meta: txn.open_table(META)?,
                docs: txn.open_table(DOCS)?,
                fields: txn.open_table(FIELDS)?,
                dirs: txn.open_table(DIRS)?,
                pages: txn.open_table(PAGES)?,
                postings: txn.open_table(POSTINGS)?,
            },
            lists: ListEdits::default(),
        })
    }

    fn flush(&mut self) -> Result<(), IndexError> {
        let (dirs, pages) = std::mem::take(&mut self.lists).finish();
        for ((list, seq), entries) in pages {
            match entries {
                Some(entries) => {
                    self.tables.pages.insert(
                        (list.as_str(), seq),
                        lists::encode_page(&entries).as_slice(),
                    )?;
                }
                None => {
                    self.tables.pages.remove((list.as_str(), seq))?;
                }
            }
        }
        for (list, dir) in dirs {
            match dir {
                Some(dir) => {
                    self.tables
                        .dirs
                        .insert(list.as_str(), dir.encode().as_slice())?;
                }
                None => {
                    self.tables.dirs.remove(list.as_str())?;
                }
            }
        }
        Ok(())
    }

    /// Stamp the index with the heads it reflects once this commit is in.
    ///
    /// # Errors
    /// The index cannot be written.
    pub fn set_heads(&mut self, heads: &[ChangeHash]) -> Result<(), IndexError> {
        self.tables
            .meta
            .insert(META_HEADS, encode_heads(heads).as_slice())?;
        Ok(())
    }

    /// # Errors
    /// The index cannot be written.
    pub fn put_doc(&mut self, doc: &str, ty: &str) -> Result<(), IndexError> {
        self.tables
            .docs
            .insert(doc, codec::put_text(ty).as_slice())?;
        Ok(())
    }

    /// # Errors
    /// The index cannot be written.
    pub fn remove_doc(&mut self, doc: &str) -> Result<(), IndexError> {
        self.tables.docs.remove(doc)?;
        Ok(())
    }

    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn field(&self, resource: &str, field: &str) -> Result<Option<FieldValue>, IndexError> {
        let Some(value) = self.tables.fields.get((resource, field))? else {
            return Ok(None);
        };
        Ok(Some(codec::get_value(value.value())?))
    }

    /// # Errors
    /// The index cannot be written.
    pub fn put_field(
        &mut self,
        resource: &str,
        field: &str,
        value: &FieldValue,
    ) -> Result<(), IndexError> {
        self.tables
            .fields
            .insert((resource, field), codec::put_value(value).as_slice())?;
        Ok(())
    }

    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn text(&self, resource: &str, key: &str) -> Result<Option<String>, IndexError> {
        let Some(value) = self.tables.fields.get((resource, key))? else {
            return Ok(None);
        };
        Ok(Some(codec::get_text(value.value())?))
    }

    /// # Errors
    /// The index cannot be written.
    pub fn put_text(&mut self, resource: &str, key: &str, text: &str) -> Result<(), IndexError> {
        self.tables
            .fields
            .insert((resource, key), codec::put_text(text).as_slice())?;
        Ok(())
    }

    /// Every field of `resource` and of the resources inside it, with what each holds.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn subtree(&self, resource: &str) -> Result<Vec<(String, String, Vec<u8>)>, IndexError> {
        let mut out = Vec::new();
        for entry in self
            .tables
            .fields
            .range((resource, "")..=(resource, "\u{10ffff}"))?
        {
            let (key, value) = entry?;
            let (r, f) = key.value();
            out.push((r.to_owned(), f.to_owned(), value.value().to_vec()));
        }
        let start = format!("{resource}/");
        let end = inside_end(resource);
        for entry in self
            .tables
            .fields
            .range((start.as_str(), "")..(end.as_str(), ""))?
        {
            let (key, value) = entry?;
            let (r, f) = key.value();
            out.push((r.to_owned(), f.to_owned(), value.value().to_vec()));
        }
        Ok(out)
    }

    /// Drop the field `field` of `resource`.
    ///
    /// # Errors
    /// The index cannot be written.
    pub fn remove_field(&mut self, resource: &str, field: &str) -> Result<(), IndexError> {
        self.tables.fields.remove((resource, field))?;
        Ok(())
    }

    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn list(&mut self, list: &str) -> Result<Vec<Entry>, IndexError> {
        self.lists.entries(&self.tables, list)
    }

    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn list_insert(&mut self, list: &str, entry: Entry) -> Result<(), IndexError> {
        self.lists.insert(&self.tables, list, entry)
    }

    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn list_remove(&mut self, list: &str, entry: &Entry) -> Result<(), IndexError> {
        self.lists.remove(&self.tables, list, entry)
    }

    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn list_replace(&mut self, list: &str, entries: Vec<Entry>) -> Result<(), IndexError> {
        self.lists.replace(&self.tables, list, entries)
    }

    /// Drop every list whose name starts with `prefix`.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn drop_lists(&mut self, prefix: &str) -> Result<(), IndexError> {
        for name in self.lists_from(prefix)? {
            self.lists.drop_list(&self.tables, &name)?;
        }
        Ok(())
    }

    /// The names of the lists starting with `prefix`, as this commit leaves them.
    ///
    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn lists_from(&mut self, prefix: &str) -> Result<Vec<String>, IndexError> {
        let mut names = std::collections::BTreeSet::new();
        for entry in self.tables.dirs.range(prefix..)? {
            let (list, _) = entry?;
            let list = list.value();
            if !list.starts_with(prefix) {
                break;
            }
            names.insert(list.to_owned());
        }
        // As this commit leaves them: lists it made, not those it emptied.
        for (name, empty) in self.lists.held_from(prefix) {
            if empty {
                names.remove(&name);
            } else {
                names.insert(name);
            }
        }
        Ok(names.into_iter().collect())
    }

    /// # Errors
    /// The index cannot be written.
    pub fn post(&mut self, word: &str, resource: &str, weight: u8) -> Result<(), IndexError> {
        self.tables.postings.insert((word, resource), weight)?;
        Ok(())
    }

    /// # Errors
    /// The index cannot be written.
    pub fn unpost(&mut self, word: &str, resource: &str) -> Result<(), IndexError> {
        self.tables.postings.remove((word, resource))?;
        Ok(())
    }
}
