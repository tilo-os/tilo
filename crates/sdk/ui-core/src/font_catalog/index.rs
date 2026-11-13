//! The font database a launch builds, kept on disk so the next launch reads it instead of building it
//! again. Building it verifies the product catalog (hashing every font of a catalog not on an immutable
//! mount) and parses every face of the catalog and of the system's fonts: tens of milliseconds before a
//! process could set any text.

use cosmic_text::fontdb;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tilo_files::native::checked::Identity;

const VERSION: u32 = 3;

const MAX_INDEX_BYTES: u64 = 64 << 20;

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

/// Where this process keeps its index: the directory `TILO_FONT_INDEX` names — in a sandbox, one every
/// app of the build shares, so the second app opened on the device reads the database the first one's
/// launch recorded — else `tilo/font-index` under its cache directory.
#[cfg(not(test))]
pub(crate) fn location() -> Option<PathBuf> {
    let paths = &crate::host::environment().paths;
    let dir = match &paths.font_index {
        Some(dir) => dir.clone(),
        None => paths.cache_dir()?.join("tilo").join("font-index"),
    };
    Some(dir.join(format!("{}.json", tilo_font_catalog::manifest_sha256())))
}

/// This platform cannot identify files: no index is recorded or trusted.
struct Unidentifiable;

