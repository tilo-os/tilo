//! The index: what the workspace record holds of one package's resources, kept on disk, paged, and
//! stamped with the record heads it reflects (docs/designs/app-language/search.md, "Who indexes").
//!
//! The store reads lists, filters and the fields rows show from the index, not from the
//! record, so opening a window costs what it shows; search and publication read the same
//! index. It holds, per resource (a package's record, or an item of one, by its id `doc/item`):
//!
//! * every field a view reads but a collection's: registers, counters, texts, sets and maps,
//!   with the resource's type and, for an item, the collection it is in and its order key;
//! * each collection's items in order ([`lists`]);
//! * per item field a filter compares (a register of a flag, a number or a text, or a
//!   counter), a list per value of the items holding it, in the collection's order: a filter
//!   or a count is answered from the lists of the values it holds of;
//!
//! The record stays the source of truth. The index is derived from it ([`derive`]) and kept
//! up to date change by change, field by field; one that reflects heads the record does not
//! hold, or that fails a checksum, is built anew and never read.
//!
//! The format is [redb](https://docs.rs/redb): an embedded store of B-trees in one file that
//! the workspace already depends on. Each commit is one transaction, so the file always holds
//! one commit's whole state and its heads; a reader holds a snapshot that later commits do not
//! change, which is what a paged listing reads from.
//!
//! # Where it is kept
//!
//! Apps own neither their pixels nor their storage, so the index belongs with the committed
//! documents, in the platform's storage service, where the search host would query the same
//! index instead of keeping a copy of its own. For now it is kept in the app's cache directory,
//! per workspace ([`workspace_dir`]), and only the app reads it. The storage service holds the
//! committed documents only of workspaces a host governs: the workspaces this device minted, joined with
//! a direct invite, or keeps as a person's own still go through the shell's own engine, and
//! their records live in the app's profile. Moved now, the index would have no record to be
//! derived from in those workspaces, or would need a second path for them. Once every workspace's
//! committed documents live in one storage service (the shell's two engines made one), the
//! index moves there: derived beside the record as each change commits, read by apps and the
//! search host through the committed-document reads they already make.

pub mod clock;
pub mod codec;
pub mod columns;
pub mod derive;
pub mod lists;
pub mod merged;
pub mod overlay;
pub mod reflect;
pub mod select;
pub mod view;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use automerge::ChangeHash;
use redb::{
    Database, Durability, ReadOnlyTable, ReadableDatabase as _, ReadableTable as _, Table,
    TableDefinition, WriteTransaction,
};
use tilolang_runtime::store::{FieldValue, Version};

use self::codec::Corrupt;
use self::lists::{Dir, Entry, ListEdits, ListSource, Pages};
use crate::chunk::{ChunkNo, Stamps};

/// The version of what the index holds and how: an index of another is built anew.
pub const FORMAT: u32 = 16;

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
/// The word postings of format 2, which search no longer reads: emptying the index drops them.
const FORMAT_2_POSTINGS: TableDefinition<(&str, &str), u8> = TableDefinition::new("postings");
/// `(list, value)`: how many entries of a column's list hold the value, for a column whose
/// values are few (a toggle, an enum): where a value's run starts and ends, and the counts
/// of a filter's chips, without reading the list's pages.
const COUNTS: TableDefinition<(&str, &str), u64> = TableDefinition::new("counts");
/// Each resource's newest version, as a [`reflect::Stamp`]: what a mark of the resource, or of the document
/// holding it, reads without the resource's log. Keyed by the resource alone, so a document's
/// resources are read as a page of them, never the document whole.
const NEWEST: TableDefinition<&str, &[u8]> = TableDefinition::new("newest");

