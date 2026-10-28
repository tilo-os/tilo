//! Explicitly granted legacy browser sources, never ambient profile discovery.
//!
//! `SQLite` is opened only on private, verified stable copies. Neither `SQLite` nor
//! this reader creates, checkpoints, chmods, or opens a source SHM file. Busy
//! profiles fail closed; the owner can retry after closing the legacy browser.

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;

use rusqlite::{Connection, OpenFlags};
use tilo_files::native::atomic::{Privacy, StorageDirectory};

use crate::imported_folders::{ImportedNode, ImportedTree, NodeKind};

use crate::imports::{
    BookmarkImport, HistoryImport, ImportBatch, ImportBundle, MAX_BUNDLE_BYTES, MAX_BUNDLE_ENTRIES,
};

use super::{CHROMIUM_EPOCH_MICROS, MAX_DATABASE_BYTES};

/// A directory handle already granted by the owner through the host's file
/// capability flow. Constructing this value does not itself request permission.
pub struct GrantedProfile {
    directory: StorageDirectory,
    owner: u32,
}

impl GrantedProfile {
    /// Retain precisely the granted directory; reject foreign or writable-by-
    /// others namespaces without changing their permissions.
    pub fn from_directory(file: File) -> io::Result<Self> {
        let owner = file.metadata()?.uid();
        let directory = StorageDirectory::from_file(file)?;
        directory.validate_namespace_control()?;
        Ok(Self { directory, owner })
    }