fn identify(path: &Path) -> Result<Option<Identity>, Unidentifiable> {
    match Identity::of_path(path) {
        Ok(identity) => Ok(identity),
        Err(error) if error.kind() == std::io::ErrorKind::Unsupported => Err(Unidentifiable),
        Err(_) => Ok(None),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Input {
    path: PathBuf,
    identity: Option<Identity>,
}

impl Input {
    fn now(path: PathBuf) -> Result<Self, Unidentifiable> {
        let identity = identify(&path)?;
        Ok(Self { path, identity })
    }

    fn holds(&self) -> bool {
        identify(&self.path).is_ok_and(|identity| identity == self.identity)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Face {
    file: usize,
    index: u32,
    families: Vec<String>,
    post_script_name: String,
    style: u8,
    weight: u16,
    /// fontdb's width, 1 (ultra-condensed) to 9 (ultra-expanded).
    stretch: u16,
    monospaced: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Index {
    version: u32,
    build: String,
    roots: Vec<PathBuf>,
    files: Vec<Input>,
    inputs: Vec<Input>,
    faces: Vec<Face>,
    generic: [String; 5],
}

const GENERIC: [fontdb::Family<'static>; 5] = [
    fontdb::Family::SansSerif,
    fontdb::Family::Serif,
    fontdb::Family::Monospace,
    fontdb::Family::Cursive,
    fontdb::Family::Fantasy,
];

fn existing_roots(roots: &[PathBuf], chosen: &Path) -> Vec<PathBuf> {
    let mut existing = Vec::new();
    for root in roots.iter().filter(|root| root.is_dir()) {
        let Ok(root) = root.canonicalize() else {
            continue;
        };
        let same = root == chosen;
        existing.push(root);
        if same {
            break;
        }
    }
    existing
}

/// The database recorded at `path`, when it is this build's and everything
/// it was built from is as it was: the catalog roots, every font file, every
/// directory holding one and the system's font directories. An input that
/// did not exist when the index was recorded is not one it named and cannot
/// invalidate it; one that exists now and was not recorded is a font (or a
/// font directory) the recorded database lacks.
pub(crate) fn load(path: &Path, roots: &[PathBuf]) -> Option<fontdb::Database> {
    let bytes = tilo_files::native::bounded::read(path, MAX_INDEX_BYTES).ok()??;
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
    let recorded: HashSet<&Path> = index
        .inputs
        .iter()
        .map(|known| known.path.as_path())
        .collect();
    let unrecorded = system_inputs()
        .into_iter()
        .any(|input| input.exists() && !recorded.contains(input.as_path()));
    if unrecorded {
        return None;
    }
    Some(database(&index))
}

fn database(index: &Index) -> fontdb::Database {
    let mut db = fontdb::Database::new();
    for face in &index.faces {
        let Some(file) = index.files.get(face.file) else {
            continue;
        };
        db.push_face_info(fontdb::FaceInfo {
            id: fontdb::ID::dummy(),
            source: fontdb::Source::File(file.path.clone()),
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
/// at `path`, replacing what was there atomically. A database with a face
/// that is not in a file is not recorded, nor one on a platform that cannot
/// identify files. Failing to write leaves the next launch to build it
/// again.
pub(crate) fn store(path: &Path, roots: &[PathBuf], root: &Path, db: &fontdb::Database) {
    let Some(index) = index_of(roots, root, db) else {
        return;
    };
    let Ok(bytes) = serde_json::to_vec(&index) else {
        return;
    };
    let dir = path.parent().unwrap_or(Path::new("."));
    if let Err(error) = tilo_files::native::atomic::write_creating_parent(path, &bytes) {
        // Where the index lives cannot be written: another launch of this
        // device records it instead, and until one does every launch builds
        // the database again.
        tracing::debug!(%error, dir = %dir.display(), "font index not recorded");
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
        let file = if let Some(file) = files.iter().position(|file| file.path == *path) {
            file
        } else {
            files.push(Input::now(path.clone()).ok()?);
            files.len() - 1
        };
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
    let directories: Vec<PathBuf> = files
        .iter()
        .filter_map(|file| file.path.parent())
        .map(Path::to_path_buf)
        .chain(system_inputs())
        .collect();
    for directory in directories {
        if inputs.iter().any(|known| known.path == directory) {
            continue;
        }
        let input = Input::now(directory).ok()?;
        if input.identity.is_some() {
            inputs.push(input);
        }
    }
    Some(Index {
        version: VERSION,
        build: tilo_font_catalog::manifest_sha256(),
        roots: existing_roots(roots, &root.canonicalize().ok()?),
        files,
        inputs,
        faces,
        generic: GENERIC.map(|family| db.family_name(&family).to_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Move `path`'s mtime a second on, as time passing between two writes
    /// does: on a filesystem whose timestamps are coarser than the time
    /// between the index being stored and the change, the change
    /// times can equal the ones recorded.
    fn a_second_later(path: &Path) {
        let file = std::fs::File::open(path).unwrap();
        let modified = file.metadata().unwrap().modified().unwrap();
        file.set_modified(modified + std::time::Duration::from_secs(1))
            .unwrap();
    }

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

        let bytes = std::fs::read(&first).unwrap();
        std::fs::write(&first, &bytes).unwrap();
        a_second_later(&first);
        assert!(load(&index_path, &roots).is_none(), "a rewritten font");
        store(&index_path, &roots, &root, &db);
        assert!(load(&index_path, &roots).is_some());

        std::fs::write(fonts.join("added.ttf"), &bytes).unwrap();
        a_second_later(&fonts);
        assert!(load(&index_path, &roots).is_none(), "an added font");
        store(&index_path, &roots, &root, &db);

        let front = dir.path().join("front");
        std::fs::create_dir_all(&front).unwrap();
        let in_front = vec![front, fonts.clone()];
        assert!(load(&index_path, &in_front).is_none(), "a root in front");

        let mut other: Index =
            serde_json::from_slice(&std::fs::read(&index_path).unwrap()).unwrap();
        other.build = "another".into();
        std::fs::write(&index_path, serde_json::to_vec(&other).unwrap()).unwrap();
        assert!(load(&index_path, &roots).is_none(), "another build");
    }

    #[test]
    fn a_catalog_reached_through_another_spelling_is_the_same_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let fonts = dir.path().join("fonts");
        std::fs::create_dir_all(&fonts).unwrap();
        let mut db = fontdb::Database::new();
        for (name, bytes) in fixture_files() {
            let path = fonts.join(name);
            std::fs::write(&path, bytes).unwrap();
            db.load_font_file(&path).unwrap();
        }
        let link = dir.path().join("fonts-link");
        std::os::unix::fs::symlink(&fonts, &link).unwrap();
        let index_path = dir.path().join("index.json");
        store(&index_path, std::slice::from_ref(&link), &link, &db);
        let recorded: Index = serde_json::from_slice(&std::fs::read(&index_path).unwrap()).unwrap();
        assert_eq!(recorded.roots, vec![fonts.canonicalize().unwrap()]);
        assert!(
            load(&index_path, std::slice::from_ref(&fonts)).is_some(),
            "the same directory by another spelling"
        );
    }
}