/// The pseudo-field holding a resource's type.
pub const TYPE: &str = "$type";
/// The pseudo-field holding an item's order key.
pub const KEY: &str = "$key";
/// The pseudo-field holding the chunk of the record an item's map is stored in: where a read or
/// a write of the item goes, without a scan of the chunks.
pub const CHUNK: &str = "$chunk";
/// The pseudo-field holding the collection an item is in: its holder and field
/// ([`derive::in_of`]).
pub const IN: &str = "$in";
/// The pseudo-field of an item holding the collection it left from ([`derive::in_of`]), kept
/// while it is out of every collection: a later removal of it names the collection without
/// reading the record's log.
pub const LEFT: &str = "$left";

/// The pseudo-field of a record holding that some move of an item of it did not take effect,
/// as it closed a cycle ([`tilolang_runtime::place`]): a move in it is derived with the whole
/// record.
pub const CYCLES: &str = "$cycles";

/// The pseudo-field of a resource holding the items that left a collection of it, in the order
/// they left, each with the version that removed it: what a list shows of them struck, until
/// the person has seen them go. The field's own name follows [`removed_field`].
pub const REMOVED: &str = "$removed";

/// The pseudo-field of the resource holding `field`'s collection that keeps the items which
/// left it ([`REMOVED`]).
#[must_use]
pub fn removed_field(field: &str) -> String {
    format!("{REMOVED}~{field}")
}

const META_FORMAT: &str = "format";
const META_SCHEMA: &str = "schema";
/// The heads the index reflects of chunk `n`, as `heads.<n>`: the record's own log in chunk
/// [`chunk::FIRST`], and each chunk above it it was built from.
const META_HEADS: &str = "heads.";
/// The instant the columns of a type that read the clock were written at, as
/// `clock.<type id>` ([`clock`]): what a later read of it writes again for.
const META_CLOCK: &str = "clock.";

/// The time the columns that read the clock are written at: the instant `now` (seconds since
/// the epoch, to the minute, as a query reads the clock) and the person's zone (seconds east of
/// UTC), so `today` is their day.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Clock {
    pub now: i64,
    pub zone: i64,
}

impl Clock {
    /// The clock as `now` reads it (ISO-8601 UTC text, `YYYY-MM-DDTHH:MM:SSZ`) in `zone`
    /// (seconds east of UTC): the instant to the minute, as a query reads it.
    #[must_use]
    pub fn at(now: &str, zone: i64) -> Self {
        let now = tilolang::calendar::instant(now).unwrap_or(0);
        Self {
            now: now.saturating_sub(now.rem_euclid(60)),
            zone,
        }
    }
}

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

/// One package's index in one workspace: a file, or memory for a workspace nobody keeps.
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
    /// An index of this format and schema, complete, reflecting these heads of each chunk.
    Ready(Index, Stamps),
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

/// The directory of the indexes of the workspace `workspace` (the workspace a window's launch names),
/// under `base`: known before the workspace's record is, so a window opens over them first. Spelled
/// `rooms` because that is the directory an installed machine already keeps its indexes in; the
/// indexes are derived from the records, so a build that wrote them elsewhere rebuilds them here.
#[must_use]
pub fn workspace_dir(base: &Path, workspace: &str) -> PathBuf {
    let name: String = workspace
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    base.join("rooms").join(name)
}

/// The directory of the indexes of the record `record` of the workspace `workspace` under `base`: the
/// workspace's ([`workspace_dir`]) for the workspace's record of an app, and one of its own for another
/// record (a member's own), so what one record holds is never shown as another's.
#[must_use]
pub fn record_dir(base: &Path, workspace: &str, record: Option<&str>) -> PathBuf {
    match record {
        None => workspace_dir(base, workspace),
        Some(record) => workspace_dir(base, &format!("{workspace}~{record}")),
    }
}

fn create_tables(txn: &WriteTransaction) -> Result<(), IndexError> {
    txn.open_table(META)?;
    txn.open_table(DOCS)?;
    txn.open_table(FIELDS)?;
    txn.open_table(DIRS)?;
    txn.open_table(PAGES)?;
    txn.open_table(COUNTS)?;
    txn.open_table(NEWEST)?;
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

/// The key of the heads the index reflects of `chunk`.
fn heads_of(chunk: ChunkNo) -> String {
    format!("{META_HEADS}{chunk}")
}

/// A record's row: its type, and the chunk that holds it.
fn encode_doc(ty: &str, chunk: ChunkNo) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(ty.len().saturating_add(8));
    bytes.extend_from_slice(&chunk.to_le_bytes());
    bytes.extend_from_slice(ty.as_bytes());
    codec::seal(bytes)
}