    fn read(&self, name: &str, limit: u64) -> io::Result<Capture> {
        self.directory.validate_namespace_control()?;
        let file = self
            .directory
            .open_read(OsStr::new(name), Privacy::Inherit)?;
        let before = Stamp::of(&file.metadata()?);
        if before.owner != self.owner || before.length > limit {
            return Err(invalid("source ownership or size is unacceptable"));
        }
        let mut bytes = Vec::new();
        file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err(invalid("source exceeds the read limit"));
        }
        let current = self
            .directory
            .open_read(OsStr::new(name), Privacy::Inherit)?;
        if before != Stamp::of(&current.metadata()?) || bytes.len() as u64 != before.length {
            return Err(busy());
        }
        Ok(Capture {
            stamp: before,
            bytes,
        })
    }

    fn optional_wal(&self, name: &str) -> io::Result<Option<Capture>> {
        match self.read(name, MAX_DATABASE_BYTES) {
            Ok(capture) => Ok(Some(capture)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    owner: u32,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl Stamp {
    fn of(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
            length: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}
#[derive(PartialEq, Eq)]
struct Capture {
    stamp: Stamp,
    bytes: Vec<u8>,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn busy() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "browser source changed; close it and retry",
    )
}
fn sqlite_error(_: rusqlite::Error) -> io::Error {
    invalid("browser database is unreadable or incompatible")
}

fn fingerprint(kind: &str, db: &[u8], wal: Option<&[u8]>) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo-browser-reviewed-source-v1");
    hash.update(kind.as_bytes());
    hash.update(&(db.len() as u64).to_le_bytes());
    hash.update(db);
    hash.update(&[u8::from(wal.is_some())]);
    if let Some(wal) = wal {
        hash.update(wal);
    }
    *hash.finalize().as_bytes()
}

fn valid_page(url: &str, title: &str) -> io::Result<bool> {
    let web = url::Url::parse(url).is_ok_and(|value| {
        matches!(value.scheme(), "http" | "https") && value.host_str().is_some()
    });
    if web && (url.len() > 8192 || title.len() > 4096) {
        return Err(invalid("web page metadata exceeds import bounds"));
    }
    Ok(web)
}

/// Parse Chromium's reviewed `Bookmarks` file, including nested folders.
/// Folder hierarchy has no native equivalent and is deliberately flattened.
///
/// # Errors
/// Rejects malformed, changed, oversized, or excessively deep source JSON.
pub fn parse_chromium_bookmarks(profile: &GrantedProfile) -> io::Result<ImportBatch> {
    let (batch, _) = read_chromium_bookmarks(profile)?;
    batch
        .validate()
        .map_err(|_| invalid("import exceeds single-batch bounds; use a complete import bundle"))?;
    Ok(batch)
}

/// Read the complete reviewed source as bounded, validated import chunks.
///
/// # Errors
/// Rejects unstable, invalid or oversized sources rather than truncating them.
pub fn parse_chromium_bookmarks_bundle(profile: &GrantedProfile) -> io::Result<ImportBundle> {
    let (batch, tree) = read_chromium_bookmarks(profile)?;
    let mut bundle = ImportBundle::from_batch(batch)
        .map_err(|_| invalid("import exceeds complete-bundle bounds"))?;
    bundle.imported_tree = Some(tree);
    bundle
        .validate()
        .map_err(|_| invalid("invalid bookmark hierarchy"))?;
    Ok(bundle)
}

fn read_chromium_bookmarks(profile: &GrantedProfile) -> io::Result<(ImportBatch, ImportedTree)> {
    let capture = profile.read("Bookmarks", MAX_BUNDLE_BYTES as u64)?;
    if capture != profile.read("Bookmarks", MAX_BUNDLE_BYTES as u64)? {
        return Err(busy());
    }
    let document: serde_json::Value =
        serde_json::from_slice(&capture.bytes).map_err(|_| invalid("invalid bookmarks JSON"))?;
    let roots = document
        .get("roots")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| invalid("missing bookmark roots"))?;
    let mut batch = ImportBatch {
        source: fingerprint("chromium-bookmarks", &capture.bytes, None),
        bookmarks: Vec::new(),
        history: Vec::new(),
    };
    let mut tree = ImportedTree {
        source: batch.source,
        nodes: Vec::new(),
    };
    let mut pending: Vec<_> = roots
        .iter()
        .enumerate()
        .rev()
        .map(|(position, (name, node))| (node, None, position, format!("root:{name}"), 0usize))
        .collect();
    let mut inspected = 0usize;
    while let Some((node, parent, position, fallback_id, depth)) = pending.pop() {
        inspected = inspected.saturating_add(1);
        if depth > 64 || inspected > MAX_BUNDLE_ENTRIES {
            return Err(invalid("too many bookmark nodes"));
        }
        let id = node
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map_or_else(|| fallback_id.clone(), |id| format!("chromium:{id}"));
        let id = if parent.is_none() {
            format!("{fallback_id}:{id}")
        } else {
            id
        };
        let title = node
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let kind = match node.get("type").and_then(serde_json::Value::as_str) {
            Some("url") => {
                let url = node
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| invalid("bookmark URL missing"))?;
                if !valid_page(url, title)? {
                    continue;
                }
                let url = crate::imports::normalized_bookmark_url(url)
                    .ok_or_else(|| invalid("invalid bookmark URL"))?;
                batch.bookmarks.push(BookmarkImport {
                    url: url.clone(),
                    title: title.into(),
                });
                NodeKind::Bookmark { url }
            }
            Some("folder") => {
                let children = node
                    .get("children")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| invalid("bookmark children missing"))?;
                pending.extend(children.iter().enumerate().rev().map(|(position, child)| {
                    (
                        child,
                        Some(id.clone()),
                        position,
                        format!("{fallback_id}/{position}"),
                        depth.saturating_add(1),
                    )
                }));
                NodeKind::Folder
            }
            _ => return Err(invalid("unsupported bookmark node")),
        };
        tree.nodes.push(ImportedNode {
            id,
            parent,
            position: u32::try_from(position).map_err(|_| invalid("too many bookmark nodes"))?,
            title: title.into(),
            kind,
        });
    }
    normalize_output(&mut batch)?;
    tree.validate()
        .map_err(|_| invalid("invalid bookmark hierarchy"))?;
    Ok((batch, tree))
}

