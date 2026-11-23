//! What a workspace's tree holds, read by a path in the workspace: a folder's entries a page at
//! a time, what its trash holds, and where one file's content is kept.
//!
//! Whoever holds the tree's documents answers these with no folder mounted and no window open:
//! a workspace's server while every device of its people is off. A path is read as the verbs
//! read one ([`super`]): it starts at the workspace's folder and stays in it.

use crate::node::FileType;

use super::{join, names};
use crate::namespace::{Namespace, NodeKind, ROOT_ID};

/// The most entries one page of a folder, or of the trash, lists.
pub const MOST_LISTED: usize = 200;

/// One entry of a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The node's key in the tree.
    pub id: String,
    /// Its name in the folder.
    pub name: String,
    /// Where it is, as a path in the workspace.
    pub path: String,
    /// Whether it is a folder.
    pub folder: bool,
    /// A file's length in bytes.
    pub size: u64,
    /// When it last changed, in milliseconds since 1970.
    pub changed: u64,
}

/// One entry of the trash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trashed {
    /// The node's key in the tree: what brings it back out of the trash.
    pub id: String,
    /// The name it had.
    pub name: String,
    /// Where it was, as a path in the workspace.
    pub was: String,
    /// Whether it is a folder.
    pub folder: bool,
}

/// One page of what a folder, or the trash, holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page<T> {
    /// The entries of this page, by name for a folder.
    pub entries: Vec<T>,
    /// The place of the first of them among all it holds.
    pub from: usize,
    /// Where the next page starts, when there is more.
    pub next: Option<usize>,
    /// How many entries it holds in all.
    pub total: usize,
}

/// A file of the tree, and what its content is read from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileAt {
    /// The node's key in the tree.
    pub id: String,
    /// The node, as this reading of the tree numbers it.
    pub ino: u64,
    /// Where it is, as a path in the workspace.
    pub path: String,
    /// Its length in bytes.
    pub size: u64,
    /// When it last changed, in milliseconds since 1970.
    pub changed: u64,
    /// The address its content manifest is kept under in the workspace's store; `None` for an
    /// empty file.
    pub content: Option<String>,
}

/// Where a page asked from `from` with at most `limit` entries starts and ends among `total`.
fn bounds(total: usize, from: usize, limit: usize) -> (usize, usize) {
    let from = from.min(total);
    let to = from.saturating_add(limit.clamp(1, MOST_LISTED)).min(total);
    (from, to)
}

impl Namespace {
    /// The entries of the folder at `folder`, by name: at most `limit` of them (and never more
    /// than [`MOST_LISTED`]) starting at the place `from`.
    ///
    /// # Errors
    /// A path that is none of the workspace, a folder that is not there, or a file named as one.
    pub fn list_folder(
        &mut self,
        folder: &str,
        from: usize,
        limit: usize,
    ) -> Result<Page<Row>, String> {
        let base = names(folder)?.join("/");
        let ino = self.folder_at(&base)?;
        let held: Vec<(u64, String)> = self
            .readdir(ino)
            .unwrap_or_default()
            .into_iter()
            .map(|(child, _, name)| (child, name.display_lossy()))
            .filter(|(_, name)| name != "." && name != "..")
            .collect();
        let total = held.len();
        let (from, to) = bounds(total, from, limit);
        let mut entries = Vec::new();
        for (child, name) in held.into_iter().skip(from).take(to.saturating_sub(from)) {
            let Some(entry) = self
                .id_for_ino(child)
                .and_then(|id| self.entry_for_node(id))
            else {
                continue;
            };
            entries.push(Row {
                id: entry.id,
                path: join(&base, &name),
                name,
                folder: entry.kind == NodeKind::Dir,
                size: entry.size,
                changed: entry.mtime,
            });
        }
        Ok(Page {
            entries,
            from,
            next: (to < total).then_some(to),
            total,
        })
    }

    /// What the workspace's trash holds, each entry with the key that brings it back
    /// ([`Self::restore_entries`]) and where it was: at most `limit` of them starting at the
    /// place `from`.
    #[must_use]
    pub fn list_trash(&self, from: usize, limit: usize) -> Page<Trashed> {
        let trashed = self.directly_trashed();
        let total = trashed.len();
        let (from, to) = bounds(total, from, limit);
        let entries = trashed
            .into_iter()
            .skip(from)
            .take(to.saturating_sub(from))
            .filter_map(|id| {
                let name = self.node_parent_name(&id)?.1.display_lossy();
                // The folder it was deleted from, while that folder is still there.
                let within = self
                    .prev_parent(&id)
                    .filter(|parent| parent == ROOT_ID || self.is_visible(parent))
                    .and_then(|parent| self.path_for_id(&parent))
                    .unwrap_or_default();
                Some(Trashed {
                    was: join(within.trim_matches('/'), &name),
                    folder: self.node_is_dir(&id).unwrap_or(false),
                    name,
                    id,
                })
            })
            .collect();
        Page {
            entries,
            from,
            next: (to < total).then_some(to),
            total,
        }
    }

