//! The person's files, found by name and, once the content index has read
//! them, by what they hold ([`crate::files_index`]).
//!
//! Names come from a walk of the home folder on a thread of its own
//! ([`FileNames::scan`]), so opening search never waits for the disk; until
//! the walk is done, search finds what it has reached. A row opens the
//! file where the host opens files, as its `file://` address.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use ui::search::FieldWeight;
use ui::search::system::{MAX_PROVIDER_SCORE, ResourceKind, rank_fields};

use crate::files_index::FilesIndex;
use crate::model::{Hit, Source, Target};

/// The most names one walk keeps.
const MAX_NAMES: usize = 200_000;
/// The most rows one query offers.
const MAX_ROWS: usize = 64;
/// A file found by what it holds, not its name: still a match, below any
/// named one.
const CONTENT_SCORE: i32 = 10;

/// One name the walk found.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Name {
    path: PathBuf,
    folder: bool,
}

/// The names under the home folder, as far as the walk has come.
#[derive(Clone, Default)]
pub struct FileNames(Arc<RwLock<Vec<Name>>>);

impl FileNames {
    /// Walk `home` on a thread of its own, skipping what the content index
    /// skips (hidden folders, build output, caches).
    #[must_use]
    pub fn scan(home: PathBuf) -> Self {
        let names = Self::default();
        let filling = names.clone();
        let spawned = std::thread::Builder::new()
            .name("tilo-file-names".into())
            .spawn(move || {
                let mut batch = Vec::new();
                for entry in walkdir::WalkDir::new(&home)
                    .min_depth(1)
                    .max_depth(crate::files_index::MAX_DEPTH)
                    .follow_links(false)
                    .into_iter()
                    .filter_entry(|entry| !crate::files_index::should_skip(entry.path(), &home))
                    .filter_map(Result::ok)
                    .take(MAX_NAMES)
                {
                    batch.push(Name {
                        path: entry.path().to_path_buf(),
                        folder: entry.file_type().is_dir(),
                    });
                    if batch.len() >= 1024 {
                        filling.extend(std::mem::take(&mut batch));
                    }
                }
                filling.extend(batch);
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "the file names are not walked");
        }
        names
    }

    /// Names known up front: `(path, is a folder)`.
    #[must_use]
    pub fn from_paths(paths: impl IntoIterator<Item = (PathBuf, bool)>) -> Self {
        Self(Arc::new(RwLock::new(
            paths
                .into_iter()
                .map(|(path, folder)| Name { path, folder })
                .collect(),
        )))
    }

    fn extend(&self, names: Vec<Name>) {
        if let Ok(mut held) = self.0.write() {
            held.extend(names);
        }
    }
}

/// The `file://` address of `path`.
#[must_use]
pub fn file_uri(path: &Path) -> String {
    use std::fmt::Write as _;
    let mut uri = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                uri.push(char::from(byte));
            }
            other => {
                let _ = write!(uri, "%{other:02X}");
            }
        }
    }
    uri
}

pub struct Files {
    home: PathBuf,
    names: FileNames,
    content: Option<Arc<FilesIndex>>,
}

impl Files {
    /// Search `names` under `home`, and what the files hold through
    /// `content` when the host keeps that index.
    #[must_use]
    pub fn new(home: PathBuf, names: FileNames, content: Option<Arc<FilesIndex>>) -> Self {
        Self {
            home,
            names,
            content,
        }
    }

    fn hit(&self, path: &Path, folder: bool, score: i32) -> Hit {
        let title = path
            .file_name()
            .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy())
            .into_owned();
        let place = path
            .parent()
            .map(|parent| parent.strip_prefix(&self.home).unwrap_or(parent))
            .map(|parent| parent.to_string_lossy().into_owned())
            .filter(|place| !place.is_empty());
        Hit {
            kind: if folder {
                ResourceKind::Folder
            } else {
                ResourceKind::File
            },
            provider: self.id().into(),
            id: path.to_string_lossy().into_owned(),
            title,
            subtitle: place,
            facts: Vec::new(),
            score,
            icon: None,
            target: Target::Open {
                uri: file_uri(path),
                fallback: None,
            },
        }
    }
}

impl Source for Files {
    fn id(&self) -> &'static str {
        "files"
    }

    fn query(&self, needle: &str) -> Vec<Hit> {
        if needle.trim().is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<Hit> = match self.names.0.read() {
            Ok(names) => names
                .iter()
                .filter_map(|name| {
                    let file = name.path.file_name()?.to_string_lossy();
                    let score = rank_fields(needle, &[(&file, FieldWeight::TITLE)])?;
                    Some(self.hit(&name.path, name.folder, score))
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        hits.sort_by_key(|hit| std::cmp::Reverse(hit.score));
        hits.truncate(MAX_ROWS);
        if let Some(content) = &self.content {
            for found in content.query(needle, MAX_ROWS) {
                let id = found.path.to_string_lossy();
                if hits.iter().any(|hit| hit.id == id) {
                    continue;
                }
                hits.push(self.hit(&found.path, false, CONTENT_SCORE.min(MAX_PROVIDER_SCORE)));
            }
        }
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_found_by_name_and_opens_by_its_address() {
        let home = PathBuf::from("/home/mara");
        let names = FileNames::from_paths([
            (home.join("Documents/Budget 2026.ods"), false),
            (home.join("Documents"), true),
        ]);
        let files = Files::new(home, names, None);
        let hits = files.query("budget");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].subtitle.as_deref(), Some("Documents"));
        assert_eq!(
            hits[0].target,
            Target::Open {
                uri: "file:///home/mara/Documents/Budget%202026.ods".into(),
                fallback: None
            }
        );
        assert_eq!(files.query("documents")[0].kind, ResourceKind::Folder);
        assert!(files.query("").is_empty());
    }

    #[test]
    fn the_walk_finds_names_without_hidden_folders() {
        let home = tempfile::tempdir().expect("a home");
        std::fs::create_dir_all(home.path().join("Notes")).expect("a folder");
        std::fs::create_dir_all(home.path().join(".cache")).expect("a cache");
        std::fs::write(home.path().join("Notes/plan.txt"), "x").expect("a file");
        std::fs::write(home.path().join(".cache/plan-cache.txt"), "x").expect("a file");
        let names = FileNames::scan(home.path().to_path_buf());
        let files = Files::new(home.path().to_path_buf(), names, None);
        let mut found = Vec::new();
        for _ in 0..200 {
            found = files.query("plan");
            if !found.is_empty() {
                break;
            }
            std::thread::yield_now();
            std::thread::park_timeout(std::time::Duration::from_millis(10));
        }
        let titles: Vec<_> = found.iter().map(|hit| hit.title.as_str()).collect();
        assert_eq!(titles, ["plan.txt"]);
    }
}
