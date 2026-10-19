//! The font database a launch builds, kept on disk so the next launch reads
//! it instead of building it again.
//!
//! Building the database verifies the product catalog (hashing every font
//! of a catalog that is not on an immutable mount) and parses every face of
//! the catalog and of the system's fonts: tens of milliseconds a process
//! spent before it could set any text, on every launch. The index records
//! the database that build produced, face by face (the file, its face index,
//! families, style, weight, width), with the identity of every file and
//! directory it was built from. A later process that finds every one of
//! them unchanged builds the same database from the index alone: it parses
//! no font and hashes nothing, and maps a font's file only when a face of it
//! is first shaped.
//!
//! Identity is the device, inode, size, modification and change times. A
//! catalog font whose identity is the one it had when it was hashed holds
//! the bytes that were hashed: rewriting a file, or changing its times,
//! moves its change time, which no unprivileged process can set. A font
//! added to or removed from a font directory moves that directory's times.
//! Anything that does not match, or an index from another build, is
//! ignored and the database is built as before; a process that may write
//! the index then records the new one.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cosmic_text::fontdb;
use serde::{Deserialize, Serialize};

/// Bumped whenever what an index records, or how a database is made from
/// it, changes.
const VERSION: u32 = 1;

/// The system font directories and configuration a build reads besides the
/// directories that hold a font: a font added to a new directory under one
/// of them, or a new configuration, moves one of these.
fn system_inputs() -> Vec<PathBuf> {
    let mut inputs: Vec<PathBuf> = [
        "/usr/share/fonts",
        "/usr/local/share/fonts",
        "/etc/fonts",
        "/etc/fonts/conf.d",
        "/etc/fonts/fonts.conf",
        "/etc/fonts/local.conf",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect();
    let paths = &crate::host::environment().paths;
    if let Some(config) = paths.config_dir() {
        inputs.push(config.join("fontconfig"));
        inputs.push(config.join("fontconfig/fonts.conf"));
    }
    if let Some(home) = &paths.home {
        inputs.push(home.join(".fonts"));
        inputs.push(home.join(".local/share/fonts"));
    }
    inputs
}

/// Where this process keeps its index: `TILO_FONT_INDEX` when the host names
/// one, else a file in the user's cache directory named for this build's
/// catalog.
#[cfg(not(test))]
pub(crate) fn location() -> Option<PathBuf> {
    let paths = &crate::host::environment().paths;
    if let Some(path) = &paths.font_index {
        return Some(path.clone());
    }
    Some(
        paths
            .cache_dir()?
            .join("tilo")
            .join("font-index")
            .join(format!("{}.json", tilo_font_catalog::manifest_sha256())),
    )
}

/// What identifies one file or directory's content and listing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Identity {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl Identity {
    /// `path`'s identity now, following links as fontdb does; `None` when
    /// it does not exist.
    fn of(path: &Path) -> Option<Self> {
        use std::os::unix::fs::MetadataExt as _;
        let meta = std::fs::metadata(path).ok()?;
        Some(Self {
            dev: meta.dev(),
            ino: meta.ino(),
            size: if meta.is_dir() { 0 } else { meta.size() },
            mtime: (meta.mtime(), meta.mtime_nsec()),
            ctime: (meta.ctime(), meta.ctime_nsec()),
        })
    }
}

/// One input of the build and its identity then (`None`: it did not exist).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Input {
    path: PathBuf,
    identity: Option<Identity>,
}

impl Input {
    fn now(path: PathBuf) -> Self {
        let identity = Identity::of(&path);
        Self { path, identity }
    }

    fn holds(&self) -> bool {
        Identity::of(&self.path) == self.identity
    }
}

/// One face of the database, as fontdb described it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Face {
    /// Index into [`Index::files`].
    file: usize,
    index: u32,
    families: Vec<String>,
    post_script_name: String,
    /// 0 normal, 1 italic, 2 oblique.
    style: u8,
    weight: u16,
    /// fontdb's width, 1 (ultra-condensed) to 9 (ultra-expanded).
    stretch: u16,
    monospaced: bool,
}