fn decode_doc(sealed: &[u8]) -> Result<(String, ChunkNo), Corrupt> {
    let bytes = codec::open(sealed)?;
    if bytes.len() < 8 {
        return Err(Corrupt);
    }
    let (chunk, ty) = bytes.split_at(8);
    let chunk = ChunkNo::from_le_bytes(chunk.try_into().map_err(|_| Corrupt)?);
    let ty = String::from_utf8(ty.to_vec()).map_err(|_| Corrupt)?;
    Ok((ty, chunk))
}

impl Index {
    /// The index of `app` at `path`, if it holds a complete index of this format and of the
    /// package's schema `schema`; else the file emptied (or made anew when it cannot be read
    /// at all) for a build. An index whose file needs repairing (the process ended in the
    /// middle of a commit) is built anew too: repairing reads the whole file.
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
            // Another index holds the file (a second window on the workspace): it is not this one's
            // to empty.
            Err(redb::DatabaseError::DatabaseAlreadyOpen) => {
                return Err(IndexError::Io("the index is open elsewhere".into()));
            }
            Err(error) => {
                // Unreadable, or it needed repairing: what it held is not trusted.
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "an index could not be opened: it is built anew"
                );
                // Absent already is as good as removed; the create below says what is wrong.
                let _ = std::fs::remove_file(path);
                Database::builder()
                    .set_cache_size(CACHE_BYTES)
                    .create(path)?
            }
        };
        let index = Self::with(app, schema, db, Some(path.to_owned()));
        if let Ok(Some(heads)) = index.valid(schema) {
            Ok(Opened::Ready(index, heads))
        } else {
            index.clear(schema)?;
            Ok(Opened::Empty(index))
        }
    }

    /// An empty index of `app` in memory, for a workspace that is not kept on disk.
    pub fn in_memory(app: &str, schema: &str) -> Result<Self, IndexError> {
        let db = Database::builder().create_with_backend(redb::backends::InMemoryBackend::new())?;
        let index = Self::with(app, schema, db, None);
        index.clear(schema)?;
        Ok(index)
    }

    fn with(app: &str, schema: &str, db: Database, path: Option<PathBuf>) -> Self {
        let db = Arc::new(db);
        let stats: Arc<IndexStats> = Arc::default();
        Self {
            app: app.to_owned(),
            schema: schema.to_owned(),
            db,
            stats,
            path,
        }
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

    /// The heads the index reflects, per chunk, when it is a complete index of this format and
    /// of the schema `schema`.
    fn valid(&self, schema: &str) -> Result<Option<Stamps>, IndexError> {
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
        let mut heads = Stamps::new();
        for entry in meta.iter()? {
            let (key, value) = entry?;
            let Some(chunk) = key.value().strip_prefix(META_HEADS) else {
                continue;
            };
            let Ok(chunk) = chunk.parse::<ChunkNo>() else {
                return Ok(None);
            };
            heads.insert(chunk, decode_heads(value.value())?);
        }
        Ok(Some(heads))
    }

    /// Empty the index for a build: no heads, so it is not read until the build commits.
    fn clear(&self, schema: &str) -> Result<(), IndexError> {
        let txn = self.db.begin_write()?;
        txn.delete_table(META)?;
        txn.delete_table(DOCS)?;
        txn.delete_table(FIELDS)?;
        txn.delete_table(DIRS)?;
        txn.delete_table(PAGES)?;
        txn.delete_table(FORMAT_2_POSTINGS)?;
        txn.delete_table(COUNTS)?;
        txn.delete_table(NEWEST)?;
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
    pub fn clear_for_build(&self) -> Result<(), IndexError> {
        self.clear(&self.schema)
    }

    /// The index as the last commit left it, to read: what later commits do not change.
    pub fn snapshot(&self) -> Result<Arc<Snapshot>, IndexError> {
        let txn = self.db.begin_read()?;
        Ok(Arc::new(Snapshot {
            docs: txn.open_table(DOCS)?,
            fields: txn.open_table(FIELDS)?,
            dirs: txn.open_table(DIRS)?,
            pages: txn.open_table(PAGES)?,
            counts: txn.open_table(COUNTS)?,
            newest: txn.open_table(NEWEST)?,
            stats: Arc::clone(&self.stats),
        }))
    }

    /// Change the index in one commit: what `f` writes, all of it or none. The commit is not
    /// waited on to reach the disk ([`Self::persist`] does); the index is derived, and one that
    /// lost its last commits reflects older heads and catches up.
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
            if let Err(error) = self.persist() {
                tracing::warn!(%error, "an index did not reach the disk as it closed");
            }
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
    counts: ReadOnlyTable<(&'static str, &'static str), u64>,
    newest: ReadOnlyTable<&'static str, &'static [u8]>,
    stats: Arc<IndexStats>,
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshot").finish_non_exhaustive()
    }
}

