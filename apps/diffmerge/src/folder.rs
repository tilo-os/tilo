//! Comparing two folder trees.

use std::collections::BTreeMap;
use std::path::Path;

use tilo_files::{Kind, VPath};
use ui::tilo_resource::AbsolutePath;

pub const MAX_ENTRIES: usize = 5_000;
const MAX_VISITED: usize = 50_000;
const MAX_DEPTH: usize = 24;
const MAX_COMPARE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderKind {
    /// Only on the left (the other side has no such file).
    Added,
    /// Only on the right.
    Removed,
    Modified,
}

impl FolderKind {
    pub fn tag(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Modified => "modified",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderEntry {
    pub path: String,
    pub kind: FolderKind,
    pub left_len: u64,
    pub right_len: u64,
    /// The files were too large to compare by content, so only their
    /// lengths were compared.
    pub by_length: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderComparison {
    pub left_label: String,
    pub left_root: String,
    pub right_label: String,
    pub right_root: String,
    pub entries: Vec<FolderEntry>,
    pub truncated: bool,
}

impl FolderComparison {
    pub fn counts(&self) -> (usize, usize, usize) {
        let count = |kind: FolderKind| self.entries.iter().filter(|e| e.kind == kind).count();
        (
            count(FolderKind::Added),
            count(FolderKind::Removed),
            count(FolderKind::Modified),
        )
    }

    pub fn paths(&self, entry: &FolderEntry) -> (Option<AbsolutePath>, Option<AbsolutePath>) {
        let left = (entry.kind != FolderKind::Removed)
            .then(|| join(&self.left_root, &entry.path))
            .flatten();
        let right = (entry.kind != FolderKind::Added)
            .then(|| join(&self.right_root, &entry.path))
            .flatten();
        (left, right)
    }
}

fn join(root: &str, relative: &str) -> Option<AbsolutePath> {
    AbsolutePath::new(Path::new(root).join(relative)).ok()
}

pub fn root_label(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

pub fn compare(left: &AbsolutePath, right: &AbsolutePath) -> Result<FolderComparison, String> {
    let (left_tree, left_truncated) = walk(left)?;
    let (right_tree, right_truncated) = walk(right)?;
    let same_content = |relative: &str| -> bool {
        match (left_tree.get(relative), right_tree.get(relative)) {
            (Some(&left_len), Some(&right_len))
                if left_len == right_len && left_len <= MAX_COMPARE_BYTES =>
            {
                files_equal(&left.join(relative), &right.join(relative))
            }
            _ => false,
        }
    };
    let entries = differences(&left_tree, &right_tree, same_content);
    let truncated = left_truncated || right_truncated || entries.len() >= MAX_ENTRIES;
    Ok(FolderComparison {
        left_label: root_label(left),
        left_root: left.to_string_lossy(),
        right_label: root_label(right),
        right_root: right.to_string_lossy(),
        entries,
        truncated,
    })
}

/// The entries that differ between two listings of `relative path → bytes`,
/// where `same_content` answers whether a file of equal length on both sides
/// really holds the same bytes. Paths only one side has are added or
/// removed; paths both sides have differ unless `same_content` says no.
pub fn differences(
    left: &BTreeMap<String, u64>,
    right: &BTreeMap<String, u64>,
    same_content: impl Fn(&str) -> bool,
) -> Vec<FolderEntry> {
    let mut entries = Vec::new();
    for (path, &left_len) in left {
        let Some(&right_len) = right.get(path) else {
            entries.push(FolderEntry {
                path: path.clone(),
                kind: FolderKind::Added,
                left_len,
                right_len: 0,
                by_length: false,
            });
            continue;
        };
        if left_len == right_len && same_content(path) {
            continue;
        }
        let by_length = left_len != right_len || left_len > MAX_COMPARE_BYTES;
        entries.push(FolderEntry {
            path: path.clone(),
            kind: FolderKind::Modified,
            left_len,
            right_len,
            by_length,
        });
    }
    for (path, &right_len) in right {
        if !left.contains_key(path) {
            entries.push(FolderEntry {
                path: path.clone(),
                kind: FolderKind::Removed,
                left_len: 0,
                right_len,
                by_length: false,
            });
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries.truncate(MAX_ENTRIES);
    entries
}

fn walk(root: &Path) -> Result<(BTreeMap<String, u64>, bool), String> {
    let failed = |error: &dyn std::fmt::Display| format!("{}: {error}", root.display());
    let dir = tilo_files::Dir::open(tilo_files::Location::Native(root.to_path_buf()))
        .map_err(|error| failed(&error))?;
    let mut files = BTreeMap::new();
    let mut stack = vec![(VPath::root(), 0usize)];
    let mut visited = 0usize;
    let mut truncated = false;
    while let Some((folder, depth)) = stack.pop() {
        let entries = tilo_files::blocking::list(&dir, &folder).map_err(|error| failed(&error))?;
        for entry in entries {
            visited = visited.saturating_add(1);
            if visited > MAX_VISITED {
                truncated = true;
                break;
            }
            let Ok(name) = VPath::new(&entry.name) else {
                // A name this tree's paths cannot spell (a newline, a
                // backslash) is not comparable here; it is skipped rather
                // than guessed at.
                continue;
            };
            let path = folder.join(&name);
            match entry.kind {
                Kind::Dir => {
                    if depth < MAX_DEPTH {
                        stack.push((path, depth.saturating_add(1)));
                    } else {
                        truncated = true;
                    }
                }
                Kind::File => {
                    let Ok(meta) = tilo_files::blocking::metadata(&dir, &path) else {
                        continue;
                    };
                    files.insert(path.as_str().to_owned(), meta.len);
                }
                Kind::Other => {}
            }
        }
        if visited > MAX_VISITED {
            break;
        }
    }
    Ok((files, truncated))
}

fn read(path: &Path) -> Option<Vec<u8>> {
    let (directory, name) = tilo_files::file_at(path).ok()?;
    tilo_files::blocking::read(&directory, &name).ok()
}

fn files_equal(left: &Path, right: &Path) -> bool {
    match (read(left), read(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(entries: &[(&str, u64)]) -> BTreeMap<String, u64> {
        entries
            .iter()
            .map(|(path, len)| ((*path).to_owned(), *len))
            .collect()
    }

    /// A file only one side has is added or removed; a file both have is
    /// modified unless its bytes agree.
    #[test]
    fn a_listing_names_what_each_side_holds_alone() {
        let left = tree(&[("a.txt", 3), ("b.txt", 4), ("same.txt", 2)]);
        let right = tree(&[("b.txt", 5), ("c.txt", 1), ("same.txt", 2)]);
        let entries = differences(&left, &right, |path| path == "same.txt");
        let kinds: Vec<(&str, FolderKind)> =
            entries.iter().map(|e| (e.path.as_str(), e.kind)).collect();
        assert_eq!(
            kinds,
            vec![
                ("a.txt", FolderKind::Added),
                ("b.txt", FolderKind::Modified),
                ("c.txt", FolderKind::Removed),
            ],
            "one entry per differing file, in path order"
        );
        assert_eq!(entries[1].left_len, 4);
        assert_eq!(entries[1].right_len, 5);
        assert!(entries[1].by_length, "a length difference is not read");
    }

    /// Two files of equal length that differ only in content are still
    /// reported: equal lengths are not equal files.
    #[test]
    fn equal_lengths_are_not_equal_files() {
        let left = tree(&[("x", 3)]);
        let right = tree(&[("x", 3)]);
        let entries = differences(&left, &right, |_| false);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, FolderKind::Modified);
        assert!(!entries[0].by_length, "the contents were compared");
        assert!(differences(&left, &right, |_| true).is_empty());
    }

    #[test]
    fn comparing_two_folders_names_the_files_that_differ() {
        let base = std::env::temp_dir().join(format!(
            "tilo-diffmerge-folder-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let left = AbsolutePath::new(base.join("left")).unwrap();
        let right = AbsolutePath::new(base.join("right")).unwrap();
        for tree in [&left, &right] {
            std::fs::create_dir_all(tree.join("src")).unwrap();
        }
        std::fs::write(left.join("src/kept.rs"), "same\n").unwrap();
        std::fs::write(right.join("src/kept.rs"), "same\n").unwrap();
        // Same length, different bytes: only a content comparison sees it.
        std::fs::write(left.join("src/edited.rs"), "one\n").unwrap();
        std::fs::write(right.join("src/edited.rs"), "two\n").unwrap();
        std::fs::write(left.join("src/removed.rs"), "gone\n").unwrap();
        std::fs::write(right.join("src/added.rs"), "new\n").unwrap();

        let comparison = compare(&left, &right).unwrap();
        let listed: Vec<(&str, FolderKind)> = comparison
            .entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry.kind))
            .collect();
        assert_eq!(
            listed,
            vec![
                ("src/added.rs", FolderKind::Removed),
                ("src/edited.rs", FolderKind::Modified),
                ("src/removed.rs", FolderKind::Added),
            ]
        );
        assert_eq!(comparison.counts(), (1, 1, 1));
        assert_eq!(comparison.left_label, "left");
        assert_eq!(comparison.right_label, "right");
        assert!(!comparison.truncated);
        let (left_path, right_path) = comparison.paths(&comparison.entries[1]);
        assert_eq!(
            left_path.as_deref(),
            Some(left.join("src/edited.rs").as_path()),
            "the entry names the file in each tree"
        );
        assert!(right_path.is_some());

        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_missing_folder_fails() {
        let missing =
            AbsolutePath::new(std::env::temp_dir().join("tilo-diffmerge-no-such-folder")).unwrap();
        let error = compare(&missing, &missing).unwrap_err();
        assert!(error.contains("tilo-diffmerge-no-such-folder"), "{error}");
    }
}