/// A database and what it was built from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Index {
    version: u32,
    /// The catalog manifest this build accepts.
    build: String,
    /// The catalog roots that existed, in order, up to and including the
    /// one the catalog loaded from.
    roots: Vec<PathBuf>,
    /// Every font file a face is in.
    files: Vec<Input>,
    /// Every directory holding one of them, and the system's font
    /// directories and configuration.
    inputs: Vec<Input>,
    faces: Vec<Face>,
    /// The generic families: sans-serif, serif, monospace, cursive, fantasy.
    generic: [String; 5],
}

const GENERIC: [fontdb::Family<'static>; 5] = [
    fontdb::Family::SansSerif,
    fontdb::Family::Serif,
    fontdb::Family::Monospace,
    fontdb::Family::Cursive,
    fontdb::Family::Fantasy,
];

/// The roots that exist, in order, up to and including `chosen`.
fn existing_roots(roots: &[PathBuf], chosen: &Path) -> Vec<PathBuf> {
    let mut existing = Vec::new();
    for root in roots.iter().filter(|root| root.is_dir()) {
        existing.push(root.clone());
        let same = root.canonicalize().is_ok_and(|root| root == chosen);
        if same {
            break;
        }
    }
    existing
}

/// The database recorded at `path`, when it is this build's and everything
/// it was built from is as it was: the catalog roots, every font file, every
/// directory holding one and the system's font directories.
pub(crate) fn load(path: &Path, roots: &[PathBuf]) -> Option<fontdb::Database> {
    let bytes = std::fs::read(path).ok()?;
    let index: Index = serde_json::from_slice(&bytes).ok()?;
    if index.version != VERSION || index.build != tilo_font_catalog::manifest_sha256() {
        return None;
    }
    let chosen = index.roots.last()?.canonicalize().ok()?;
    if existing_roots(roots, &chosen) != index.roots {
        return None;
    }
    if !index.inputs.iter().chain(&index.files).all(Input::holds) {
        return None;
    }
    let mut system_inputs = system_inputs();
    system_inputs.retain(|input| !index.inputs.iter().any(|known| known.path == *input));
    if !system_inputs.is_empty() {
        // Inputs this environment reads that the build did not record.
        return None;
    }
    Some(database(&index))
}

/// The database `index` records: its faces in order, each file mapped only
/// when one of its faces is first used.
fn database(index: &Index) -> fontdb::Database {
    let mut db = fontdb::Database::new();
    let data: Vec<Arc<dyn AsRef<[u8]> + Send + Sync>> = index
        .files
        .iter()
        .map(|file| Arc::new(LazyMap::new(file.path.clone())) as _)
        .collect();
    for face in &index.faces {
        let (Some(file), Some(data)) = (index.files.get(face.file), data.get(face.file)) else {
            continue;
        };
        db.push_face_info(fontdb::FaceInfo {
            id: fontdb::ID::dummy(),
            source: fontdb::Source::SharedFile(file.path.clone(), Arc::clone(data)),
            index: face.index,
            families: face
                .families
                .iter()
                .map(|name| (name.clone(), fontdb::Language::English_UnitedStates))
                .collect(),
            post_script_name: face.post_script_name.clone(),
            style: match face.style {
                1 => fontdb::Style::Italic,
                2 => fontdb::Style::Oblique,
                _ => fontdb::Style::Normal,
            },
            weight: fontdb::Weight(face.weight),
            stretch: stretch(face.stretch),
            monospaced: face.monospaced,
        });
    }
    let [sans, serif, mono, cursive, fantasy] = &index.generic;
    db.set_sans_serif_family(sans.clone());
    db.set_serif_family(serif.clone());
    db.set_monospace_family(mono.clone());
    db.set_cursive_family(cursive.clone());
    db.set_fantasy_family(fantasy.clone());
    db
}

fn stretch(number: u16) -> fontdb::Stretch {
    match number {
        1 => fontdb::Stretch::UltraCondensed,
        2 => fontdb::Stretch::ExtraCondensed,
        3 => fontdb::Stretch::Condensed,
        4 => fontdb::Stretch::SemiCondensed,
        6 => fontdb::Stretch::SemiExpanded,
        7 => fontdb::Stretch::Expanded,
        8 => fontdb::Stretch::ExtraExpanded,
        9 => fontdb::Stretch::UltraExpanded,
        _ => fontdb::Stretch::Normal,
    }
}