/// One resource as [`Snapshot::texted`] reads it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Texted {
    pub path: String,
    /// Its type, if the index holds one.
    pub ty: Option<String>,
    /// The text values of its fields, by field id: texts and rich texts as stored
    /// ([`crate::reads::text_value`]), read for a reader by whoever shows them.
    pub texts: std::collections::BTreeMap<String, FieldValue>,
}

/// The last resource path that starts with `path/`: what a range over the resources inside
/// `path` stops before.
fn inside_end(path: &str) -> String {
    format!("{path}0")
}

/// Whether `resource` is `path` or a resource inside it.
fn inside(path: &str, resource: &str) -> bool {
    resource == path
        || resource
            .strip_prefix(path)
            .is_some_and(|rest| rest.starts_with('/'))
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
    pub fn text(&self, resource: &str, key: &str) -> Result<Option<String>, IndexError> {
        let read = (|| {
            let Some(value) = self.fields.get((resource, key))? else {
                return Ok(None);
            };
            Ok(Some(codec::get_text(value.value())?))
        })();
        self.checked(read)
    }

    /// Every resource the index holds (records and items, in path order), with its type and
    /// the text values of its fields by field id: one pass over the fields, what lists every
    /// resource search finds.
    pub fn texted(&self) -> Result<Vec<Texted>, IndexError> {
        let read = (|| {
            let mut out: Vec<Texted> = Vec::new();
            for entry in self.fields.iter()? {
                let (key, value) = entry?;
                let (at, field) = key.value();
                if out.last().is_none_or(|last| last.path != at) {
                    out.push(Texted {
                        path: at.to_owned(),
                        ty: None,
                        texts: std::collections::BTreeMap::new(),
                    });
                }
                let Some(last) = out.last_mut() else {
                    continue;
                };
                if field == TYPE {
                    last.ty = Some(codec::get_text(value.value())?);
                } else if !field.starts_with('$')
                    && let Some(text) = crate::reads::text_value(codec::get_value(value.value())?)
                {
                    last.texts.insert(field.to_owned(), text);
                }
            }
            Ok(out)
        })();
        self.checked(read)
    }

    /// The package's records, their types, and the chunk that holds each.
    pub fn docs(&self) -> Result<Vec<(String, String, ChunkNo)>, IndexError> {
        let read = (|| {
            let mut out = Vec::new();
            for entry in self.docs.iter()? {
                let (doc, ty) = entry?;
                let (ty, chunk) = decode_doc(ty.value())?;
                out.push((doc.value().to_owned(), ty, chunk));
            }
            Ok(out)
        })();
        self.checked(read)
    }

    /// Every field the index holds of the resource `resource`, by field id (`$type` and
    /// `$key` apart).
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

    /// The newest version the index holds of `resource`, else `None`: what a mark of it says,
    /// read without the resource's log.
    pub fn newest(&self, resource: &str) -> Result<Option<Version>, IndexError> {
        let read = (|| {
            let Some(value) = self.newest.get(resource)? else {
                return Ok(None);
            };
            let mut version = reflect::Stamp::of_value(&codec::get_value(value.value())?)
                .ok_or(IndexError::Corrupt)?
                .version;
            resource.clone_into(&mut version.resource);
            Ok(Some(version))
        })();
        self.checked(read)
    }

    /// The newest version of each resource of the document `path` (it and its items, and the
    /// items a collection of one of them had removed), in path order, at most `page` of them,
    /// and whether the document holds more: what a window's mark counts the unseen changes of,
    /// reading no log and no more of the document than the page.
    pub fn newest_page(
        &self,
        path: &str,
        page: usize,
    ) -> Result<(Vec<(String, reflect::Stamp)>, bool), IndexError> {
        let read = (|| {
            let end = inside_end(path);
            let mut out = Vec::new();
            let mut more = false;
            for entry in self.newest.range(path..end.as_str())? {
                let (resource, value) = entry?;
                let resource = resource.value();
                if !inside(path, resource) {
                    continue;
                }
                if out.len() == page {
                    more = true;
                    break;
                }
                let stamp = reflect::Stamp::of_value(&codec::get_value(value.value())?)
                    .ok_or(IndexError::Corrupt)?;
                out.push((resource.to_owned(), stamp));
            }
            Ok((out, more))
        })();
        self.checked(read)
    }

    /// The list `list`, read page by page.
    pub fn pages(self: &Arc<Self>, list: &str) -> Result<Pages<Self>, IndexError> {
        let read = Pages::open(Arc::clone(self), list, Arc::clone(&self.stats));
        self.checked(read)
    }

    /// The lists whose names start with `prefix`, with their directories.
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

    /// How many entries of the list `list` hold each value, for a column that counts them
    /// ([`COUNTS`]), in the values' order: one read, counted as one field's.
    pub fn counts(&self, list: &str) -> Result<Vec<(String, u64)>, IndexError> {
        self.stats.count_field();
        let read = (|| {
            let mut out = Vec::new();
            for entry in self.counts.range((list, "")..)? {
                let (key, count) = entry?;
                let (held, value) = key.value();
                if held != list {
                    break;
                }
                out.push((value.to_owned(), count.value()));
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
    /// Each record's type, and the chunk that holds it.
    pub docs: std::collections::BTreeMap<String, (String, ChunkNo)>,
    pub fields: std::collections::BTreeMap<(String, String), String>,
    pub lists: std::collections::BTreeMap<String, Vec<Entry>>,
    pub counts: std::collections::BTreeMap<(String, String), u64>,
    /// Each resource's newest version, as it reads.
    pub newest: std::collections::BTreeMap<String, String>,
}

impl Snapshot {
    /// Everything it holds: what tests compare an index kept change by change with one built
    /// from the record whole.
    pub fn contents(&self) -> Result<Contents, IndexError> {
        let mut out = Contents::default();
        for (doc, ty, chunk) in self.docs()? {
            out.docs.insert(doc, (ty, chunk));
        }
        for entry in self.fields.iter()? {
            let (key, value) = entry?;
            let (resource, field) = key.value();
            let shown = if field.starts_with('$') && !reflect::is_reflected(field) {
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
        for entry in self.newest.iter()? {
            let (resource, value) = entry?;
            out.newest.insert(
                resource.value().to_owned(),
                format!(
                    "{:?}",
                    reflect::Stamp::of_value(&codec::get_value(value.value())?)
                ),
            );
        }
        for entry in self.counts.iter()? {
            let (key, count) = entry?;
            let (list, value) = key.value();
            if count.value() > 0 {
                out.counts
                    .insert((list.to_owned(), value.to_owned()), count.value());
            }
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
    counts: Table<'t, (&'static str, &'static str), u64>,
    newest: Table<'t, &'static str, &'static [u8]>,
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
                counts: txn.open_table(COUNTS)?,
                newest: txn.open_table(NEWEST)?,
            },
            lists: ListEdits::default(),
        })
    }

    fn flush(&mut self) -> Result<(), IndexError> {
        let (dirs, pages) = std::mem::take(&mut self.lists).finish();
        for ((list, seq), entries) in pages {
            match entries {
                Some(page) => {
                    self.tables
                        .pages
                        .insert((list.as_str(), seq), page.sealed().as_slice())?;
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

    /// Stamp the index with the heads it reflects of each chunk, once this commit is in.
    pub fn set_heads(&mut self, heads: &Stamps) -> Result<(), IndexError> {
        let stale: Vec<String> = self
            .tables
            .meta
            .iter()?
            .filter_map(std::result::Result::ok)
            .map(|(key, _)| key.value().to_owned())
            .filter(|key| key.starts_with(META_HEADS))
            .filter(|key| heads.keys().all(|chunk| key.as_str() != heads_of(*chunk)))
            .collect();
        for key in stale {
            self.tables.meta.remove(key.as_str())?;
        }
        for (chunk, of_chunk) in heads {
            self.tables
                .meta
                .insert(heads_of(*chunk).as_str(), encode_heads(of_chunk).as_slice())?;
        }
        Ok(())
    }

    /// # Errors
    /// The index cannot be written.
    pub fn put_doc(&mut self, doc: &str, ty: &str, chunk: ChunkNo) -> Result<(), IndexError> {
        self.tables
            .docs
            .insert(doc, encode_doc(ty, chunk).as_slice())?;
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

    /// Keep `stamp`'s version as the newest version of `resource`, unless one it holds comes
    /// after it: what a mark of it, or of the document holding it, reads without the resource's
    /// log.
    pub fn put_newest(&mut self, resource: &str, stamp: &reflect::Stamp) -> Result<(), IndexError> {
        if let Some(held) = self.tables.newest.get(resource)? {
            let held = reflect::Stamp::of_value(&codec::get_value(held.value())?)
                .ok_or(IndexError::Corrupt)?;
            if !held.before(stamp) {
                return Ok(());
            }
        }
        self.tables
            .newest
            .insert(resource, codec::put_value(&stamp.value()).as_slice())?;
        Ok(())
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

    /// How many entries `list` holds, as this commit leaves it.
    pub fn list_len(&mut self, list: &str) -> Result<usize, IndexError> {
        Ok(self.lists.read(&self.tables, list)?.len())
    }

    /// # Errors
    /// The index cannot be read or is corrupt.
    pub fn list_replace(&mut self, list: &str, entries: Vec<Entry>) -> Result<(), IndexError> {
        self.lists.replace(&self.tables, list, entries)
    }

    /// Drop every list whose name starts with `prefix`, and its counts.
    pub fn drop_lists(&mut self, prefix: &str) -> Result<(), IndexError> {
        for name in self.lists_from(prefix)? {
            self.lists.drop_list(&self.tables, &name)?;
        }
        let mut counted = Vec::new();
        for entry in self.tables.counts.range((prefix, "")..)? {
            let (key, _) = entry?;
            let (list, value) = key.value();
            if !list.starts_with(prefix) {
                break;
            }
            counted.push((list.to_owned(), value.to_owned()));
        }
        for (list, value) in counted {
            self.tables.counts.remove((list.as_str(), value.as_str()))?;
        }
        Ok(())
    }

    /// Add `delta` to how many entries of `list` hold `value`.
    pub fn count(&mut self, list: &str, value: &str, delta: i64) -> Result<(), IndexError> {
        let held = self
            .tables
            .counts
            .get((list, value))?
            .map_or(0, |v| v.value());
        let now = held.saturating_add_signed(delta);
        if now == 0 {
            self.tables.counts.remove((list, value))?;
        } else {
            self.tables.counts.insert((list, value), now)?;
        }
        Ok(())
    }

    /// The names of the lists starting with `prefix`, as this commit leaves them.
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
}