fn snapshot(
    profile: &GrantedProfile,
    name: &str,
) -> io::Result<(tempfile::TempDir, Connection, [u8; 32])> {
    let wal_name = format!("{name}-wal");
    let first = profile.read(name, MAX_DATABASE_BYTES)?;
    let wal = profile.optional_wal(&wal_name)?;
    if first != profile.read(name, MAX_DATABASE_BYTES)? || wal != profile.optional_wal(&wal_name)? {
        return Err(busy());
    }
    let source = fingerprint(
        name,
        &first.bytes,
        wal.as_ref().map(|value| value.bytes.as_slice()),
    );
    let directory = tempfile::tempdir()?;
    let path = directory.path().join(name);
    private_write(&path, &first.bytes)?;
    if let Some(wal) = wal {
        private_write(&directory.path().join(wal_name), &wal.bytes)?;
    }
    let connection = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sqlite_error)?;
    connection
        .execute_batch("PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; BEGIN;")
        .map_err(sqlite_error)?;
    let check: String = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(sqlite_error)?;
    if check != "ok" {
        return Err(invalid("browser database integrity check failed"));
    }
    Ok((directory, connection, source))
}

fn private_write(path: &std::path::Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut file = tilo_files::native::private::create_new(path)?;
    file.write_all(bytes)
}

/// Read every HTTP(S) URL from an explicitly granted Chromium history source.
/// The single-batch API refuses a corpus larger than its 10,000-row bound. Chromium timestamps are microseconds since 1601.
///
/// # Errors
/// Rejects busy, corrupt, foreign, or unsupported sources without editing them.
pub fn parse_chromium_history(profile: &GrantedProfile) -> io::Result<ImportBatch> {
    let batch = read_chromium_history(profile)?;
    batch
        .validate()
        .map_err(|_| invalid("import exceeds single-batch bounds; use a complete import bundle"))?;
    Ok(batch)
}

/// Read the complete reviewed source as bounded, validated import chunks.
///
/// # Errors
/// Rejects unstable, invalid or oversized sources rather than truncating them.
pub fn parse_chromium_history_bundle(profile: &GrantedProfile) -> io::Result<ImportBundle> {
    ImportBundle::from_batch(read_chromium_history(profile)?)
        .map_err(|_| invalid("import exceeds complete-bundle bounds"))
}

fn read_chromium_history(profile: &GrantedProfile) -> io::Result<ImportBatch> {
    let (_directory, connection, source) = snapshot(profile, "History")?;
    let mut batch = ImportBatch {
        source,
        bookmarks: Vec::new(),
        history: Vec::new(),
    };
    read_history(
        &connection,
        "SELECT url, COALESCE(title,''), last_visit_time, visit_count FROM urls ORDER BY last_visit_time DESC, url",
        true,
        &mut batch.history,
    )?;
    normalize_output(&mut batch)?;
    Ok(batch)
}

/// Read bookmarks and history from an explicitly granted Firefox
/// `places.sqlite`, including committed WAL rows. Firefox times use Unix micros.
///
/// # Errors
/// Rejects busy, corrupt, foreign, or unsupported sources without editing them.
pub fn parse_firefox_places(profile: &GrantedProfile) -> io::Result<ImportBatch> {
    let (batch, _) = read_firefox_places(profile, false, true, true)?;
    batch
        .validate()
        .map_err(|_| invalid("import exceeds single-batch bounds; use a complete import bundle"))?;
    Ok(batch)
}

/// Read the complete reviewed source as bounded, validated import chunks.
///
/// # Errors
/// Rejects unstable, invalid or oversized sources rather than truncating them.
pub fn parse_firefox_places_bundle(profile: &GrantedProfile) -> io::Result<ImportBundle> {
    parse_firefox_places_selected_bundle(profile, true, true)
}

