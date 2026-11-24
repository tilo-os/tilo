//! Comparing two folder trees: what only one side holds, and what both hold but differ in.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value as Json, json};

/// The most files one comparison lists: a person reads a list, not a disk.
pub const MAX_ENTRIES: usize = 5_000;
/// The most entries either tree is walked through: a folder that keeps growing under the walk
/// stops being compared rather than never answering.
pub const MAX_VISITED: usize = 50_000;
/// How deep a tree is walked.
pub const MAX_DEPTH: usize = 24;
/// The most a pair of files is read and held to compare their bytes: past it their lengths
/// decide.
pub const MAX_COMPARE_BYTES: u64 = 8 * 1024 * 1024;

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
    /// The files were too large to compare by content, so only their lengths were compared.
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

    /// The comparison as the app reads it: one list per fact of a file, so a file is a place
    /// among them.
    pub fn to_json(&self) -> Json {
        let mut paths = Vec::with_capacity(self.entries.len());
        let mut kinds = Vec::with_capacity(self.entries.len());
        let mut left_lens = Vec::with_capacity(self.entries.len());
        let mut right_lens = Vec::with_capacity(self.entries.len());
        let mut by_lengths = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            paths.push(entry.path.clone());
            kinds.push(entry.kind.tag());
            left_lens.push(entry.left_len);
            right_lens.push(entry.right_len);
            by_lengths.push(entry.by_length);
        }
        json!({
            "left_label": self.left_label,
            "left_root": self.left_root,
            "right_label": self.right_label,
            "right_root": self.right_root,
            "entry_paths": paths,
            "entry_kinds": kinds,
            "entry_left_lens": left_lens,
            "entry_right_lens": right_lens,
            "entry_by_lengths": by_lengths,
            "truncated": self.truncated,
        })
    }
}

/// The name a tree's root is shown by.
pub fn root_label(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

pub fn compare(left: &Path, right: &Path) -> Result<FolderComparison, String> {
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
        left_root: left.to_string_lossy().into_owned(),
        right_label: root_label(right),
        right_root: right.to_string_lossy().into_owned(),
        entries,
        truncated,
    })
}

/// The entries that differ between two listings of `relative path → bytes`, where `same_content`
/// answers whether a file of equal length on both sides really holds the same bytes. Paths only
/// one side has are added or removed; paths both sides have differ unless `same_content` says no.
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

/// Every file a tree holds, by the path under its root and its length, and whether the walk
/// stopped before it saw all of it.
fn walk(root: &Path) -> Result<(BTreeMap<String, u64>, bool), String> {
    let failed = |error: &dyn std::fmt::Display| format!("{}: {error}", root.display());
    let meta = std::fs::metadata(root).map_err(|error| failed(&error))?;
    if !meta.is_dir() {
        return Err(format!("{} is not a folder", root.display()));
    }
    let mut files = BTreeMap::new();
    let mut stack = vec![(root.to_path_buf(), String::new(), 0usize)];
    let mut visited = 0usize;
    let mut truncated = false;
    while let Some((folder, relative, depth)) = stack.pop() {
        let listing = std::fs::read_dir(&folder).map_err(|error| failed(&error))?;
        for entry in listing {
            visited = visited.saturating_add(1);
            if visited > MAX_VISITED {
                truncated = true;
                break;
            }
            let Ok(entry) = entry else {
                continue;
            };
            let Ok(name) = entry.file_name().into_string() else {
                // A name that is not text is not comparable here; it is skipped rather than
                // guessed at.
                continue;
            };
            let path = folder.join(&name);
            let under = if relative.is_empty() {
                name.clone()
            } else {
                format!("{relative}/{name}")
            };
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((path, under, depth.saturating_add(1)));
                } else {
                    truncated = true;
                }
            } else if meta.is_file() {
                files.insert(under, meta.len());
            }
            // A link, a socket or a device is not a file a person compares.
        }
        if visited > MAX_VISITED {
            break;
        }
    }
    Ok((files, truncated))
}

fn files_equal(left: &Path, right: &Path) -> bool {
    match (std::fs::read(left), std::fs::read(right)) {
        (Ok(left), Ok(right)) => left == right,
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

    tilo_test_env::scenario! {
        fn two_folders_compare_by_what_each_holds_and_by_content;

        phase a_listing_names_what_each_side_holds_alone;
        {
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

        phase equal_lengths_are_not_equal_files;
        {
            let left = tree(&[("x", 3)]);
            let right = tree(&[("x", 3)]);
            let entries = differences(&left, &right, |_| false);
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].kind, FolderKind::Modified);
            assert!(!entries[0].by_length, "the contents were compared");
            assert!(differences(&left, &right, |_| true).is_empty());
        }

        phase comparing_two_folders_names_the_files_that_differ;
        {
            let base = std::env::temp_dir().join(format!(
                "tilo-diffmerge-folder-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let left = base.join("left");
            let right = base.join("right");
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

            std::fs::remove_dir_all(&base).unwrap();
        }

        phase a_missing_folder_fails;
        {
            let missing = std::env::temp_dir().join("tilo-diffmerge-no-such-folder");
            let error = compare(&missing, &missing).unwrap_err();
            assert!(error.contains("tilo-diffmerge-no-such-folder"), "{error}");
        }
    }
}
