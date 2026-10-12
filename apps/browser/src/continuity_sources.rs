//! Explicitly granted legacy browser sources, never ambient profile discovery.
//!
//! `SQLite` is opened only on private, verified stable copies. Neither `SQLite` nor
//! this reader creates, checkpoints, chmods, or opens a source SHM file. Busy
//! profiles fail closed; the owner can retry after closing the legacy browser.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;

use rusqlite::{Connection, OpenFlags};
use tilo_files::native::atomic::{Privacy, StorageDirectory};

use crate::imported_folders::{ImportedNode, ImportedTree, NodeKind};

use crate::continuity::{
    BookmarkImport, HistoryImport, ImportBatch, ImportBundle, MAX_BUNDLE_BYTES, MAX_BUNDLE_ENTRIES,
};

const MAX_DATABASE_BYTES: u64 = 64 * 1024 * 1024;
const CHROMIUM_EPOCH_MICROS: u64 = 11_644_473_600_000_000;

/// A directory handle already granted by the owner through the host's file
/// capability flow. Constructing this value does not itself request permission.
pub struct GrantedProfile {
    directory: StorageDirectory,
    owner: u32,
}

impl GrantedProfile {
    /// Retain precisely the granted directory; reject foreign or writable-by-
    /// others namespaces without changing their permissions.
    ///
    /// # Errors
    /// Returns an error if the handle is not an owner-controlled directory.
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
        file.take(limit + 1).read_to_end(&mut bytes)?;
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
        .map(|(position, (name, node))| {
            (node, None, position as u32, format!("root:{name}"), 0usize)
        })
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
                let url = crate::actions::normalized_bookmark_url(url)
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
                        position as u32,
                        format!("{fallback_id}/{position}"),
                        depth + 1,
                    )
                }));
                NodeKind::Folder
            }
            _ => return Err(invalid("unsupported bookmark node")),
        };
        tree.nodes.push(ImportedNode {
            id,
            parent,
            position,
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
    let mut file = private_fs::create_new(path)?;
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
            accepted_bytes += url.len() + title.len();
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
                    url: crate::actions::normalized_bookmark_url(&url)
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
            accepted_bytes += url.len() + title.len();
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
    if batch.bookmarks.len() + batch.history.len() > MAX_BUNDLE_ENTRIES {
        return Err(invalid("too many browser import records"));
    }
    let bytes: usize = batch
        .bookmarks
        .iter()
        .map(|row| row.url.len() + row.title.len())
        .chain(
            batch
                .history
                .iter()
                .map(|row| row.url.len() + row.title.len()),
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
        let Some(url) = crate::actions::normalized_bookmark_url(&row.url) else {
            return false;
        };
        row.url = url;
        seen.insert(row.url.clone())
    });
    let mut history = std::collections::BTreeMap::<String, HistoryImport>::new();
    for mut row in batch.history.drain(..) {
        let Some(url) = crate::actions::normalized_bookmark_url(&row.url) else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn granted(directory: &tempfile::TempDir) -> GrantedProfile {
        GrantedProfile::from_directory(File::open(directory.path()).unwrap()).unwrap()
    }

    fn assert_native_import(batch: &ImportBatch) {
        use tilo_actions::Action;
        let mut library = tilo_actions::Doc::new();
        let mut personal = ui::tilo_resource::Replica::<crate::history::BrowsingHistory>::new();
        batch.bookmark_action().apply(&mut library).unwrap();
        personal.apply(batch.history_action().unwrap()).unwrap();
        batch.bookmark_action().apply(&mut library).unwrap();
        personal.apply(batch.history_action().unwrap()).unwrap();
        if !batch.bookmarks.is_empty() {
            let projected = crate::continuity_bookmarks::bookmarks_from_doc(&library).unwrap();
            assert_eq!(projected.entries().len(), batch.bookmarks.len());
        }
        let projected = personal.state().unwrap().entries();
        assert_eq!(projected.len(), batch.history.len());
        for row in &batch.history {
            let actual = projected
                .iter()
                .find(|actual| actual.url == row.url)
                .unwrap();
            assert_eq!(actual.visit_count, row.visit_count);
            assert_eq!(actual.last_visit, row.last_visit);
            assert_eq!(actual.title, row.title);
        }
    }

    fn source_bytes(directory: &tempfile::TempDir, name: &str) -> Vec<(String, Vec<u8>, u32)> {
        [
            name.to_string(),
            format!("{name}-wal"),
            format!("{name}-shm"),
        ]
        .into_iter()
        .filter_map(|name| {
            let path = directory.path().join(&name);
            let bytes = std::fs::read(&path).ok()?;
            Some((
                name,
                bytes,
                std::fs::metadata(path).unwrap().permissions().mode(),
            ))
        })
        .collect()
    }

    #[test]
    fn chromium_bookmarks_use_the_granted_file_and_skip_non_web_pages() {
        let source = tempfile::tempdir().unwrap();
        let json = br#"{"roots":{"bookmark_bar":{"type":"folder","children":[{"type":"folder","children":[{"type":"url","url":"https://example.org/","name":"Example"}]},{"type":"url","url":"https://EXAMPLE.org","name":"Duplicate"},{"type":"url","url":"file:///secret","name":"Local"}]}}}"#;
        std::fs::write(source.path().join("Bookmarks"), json).unwrap();
        let before = source_bytes(&source, "Bookmarks");
        let batch = parse_chromium_bookmarks(&granted(&source)).unwrap();
        assert_native_import(&batch);
        assert_eq!(
            batch.bookmarks,
            vec![BookmarkImport {
                url: "https://example.org/".into(),
                title: "Example".into()
            }]
        );
        assert_eq!(before, source_bytes(&source, "Bookmarks"));
        assert_eq!(
            batch.source,
            parse_chromium_bookmarks(&granted(&source)).unwrap().source
        );
    }

    #[test]
    fn chromium_complete_bundle_preserves_roots_nested_order_and_repeated_url_placements() {
        use tilo_actions::Action;
        let source = tempfile::tempdir().unwrap();
        let json = br#"{"roots":{"bookmark_bar":{"id":"1","name":"Bookmarks bar","type":"folder","children":[{"id":"2","name":"Work","type":"folder","children":[{"id":"3","type":"url","url":"https://example.org/","name":"Work example"}]},{"id":"4","name":"Personal","type":"folder","children":[{"id":"5","type":"url","url":"https://example.org/","name":"Personal example"}]}]},"other":{"id":"6","name":"Other bookmarks","type":"folder","children":[]}}}"#;
        std::fs::write(source.path().join("Bookmarks"), json).unwrap();
        let before = source_bytes(&source, "Bookmarks");
        let bundle = parse_chromium_bookmarks_bundle(&granted(&source)).unwrap();
        let tree = bundle.imported_tree.as_ref().unwrap();
        assert_eq!(tree.nodes.len(), 6);
        assert_eq!(
            tree.nodes
                .iter()
                .filter(|n| matches!(n.kind, NodeKind::Bookmark { .. }))
                .count(),
            2
        );
        let work = tree.nodes.iter().find(|n| n.id == "chromium:2").unwrap();
        let personal = tree.nodes.iter().find(|n| n.id == "chromium:4").unwrap();
        assert_eq!(work.parent.as_deref(), Some("root:bookmark_bar:chromium:1"));
        assert_eq!((work.position, personal.position), (0, 1));
        assert_eq!(
            tree.nodes
                .iter()
                .find(|n| n.id == "chromium:5")
                .unwrap()
                .title,
            "Personal example"
        );
        let encoded = serde_json::to_string(&bundle).unwrap();
        assert_eq!(ImportBundle::decode(&encoded).unwrap(), bundle);
        let mut doc = tilo_actions::Doc::new();
        bundle.bookmark_action().apply(&mut doc).unwrap();
        assert_eq!(
            crate::imported_folders::from_doc(&doc).unwrap(),
            vec![tree.clone()]
        );
        assert_eq!(before, source_bytes(&source, "Bookmarks"));
        assert_eq!(
            parse_chromium_bookmarks_bundle(&granted(&source)).unwrap(),
            bundle
        );
    }

    #[test]
    fn firefox_wal_complete_tree_preserves_folder_parents_position_and_duplicate_placements() {
        use tilo_actions::Action;
        let source = tempfile::tempdir().unwrap();
        let writer = Connection::open(source.path().join("places.sqlite")).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE moz_places(id INTEGER PRIMARY KEY,url TEXT,title TEXT,last_visit_date INTEGER,visit_count INTEGER); CREATE TABLE moz_bookmarks(id INTEGER PRIMARY KEY,fk INTEGER,title TEXT,type INTEGER,parent INTEGER,position INTEGER); INSERT INTO moz_places VALUES(1,'https://example.org/','Web title',9000000,3); INSERT INTO moz_bookmarks VALUES(1,NULL,'Root',2,0,0),(2,NULL,'Toolbar',2,1,0),(3,NULL,'Nested',2,2,0),(4,1,'Nested example',1,3,0),(5,1,'Toolbar example',1,2,1),(6,NULL,'',3,2,2);").unwrap();
        let before = source_bytes(&source, "places.sqlite");
        assert!(before.iter().any(|row| row.0 == "places.sqlite-wal"));
        let bundle = parse_firefox_places_bundle(&granted(&source)).unwrap();
        let tree = bundle.imported_tree.as_ref().unwrap();
        assert_eq!(tree.nodes.len(), 6);
        let nested = tree.nodes.iter().find(|n| n.id == "firefox:4").unwrap();
        let toolbar = tree.nodes.iter().find(|n| n.id == "firefox:5").unwrap();
        assert_eq!(nested.parent.as_deref(), Some("firefox:3"));
        assert_eq!(toolbar.parent.as_deref(), Some("firefox:2"));
        assert_eq!(toolbar.position, 1);
        assert_eq!(nested.kind, toolbar.kind);
        assert_eq!(nested.title, "Nested example");
        assert!(matches!(
            tree.nodes
                .iter()
                .find(|n| n.id == "firefox:6")
                .unwrap()
                .kind,
            NodeKind::Separator
        ));
        let mut doc = tilo_actions::Doc::new();
        bundle.bookmark_action().apply(&mut doc).unwrap();
        assert_eq!(
            crate::imported_folders::from_doc(&doc).unwrap(),
            vec![tree.clone()]
        );
        assert!(
            ui::tilo_resource::Replica::<crate::history::BrowsingHistory>::new()
                .apply(bundle.history_action().unwrap())
                .is_ok()
        );
        assert_eq!(before, source_bytes(&source, "places.sqlite"));
        assert_eq!(
            parse_firefox_places_bundle(&granted(&source)).unwrap(),
            bundle
        );
    }

    #[test]
    fn chromium_wal_history_is_read_consistently_without_source_changes() {
        let source = tempfile::tempdir().unwrap();
        let writer = Connection::open(source.path().join("History")).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE urls(url TEXT,title TEXT,last_visit_time INTEGER,visit_count INTEGER);").unwrap();
        writer
            .execute(
                "INSERT INTO urls VALUES (?1,?2,?3,?4)",
                (
                    "https://example.org/",
                    "Committed WAL page",
                    (CHROMIUM_EPOCH_MICROS + 42_000_000) as i64,
                    7,
                ),
            )
            .unwrap();
        let before = source_bytes(&source, "History");
        assert!(before.iter().any(|row| row.0 == "History-wal"));
        let batch = parse_chromium_history(&granted(&source)).unwrap();
        assert_native_import(&batch);
        assert_eq!(
            batch.history,
            vec![HistoryImport {
                url: "https://example.org/".into(),
                title: "Committed WAL page".into(),
                last_visit: 42,
                visit_count: 7
            }]
        );
        assert_eq!(before, source_bytes(&source, "History"));
        assert_eq!(
            batch.source,
            parse_chromium_history(&granted(&source)).unwrap().source
        );
    }

    #[test]
    fn firefox_wal_bookmarks_and_history_preserve_actual_counts_and_times() {
        let source = tempfile::tempdir().unwrap();
        let writer = Connection::open(source.path().join("places.sqlite")).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE moz_places(id INTEGER PRIMARY KEY,url TEXT,title TEXT,last_visit_date INTEGER,visit_count INTEGER); CREATE TABLE moz_bookmarks(id INTEGER PRIMARY KEY,fk INTEGER,title TEXT,type INTEGER);").unwrap();
        writer.execute_batch("INSERT INTO moz_places VALUES(1,'https://example.org/','History title',9000000,3); INSERT INTO moz_bookmarks VALUES(1,1,'Bookmark title',1); INSERT INTO moz_places VALUES(2,'https://EXAMPLE.org','New history title',10000000,2); INSERT INTO moz_bookmarks VALUES(2,2,'Duplicate bookmark',1);").unwrap();
        let before = source_bytes(&source, "places.sqlite");
        let batch = parse_firefox_places(&granted(&source)).unwrap();
        assert_native_import(&batch);
        assert_eq!(
            batch.bookmarks,
            vec![BookmarkImport {
                url: "https://example.org/".into(),
                title: "Bookmark title".into()
            }]
        );
        assert_eq!(
            batch.history,
            vec![HistoryImport {
                url: "https://example.org/".into(),
                title: "New history title".into(),
                last_visit: 10,
                visit_count: 5
            }]
        );
        assert_eq!(before, source_bytes(&source, "places.sqlite"));
    }

    #[test]
    fn firefox_bookmark_selection_never_queries_missing_history_metadata() {
        use tilo_actions::Action;
        use ui::tilo_resource::Resource as _;
        let source = tempfile::tempdir().unwrap();
        let writer = Connection::open(source.path().join("places.sqlite")).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE moz_places(id INTEGER PRIMARY KEY,url TEXT,title TEXT); CREATE TABLE moz_bookmarks(id INTEGER PRIMARY KEY,parent INTEGER,position INTEGER,fk INTEGER,title TEXT,type INTEGER); INSERT INTO moz_places VALUES(1,'https://example.org/','Page title'); INSERT INTO moz_bookmarks VALUES(1,0,0,NULL,'Research',2); INSERT INTO moz_bookmarks VALUES(2,1,0,1,'My bookmark',1);").unwrap();
        let before = source_bytes(&source, "places.sqlite");
        let bundle = parse_firefox_places_selected_bundle(&granted(&source), true, false).unwrap();
        assert!(bundle.chunks.iter().all(|chunk| chunk.history.is_empty()));
        let mut personal = tilo_actions::Doc::new();
        crate::personal_bookmarks::PersonalBookmarkAction::Import { bundle }
            .apply(&mut personal)
            .unwrap();
        let state = crate::personal_bookmarks::PersonalBookmarks::read(&personal).unwrap();
        assert_eq!(state.bookmarks.entries().len(), 1);
        assert_eq!(state.imported_trees.first().unwrap().nodes.len(), 2);
        assert!(parse_firefox_places_selected_bundle(&granted(&source), false, true).is_err());
        assert!(parse_firefox_places_bundle(&granted(&source)).is_err());
        assert_eq!(before, source_bytes(&source, "places.sqlite"));
    }

    #[test]
    fn firefox_history_selection_never_queries_missing_bookmark_tables() {
        let source = tempfile::tempdir().unwrap();
        let writer = Connection::open(source.path().join("places.sqlite")).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE moz_places(id INTEGER PRIMARY KEY,url TEXT,title TEXT,last_visit_date INTEGER,visit_count INTEGER); INSERT INTO moz_places VALUES(1,'https://example.org/','A visited page',19000000,4);").unwrap();
        let before = source_bytes(&source, "places.sqlite");
        let bundle = parse_firefox_places_selected_bundle(&granted(&source), false, true).unwrap();
        assert!(bundle.imported_tree.is_none());
        assert!(bundle.chunks.iter().all(|chunk| chunk.bookmarks.is_empty()));
        let mut personal = ui::tilo_resource::Replica::<crate::history::BrowsingHistory>::new();
        personal.apply(bundle.history_action().unwrap()).unwrap();
        let rows = personal.state().unwrap().entries();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows.first().unwrap().last_visit, 19);
        assert_eq!(rows.first().unwrap().visit_count, 4);
        assert!(parse_firefox_places_selected_bundle(&granted(&source), true, false).is_err());
        assert!(parse_firefox_places_selected_bundle(&granted(&source), false, false).is_err());
        assert!(parse_firefox_places_bundle(&granted(&source)).is_err());
        assert_eq!(before, source_bytes(&source, "places.sqlite"));
    }

    #[test]
    fn complete_history_bundle_preserves_every_row_beyond_single_batch_limit() {
        let source = tempfile::tempdir().unwrap();
        let mut writer = Connection::open(source.path().join("History")).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE urls(url TEXT,title TEXT,last_visit_time INTEGER,visit_count INTEGER);").unwrap();
        let transaction = writer.transaction().unwrap();
        {
            let mut insert = transaction
                .prepare("INSERT INTO urls VALUES (?1,?2,?3,?4)")
                .unwrap();
            for index in 0..10_001u64 {
                insert
                    .execute((
                        format!("https://example.org/page/{index}"),
                        format!("Page {index}"),
                        (CHROMIUM_EPOCH_MICROS + index * 1_000_000) as i64,
                        3,
                    ))
                    .unwrap();
            }
        }
        transaction.commit().unwrap();
        let before = source_bytes(&source, "History");
        assert!(parse_chromium_history(&granted(&source)).is_err());
        let bundle = parse_chromium_history_bundle(&granted(&source)).unwrap();
        assert_eq!(
            bundle
                .chunks
                .iter()
                .map(|chunk| chunk.history.len())
                .sum::<usize>(),
            10_001
        );
        assert!(
            bundle
                .chunks
                .iter()
                .all(|chunk| chunk.history.len() <= 1000)
        );
        bundle.validate().unwrap();
        assert_eq!(
            bundle,
            parse_chromium_history_bundle(&granted(&source)).unwrap()
        );
        let mut personal = ui::tilo_resource::Replica::<crate::history::BrowsingHistory>::new();
        personal.apply(bundle.history_action().unwrap()).unwrap();
        assert_eq!(personal.state().unwrap().imported.len(), 10_001);
        personal.apply(bundle.history_action().unwrap()).unwrap();
        assert_eq!(personal.state().unwrap().imported.len(), 10_001);
        assert_eq!(before, source_bytes(&source, "History"));
    }

    #[test]
    fn links_corruption_and_oversized_sources_are_refused() {
        let source = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("Bookmarks"), b"{}").unwrap();
        symlink(
            elsewhere.path().join("Bookmarks"),
            source.path().join("Bookmarks"),
        )
        .unwrap();
        assert!(parse_chromium_bookmarks(&granted(&source)).is_err());
        std::fs::write(source.path().join("History"), b"not a database").unwrap();
        assert!(parse_chromium_history(&granted(&source)).is_err());
        let oversized = File::create(source.path().join("places.sqlite")).unwrap();
        oversized.set_len(MAX_DATABASE_BYTES + 1).unwrap();
        assert!(parse_firefox_places(&granted(&source)).is_err());
        assert_eq!(
            std::fs::read(elsewhere.path().join("Bookmarks")).unwrap(),
            b"{}"
        );
    }
}