/// Read only the explicitly selected Firefox data domains from one stable
/// source snapshot. Bookmark hierarchy is included only with bookmarks.
///
/// # Errors
/// Rejects an empty selection, unstable, invalid, or oversized selected data.
pub fn parse_firefox_places_selected_bundle(
    profile: &GrantedProfile,
    bookmarks: bool,
    history: bool,
) -> io::Result<ImportBundle> {
    if !bookmarks && !history {
        return Err(invalid("no Firefox data selected"));
    }
    let (batch, tree) = read_firefox_places(profile, true, bookmarks, history)?;
    let mut bundle = ImportBundle::from_batch(batch)
        .map_err(|_| invalid("import exceeds complete-bundle bounds"))?;
    bundle.imported_tree = tree;
    bundle
        .validate()
        .map_err(|_| invalid("invalid bookmark hierarchy"))?;
    Ok(bundle)
}

fn read_firefox_places(
    profile: &GrantedProfile,
    hierarchy: bool,
    bookmarks: bool,
    history: bool,
) -> io::Result<(ImportBatch, Option<ImportedTree>)> {
    let (_directory, connection, source) = snapshot(profile, "places.sqlite")?;
    let mut batch = ImportBatch {
        source,
        bookmarks: Vec::new(),
        history: Vec::new(),
    };
    if bookmarks {
        read_firefox_bookmarks(&connection, &mut batch.bookmarks)?;
    }
    if history {
        read_history(
            &connection,
            "SELECT url, COALESCE(title,''), last_visit_date, visit_count FROM moz_places WHERE visit_count>0 ORDER BY last_visit_date DESC, url",
            false,
            &mut batch.history,
        )?;
    }
    normalize_output(&mut batch)?;
    let tree = if hierarchy && bookmarks {
        Some(read_firefox_tree(&connection, source)?)
    } else {
        None
    };
    Ok((batch, tree))
}

fn read_firefox_bookmarks(
    connection: &Connection,
    output: &mut Vec<BookmarkImport>,
) -> io::Result<()> {
    let mut statement = connection.prepare("SELECT p.url, COALESCE(b.title,p.title,'') FROM moz_bookmarks b JOIN moz_places p ON p.id=b.fk WHERE b.type=1 ORDER BY b.id").map_err(sqlite_error)?;
    let mut rows = statement.query([]).map_err(sqlite_error)?;
    let mut accepted_bytes = 0usize;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        let url: String = row.get(0).map_err(sqlite_error)?;
        let title: String = row.get(1).map_err(sqlite_error)?;
        if valid_page(&url, &title)? {
            accepted_bytes = accepted_bytes
                .saturating_add(url.len())
                .saturating_add(title.len());
            if accepted_bytes > MAX_BUNDLE_BYTES {
                return Err(invalid("bookmark metadata is too large"));
            }
            output.push(BookmarkImport { url, title });
        }
        if output.len() > MAX_BUNDLE_ENTRIES {
            return Err(invalid("too many bookmarks"));
        }
    }
    Ok(())
}

fn read_firefox_tree(connection: &Connection, source: [u8; 32]) -> io::Result<ImportedTree> {
    let mut statement = connection.prepare("SELECT b.id,b.parent,b.position,COALESCE(b.title,p.title,''),b.type,p.url FROM moz_bookmarks b LEFT JOIN moz_places p ON p.id=b.fk ORDER BY b.id").map_err(sqlite_error)?;
    let mut rows = statement.query([]).map_err(sqlite_error)?;
    let mut tree = ImportedTree {
        source,
        nodes: Vec::new(),
    };
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        if tree.nodes.len() >= MAX_BUNDLE_ENTRIES {
            return Err(invalid("too many bookmark nodes"));
        }
        let id: i64 = row.get(0).map_err(sqlite_error)?;
        let parent: i64 = row.get(1).map_err(sqlite_error)?;
        let position: i64 = row.get(2).map_err(sqlite_error)?;
        let title: String = row.get(3).map_err(sqlite_error)?;
        let kind: i64 = row.get(4).map_err(sqlite_error)?;
        let kind = match kind {
            1 => {
                let url: String = row.get(5).map_err(sqlite_error)?;
                if !valid_page(&url, &title)? {
                    continue;
                }
                NodeKind::Bookmark {
                    url: crate::imports::normalized_bookmark_url(&url)
                        .ok_or_else(|| invalid("invalid bookmark URL"))?,
                }
            }
            2 => NodeKind::Folder,
            3 => NodeKind::Separator,
            _ => return Err(invalid("unsupported Firefox bookmark node")),
        };
        if id <= 0 || parent < 0 {
            return Err(invalid("invalid Firefox bookmark identity"));
        }
        tree.nodes.push(ImportedNode {
            id: format!("firefox:{id}"),
            parent: (parent != 0).then(|| format!("firefox:{parent}")),
            position: u32::try_from(position).map_err(|_| invalid("invalid bookmark order"))?,
            title,
            kind,
        });
    }
    tree.validate()
        .map_err(|_| invalid("invalid Firefox bookmark hierarchy"))?;
    Ok(tree)
}