/// Record `db`, built from the catalog at `root` (canonical) and `roots`,
/// at `path`, replacing what was there in one rename. A database with a
/// face that is not in a file is not recorded. Failing to write leaves the
/// next launch to build it again.
pub(crate) fn store(path: &Path, roots: &[PathBuf], root: &Path, db: &fontdb::Database) {
    let Some(index) = index_of(roots, root, db) else {
        return;
    };
    let Ok(bytes) = serde_json::to_vec(&index) else {
        return;
    };
    let Some(directory) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(directory).is_err() {
        return;
    }
    let temporary = directory.join(format!(
        ".{}.{}",
        path.file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_default(),
        std::process::id()
    ));
    if std::fs::write(&temporary, bytes).is_err() || std::fs::rename(&temporary, path).is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
}

fn index_of(roots: &[PathBuf], root: &Path, db: &fontdb::Database) -> Option<Index> {
    let mut files: Vec<Input> = Vec::new();
    let mut faces = Vec::new();
    for face in db.faces() {
        let (fontdb::Source::File(path) | fontdb::Source::SharedFile(path, _)) = &face.source
        else {
            return None;
        };
        let file = files
            .iter()
            .position(|file| file.path == *path)
            .unwrap_or_else(|| {
                files.push(Input::now(path.clone()));
                files.len() - 1
            });
        faces.push(Face {
            file,
            index: face.index,
            families: face.families.iter().map(|(name, _)| name.clone()).collect(),
            post_script_name: face.post_script_name.clone(),
            style: match face.style {
                fontdb::Style::Normal => 0,
                fontdb::Style::Italic => 1,
                fontdb::Style::Oblique => 2,
            },
            weight: face.weight.0,
            stretch: face.stretch.to_number(),
            monospaced: face.monospaced,
        });
    }
    let mut inputs: Vec<Input> = Vec::new();
    for directory in files
        .iter()
        .filter_map(|file| file.path.parent())
        .map(Path::to_path_buf)
        .chain(system_inputs())
    {
        if !inputs.iter().any(|known| known.path == directory) {
            inputs.push(Input::now(directory));
        }
    }
    Some(Index {
        version: VERSION,
        build: tilo_font_catalog::manifest_sha256(),
        roots: existing_roots(roots, root),
        files,
        inputs,
        faces,
        generic: GENERIC.map(|family| db.family_name(&family).to_owned()),
    })
}

/// A font file's bytes, mapped the first time they are read: a database
/// made from an index holds hundreds of files, of which a launch shapes
/// with a handful.
struct LazyMap {
    path: PathBuf,
    map: std::sync::OnceLock<Mapping>,
}

impl LazyMap {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            map: std::sync::OnceLock::new(),
        }
    }
}

impl AsRef<[u8]> for LazyMap {
    fn as_ref(&self) -> &[u8] {
        self.map.get_or_init(|| Mapping::of(&self.path)).bytes()
    }
}

/// A read-only private mapping of a whole file, or nothing when it cannot
/// be mapped (a face of it then fails to load, as an unreadable file does).
struct Mapping {
    ptr: *mut libc::c_void,
    len: usize,
}

// SAFETY: the mapping is read-only and owned by this value until it drops;
// nothing writes through `ptr`.
unsafe impl Send for Mapping {}
// SAFETY: as above; shared reads of immutable memory.
unsafe impl Sync for Mapping {}

impl Mapping {
    fn of(path: &Path) -> Self {
        use std::os::fd::AsRawFd as _;
        let empty = Self {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        let Ok(file) = std::fs::File::open(path) else {
            return empty;
        };
        let Some(len) = file
            .metadata()
            .ok()
            .and_then(|meta| usize::try_from(meta.len()).ok())
            .filter(|len| *len > 0)
        else {
            return empty;
        };
        // SAFETY: a fresh read-only private mapping of a whole open file;
        // the descriptor may close once mapped.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                file.as_raw_fd(),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return empty;
        }
        Self { ptr, len }
    }

