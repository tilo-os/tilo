//! Dropbox metadata (`FileMetadata`, `FolderMetadata`, `DeletedMetadata`)
//! and the id/path bookkeeping that turns path-addressed entries into
//! id-addressed items.

use std::collections::{HashMap, HashSet};

use cloud_source::ItemId;
use serde::Deserialize;

/// One metadata entry. Tagged (`.tag`) where Dropbox returns the `Metadata`
/// union; untagged where a route returns `FileMetadata` directly.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Entry {
    #[serde(rename = ".tag", default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub path_lower: Option<String>,
    #[serde(default)]
    pub rev: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub server_modified: Option<String>,
    #[serde(default)]
    pub content_hash: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Folder,
    Deleted,
}

impl Entry {
    pub fn parse(value: &serde_json::Value) -> Option<Self> {
        Self::deserialize(value).ok()
    }

    /// The entry's kind; untagged entries are files when they carry a
    /// `rev` (only files have one) and folders otherwise.
    pub fn kind(&self) -> Kind {
        match self.tag.as_deref() {
            Some("file") => Kind::File,
            Some("folder") => Kind::Folder,
            Some("deleted") => Kind::Deleted,
            _ if self.rev.is_some() => Kind::File,
            _ => Kind::Folder,
        }
    }
}

/// The parent path of a lowercased Dropbox path (`"/a/b"` → `"/a"`,
/// `"/a"` → `""`).
pub fn parent_path(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

pub fn is_within(path: &str, root: &str) -> bool {
    root.is_empty()
        || path == root
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Known ids by lowercased path and back.
///
/// Dropbox entries carry paths, not parent ids, and a `deleted` entry
/// carries only its path. This map is how the source names parents and
/// attributes deletions to the ids it reported earlier.
#[derive(Debug, Default)]
pub struct Paths {
    by_path: HashMap<String, ItemId>,
    by_id: HashMap<ItemId, String>,
    /// Paths an id moved away from since, whose `deleted` entries are
    /// the old half of a move rather than a removal.
    vacated: HashSet<String>,
}

impl Paths {
    pub fn get(&self, path: &str) -> Option<&ItemId> {
        self.by_path.get(path)
    }

    /// Record that `id` lives at `path`.
    pub fn insert(&mut self, id: &ItemId, path: &str) {
        if let Some(old) = self.by_id.get(id).cloned()
            && old != path
        {
            if self.by_path.get(&old) == Some(id) {
                self.by_path.remove(&old);
            }
            self.vacated.insert(old);
        }
        self.vacated.remove(path);
        if let Some(previous) = self.by_path.insert(path.to_owned(), id.clone())
            && &previous != id
        {
            self.by_id.remove(&previous);
        }
        self.by_id.insert(id.clone(), path.to_owned());
    }

    /// Forget `path` and everything below it, returning their ids with
    /// `path`'s own first.
    pub fn remove_tree(&mut self, path: &str) -> Vec<ItemId> {
        if path.is_empty() {
            // The account root is never deleted.
            return Vec::new();
        }
        let mut doomed: Vec<(String, ItemId)> = self
            .by_path
            .iter()
            .filter(|(candidate, _)| is_within(candidate, path))
            .map(|(candidate, id)| (candidate.clone(), id.clone()))
            .collect();
        doomed.sort_by_key(|(candidate, _)| candidate.len());
        for (candidate, id) in &doomed {
            self.by_path.remove(candidate);
            self.by_id.remove(id);
        }
        doomed.into_iter().map(|(_, id)| id).collect()
    }

    /// A folder moved from `from` to `to`: rewrite the paths below it.
    pub fn rebase(&mut self, from: &str, to: &str) {
        if from == to || from.is_empty() {
            return;
        }
        let moved: Vec<(String, ItemId)> = self
            .by_path
            .iter()
            .filter(|(candidate, _)| candidate.as_str() != from && is_within(candidate, from))
            .map(|(candidate, id)| (candidate.clone(), id.clone()))
            .collect();
        for (old, id) in moved {
            let rest = old.get(from.len()..).unwrap_or_default();
            let new = format!("{to}{rest}");
            self.by_path.remove(&old);
            self.by_path.insert(new.clone(), id.clone());
            self.by_id.insert(id, new);
        }
    }

    /// Consume a vacated mark for `path`.
    pub fn take_vacated(&mut self, path: &str) -> bool {
        self.vacated.remove(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untagged_file_metadata_is_a_file() {
        let entry = Entry::parse(&serde_json::json!({
            "name": "a.txt", "id": "id:1", "rev": "015f00000001", "size": 3
        }))
        .unwrap();
        assert_eq!(entry.kind(), Kind::File);
        let folder = Entry::parse(&serde_json::json!({"name": "D", "id": "id:2"})).unwrap();
        assert_eq!(folder.kind(), Kind::Folder);
        let deleted =
            Entry::parse(&serde_json::json!({".tag": "deleted", "name": "x", "path_lower": "/x"}))
                .unwrap();
        assert_eq!(deleted.kind(), Kind::Deleted);
    }

    #[test]
    fn paths_follow_moves_and_deletions() {
        let mut paths = Paths::default();
        let (folder, child) = (ItemId::new("id:f"), ItemId::new("id:c"));
        paths.insert(&folder, "/a");
        paths.insert(&child, "/a/c.txt");
        paths.insert(&folder, "/b");
        paths.rebase("/a", "/b");
        assert!(paths.take_vacated("/a"));
        assert_eq!(paths.get("/b/c.txt"), Some(&child));
        assert!(paths.get("/a/c.txt").is_none());
        assert_eq!(paths.remove_tree("/b"), vec![folder, child]);
        assert!(paths.get("/b").is_none());
        assert!(is_within("/a/b", "/a"));
        assert!(!is_within("/ab", "/a"));
        assert_eq!(parent_path("/a/b"), "/a");
        assert_eq!(parent_path("/a"), "");
    }
}