    /// The file at `path`, and what its content is read from.
    ///
    /// # Errors
    /// A path that is none of the workspace, a file that is not there, or a folder named as one.
    pub fn file_at(&mut self, path: &str) -> Result<FileAt, String> {
        let parts = names(path)?;
        let Some((name, folders)) = parts.split_last() else {
            return Err("a file is named by its path in the workspace".to_owned());
        };
        let holder = folders.join("/");
        let parent = self.folder_at(&holder)?;
        let at = join(&holder, name);
        let attr = self
            .lookup(parent, name)
            .ok_or_else(|| format!("the workspace has no {at:?}"))?;
        if attr.kind == FileType::Directory {
            return Err(format!("{at:?} is a folder: `list` answers what it holds"));
        }
        let ino: u64 = attr.ino.into();
        let entry = self
            .id_for_ino(ino)
            .and_then(|id| self.entry_for_node(id))
            .ok_or_else(|| format!("the workspace has no {at:?}"))?;
        Ok(FileAt {
            id: entry.id,
            ino,
            path: at,
            size: entry.size,
            changed: entry.mtime,
            content: entry.hash.map(|hash| hex::encode(hash.as_bytes())),
        })
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    fn hash(fill: u8) -> String {
        hex::encode([fill; 32])
    }

    fn named(page: &Page<Row>) -> Vec<&str> {
        page.entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect()
    }

    /// Put one file into `tree`, as a mount copying it in does.
    fn put(tree: &mut Namespace, to: &str, name: &str, hash: &str, size: u64, mtime: u64) {
        tree.put_files(&[super::super::FileToPut {
            to,
            name,
            hash,
            size,
            mtime,
        }])
        .unwrap();
    }

    #[test]
    fn a_folder_lists_a_page_at_a_time_and_a_file_is_found_by_its_path() {
        let mut tree = Namespace::new("server").unwrap();
        tree.make_folder("Tasks/12 Plan", "sources", 3).unwrap();
        put(&mut tree, "Tasks/12 Plan", "notes.md", &hash(1), 11, 4);
        put(&mut tree, "Tasks/12 Plan", "budget.csv", &hash(2), 22, 5);

        // By name, two at a time, and the second page says there is no third.
        let first = tree.list_folder("Tasks/12 Plan", 0, 2).unwrap();
        assert_eq!(named(&first), ["budget.csv", "notes.md"]);
        assert_eq!((first.from, first.next, first.total), (0, Some(2), 3));
        let budget = first.entries.first().unwrap();
        assert_eq!(budget.path, "Tasks/12 Plan/budget.csv");
        assert!(!budget.folder);
        assert_eq!(budget.size, 22);
        let rest = tree.list_folder("Tasks/12 Plan/", 2, 2).unwrap();
        assert_eq!(named(&rest), ["sources"]);
        assert_eq!((rest.from, rest.next, rest.total), (2, None, 3));
        assert!(rest.entries.first().unwrap().folder);
        // The workspace's own folder is the empty path, and a place past the end lists nothing.
        let root = tree.list_folder("", 0, MOST_LISTED).unwrap();
        assert_eq!(named(&root), ["Tasks"]);
        let past = tree.list_folder("", 9, 5).unwrap();
        assert!(past.entries.is_empty());
        assert_eq!((past.from, past.next, past.total), (1, None, 1));

        // A file is found by its path, with what its content is kept under.
        let notes = tree.file_at("Tasks/12 Plan/notes.md").unwrap();
        assert_eq!(notes.path, "Tasks/12 Plan/notes.md");
        assert_eq!(notes.size, 11);
        assert_eq!(notes.content, Some(hash(1)));
        assert_eq!(
            notes.id,
            first.entries.get(1).unwrap().id,
            "the same node the folder listed"
        );

        // A path stays in the workspace, a folder is no file and a file no folder.
        for outside in ["/etc/passwd", "Tasks/../Tasks", "..", "./Tasks", "Tasks/."] {
            assert!(tree.list_folder(outside, 0, 10).is_err(), "{outside:?}");
            assert!(tree.file_at(outside).is_err(), "{outside:?}");
        }
        assert!(tree.file_at("").is_err(), "no file is named");
        assert!(tree.file_at("Tasks/12 Plan").is_err(), "a folder");
        assert!(tree.file_at("Tasks/12 Plan/never.md").is_err());
        assert!(tree.list_folder("Tasks/12 Plan/notes.md", 0, 10).is_err());
        assert!(tree.list_folder("Nowhere", 0, 10).is_err());
    }

    #[test]
    fn the_trash_lists_what_it_holds_with_the_key_that_brings_it_back() {
        let mut tree = Namespace::new("server").unwrap();
        put(&mut tree, "Tasks/7", "notes.md", &hash(3), 4, 1);
        put(&mut tree, "Tasks/7", "plan.md", &hash(4), 4, 1);
        assert_eq!(tree.list_trash(0, 10).total, 0);
        let gone = tree
            .trash_entries("Tasks/7", &["notes.md".to_owned()])
            .unwrap();
        let trash = tree.list_trash(0, 10);
        assert_eq!((trash.from, trash.next, trash.total), (0, None, 1));
        let held = trash.entries.first().unwrap();
        assert_eq!(held.id, gone.entries.first().unwrap().id);
        assert_eq!(held.name, "notes.md");
        assert_eq!(held.was, "Tasks/7/notes.md");
        assert!(!held.folder);
        // What is in the trash is in no folder, and comes back by the key the trash lists.
        assert_eq!(
            named(&tree.list_folder("Tasks/7", 0, 10).unwrap()),
            ["plan.md"]
        );
        tree.restore_entries(std::slice::from_ref(&held.id))
            .unwrap();
        assert_eq!(tree.list_trash(0, 10).total, 0);
        assert_eq!(
            named(&tree.list_folder("Tasks/7", 0, 10).unwrap()),
            ["notes.md", "plan.md"]
        );
    }
}