    fn bytes(&self) -> &[u8] {
        if self.ptr.is_null() {
            return &[];
        }
        // SAFETY: `ptr` maps `len` readable bytes for as long as `self`.
        unsafe { std::slice::from_raw_parts(self.ptr.cast::<u8>(), self.len) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // SAFETY: unmapping exactly the mapping this value owns.
            unsafe {
                libc::munmap(self.ptr, self.len);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two of the fixture catalog's font files, by name, with their bytes.
    fn fixture_files() -> Vec<(String, Vec<u8>)> {
        let system = crate::font_catalog::fixture_font_system();
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        for face in system.db().faces() {
            let (fontdb::Source::File(path) | fontdb::Source::SharedFile(path, _)) = &face.source
            else {
                continue;
            };
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if !files.iter().any(|(known, _)| *known == name) {
                files.push((name, std::fs::read(path).unwrap()));
            }
            if files.len() == 2 {
                break;
            }
        }
        files
    }

    /// A database made from an index is the one that was recorded, face by
    /// face, and reads the same bytes.
    #[test]
    fn a_recorded_database_reads_back_face_for_face() {
        let dir = tempfile::tempdir().unwrap();
        let fonts = dir.path().join("fonts");
        std::fs::create_dir_all(&fonts).unwrap();
        let mut db = fontdb::Database::new();
        for (name, bytes) in fixture_files() {
            let path = fonts.join(name);
            std::fs::write(&path, bytes).unwrap();
            db.load_font_file(&path).unwrap();
        }
        assert!(db.faces().count() > 0, "the fixture holds faces");
        db.set_sans_serif_family("Inter");
        let index_path = dir.path().join("index.json");
        let roots = vec![fonts.clone()];
        let root = fonts.canonicalize().unwrap();
        store(&index_path, &roots, &root, &db);
        let read = load(&index_path, &roots).expect("the index holds");
        let describe = |db: &fontdb::Database| {
            db.faces()
                .map(|face| {
                    (
                        face.families
                            .iter()
                            .map(|(name, _)| name.clone())
                            .collect::<Vec<_>>(),
                        face.post_script_name.clone(),
                        face.index,
                        face.style,
                        face.weight,
                        face.stretch,
                        face.monospaced,
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(describe(&read), describe(&db));
        assert_eq!(
            read.family_name(&fontdb::Family::SansSerif),
            db.family_name(&fontdb::Family::SansSerif)
        );
        let id = read.faces().next().unwrap().id;
        let bytes = read.with_face_data(id, |data, _| data.len()).unwrap();
        assert!(bytes > 0, "a face's file is read when it is used");
    }

    /// Anything the database was built from that changed, a font added to a
    /// font directory, or another root in front, makes the index not hold.
    #[test]
    fn an_index_does_not_hold_once_what_it_was_built_from_changes() {
        let dir = tempfile::tempdir().unwrap();
        let fonts = dir.path().join("fonts");
        std::fs::create_dir_all(&fonts).unwrap();
        let mut db = fontdb::Database::new();
        let mut first = None;
        for (name, bytes) in fixture_files() {
            let path = fonts.join(name);
            std::fs::write(&path, bytes).unwrap();
            db.load_font_file(&path).unwrap();
            first.get_or_insert(path);
        }
        let first = first.expect("a fixture font");
        let index_path = dir.path().join("index.json");
        let roots = vec![fonts.clone()];
        let root = fonts.canonicalize().unwrap();
        store(&index_path, &roots, &root, &db);
        assert!(load(&index_path, &roots).is_some());

        // A font rewritten in place.
        let bytes = std::fs::read(&first).unwrap();
        std::fs::write(&first, &bytes).unwrap();
        assert!(load(&index_path, &roots).is_none(), "a rewritten font");
        store(&index_path, &roots, &root, &db);
        assert!(load(&index_path, &roots).is_some());

        // A font added beside the others.
        std::fs::write(fonts.join("added.ttf"), &bytes).unwrap();
        assert!(load(&index_path, &roots).is_none(), "an added font");
        store(&index_path, &roots, &root, &db);

        // Another catalog root in front of the one it loaded from.
        let front = dir.path().join("front");
        std::fs::create_dir_all(&front).unwrap();
        let in_front = vec![front, fonts.clone()];
        assert!(load(&index_path, &in_front).is_none(), "a root in front");

        // Another build's index.
        let mut other: Index =
            serde_json::from_slice(&std::fs::read(&index_path).unwrap()).unwrap();
        other.build = "another".into();
        std::fs::write(&index_path, serde_json::to_vec(&other).unwrap()).unwrap();
        assert!(load(&index_path, &roots).is_none(), "another build");
    }
}