fn read_history(
    connection: &Connection,
    query: &str,
    chromium: bool,
    output: &mut Vec<HistoryImport>,
) -> io::Result<()> {
    let mut statement = connection.prepare(query).map_err(sqlite_error)?;
    let mut rows = statement.query([]).map_err(sqlite_error)?;
    let mut accepted_bytes = 0usize;
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        let url: String = row.get(0).map_err(sqlite_error)?;
        let title: String = row.get(1).map_err(sqlite_error)?;
        let micros: i64 = row.get(2).map_err(sqlite_error)?;
        let count: i64 = row.get(3).map_err(sqlite_error)?;
        let Ok(micros) = u64::try_from(micros) else {
            continue;
        };
        let micros = if chromium {
            let Some(value) = micros.checked_sub(CHROMIUM_EPOCH_MICROS) else {
                continue;
            };
            value
        } else {
            micros
        };
        if valid_page(&url, &title)? && count > 0 {
            accepted_bytes = accepted_bytes
                .saturating_add(url.len())
                .saturating_add(title.len());
            if accepted_bytes > MAX_BUNDLE_BYTES {
                return Err(invalid("history metadata is too large"));
            }
            if output.len() == MAX_BUNDLE_ENTRIES {
                return Err(invalid("too many history records"));
            }
            output.push(HistoryImport {
                url,
                title,
                last_visit: micros / 1_000_000,
                visit_count: u32::try_from(count)
                    .map_err(|_| invalid("visit count exceeds native bounds"))?,
            });
        }
    }
    Ok(())
}

fn raw_bounds(batch: &ImportBatch) -> io::Result<()> {
    if batch.bookmarks.len().saturating_add(batch.history.len()) > MAX_BUNDLE_ENTRIES {
        return Err(invalid("too many browser import records"));
    }
    let bytes: usize = batch
        .bookmarks
        .iter()
        .map(|row| row.url.len().saturating_add(row.title.len()))
        .chain(
            batch
                .history
                .iter()
                .map(|row| row.url.len().saturating_add(row.title.len())),
        )
        .sum();
    if bytes > MAX_BUNDLE_BYTES {
        return Err(invalid("browser import metadata is too large"));
    }
    Ok(())
}

fn normalize_output(batch: &mut ImportBatch) -> io::Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    batch.bookmarks.retain_mut(|row| {
        let Some(url) = crate::imports::normalized_bookmark_url(&row.url) else {
            return false;
        };
        row.url = url;
        seen.insert(row.url.clone())
    });
    let mut history = std::collections::BTreeMap::<String, HistoryImport>::new();
    for mut row in batch.history.drain(..) {
        let Some(url) = crate::imports::normalized_bookmark_url(&row.url) else {
            continue;
        };
        row.url = url;
        if let Some(prior) = history.get_mut(&row.url) {
            prior.visit_count = prior
                .visit_count
                .checked_add(row.visit_count)
                .ok_or_else(|| invalid("combined visit count exceeds native bounds"))?;
            if row.last_visit > prior.last_visit {
                prior.last_visit = row.last_visit;
                if !row.title.is_empty() {
                    prior.title = row.title;
                }
            }
        } else {
            history.insert(row.url.clone(), row);
        }
    }
    batch.history = history.into_values().collect();
    raw_bounds(batch)
}
