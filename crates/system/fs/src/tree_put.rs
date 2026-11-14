//! A file put into a workspace's tree by someone who holds the record and no
//! folder of its own: a workspace's host, which keeps the Files record its
//! members' files are, and writes a file into it for a command or an agent
//! that has the bytes but no mount.
//!
//! The node is written where the filesystem writes one it creates — the same
//! layout [`namespace`] keeps and a mounted copy reads — so a member's folder
//! shows the file as if it had been copied in there. The content stays where
//! the workspace keeps it, named by the node's hash: a whole file, or the
//! manifest of the chunks a large one was split into.

use fuser::FileType;
use iroh_blobs::Hash;

use crate::namespace::{Namespace, ROOT_INO, hash_from_hex};

/// A file put into the tree: the change that puts it there, and where in the
/// tree it now is.
pub struct Placed {
    /// The change to the tree's record, as the namespace saves one: no heads
    /// to merge it against, the record it belongs to already the reader's.
    pub change: Vec<u8>,
    /// Where the file is, as a path from the workspace's folder
    /// (`docs/2026/plan.md`).
    pub path: String,
    /// Whether a file of that name was there and took the new content, rather
    /// than a node being made for it.
    pub replaced: bool,
}

fn refused(why: impl Into<String>) -> String {
    why.into()
}

/// A name no file may have: empty, `.`, `..`, or with a `/`.
fn plain(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
}

fn hash_of(hex: &str) -> Option<Hash> {
    hash_from_hex(hex)
}

impl Namespace {
    /// Put the file `name`, whose content the workspace keeps under `hash`,
    /// into the folder `to` of the tree — `""` for its root — making the
    /// folders on the way that are not there yet.
    ///
    /// A file of that name already in the folder takes the new content, as a
    /// copy over it does; a folder of that name there refuses it.
    ///
    /// # Errors
    /// A name no file may have, a file named as a folder on the way, a folder
    /// where the file goes, or content that is not a hash.
    pub fn put_file(
        &mut self,
        to: &str,
        name: &str,
        hash: &str,
        size: u64,
        mtime: u64,
    ) -> Result<Placed, String> {
        if !plain(name) {
            return Err(refused(format!("{name:?} is no file's name")));
        }
        let hash = hash_of(hash).ok_or_else(|| refused("the content's hash is not one"))?;
        let mut parent = ROOT_INO;
        for folder in to.split('/').filter(|folder| !folder.is_empty()) {
            if !plain(folder) {
                return Err(refused(format!("{folder:?} is no folder's name")));
            }
            parent = match self.lookup(parent, folder) {
                Some(attr) if attr.kind == FileType::Directory => attr.ino.into(),
                Some(_) => {
                    return Err(refused(format!(
                        "{folder} is a file, not a folder to put {name} in"
                    )));
                }
                None => self
                    .create_dir(parent, folder, mtime)
                    .ok_or_else(|| refused(format!("the folder {folder} could not be made")))?,
            };
        }
        let replaced = match self.lookup(parent, name) {
            Some(attr) if attr.kind == FileType::Directory => {
                return Err(refused(format!("a folder named {name} is already there")));
            }
            Some(attr) => {
                if !self.set_content(attr.ino.into(), Some(&hash), size, mtime) {
                    return Err(refused(format!("{name} could not take the file")));
                }
                true
            }
            None => {
                self.create_file(parent, name, Some(&hash), size, mtime)
                    .ok_or_else(|| refused(format!("{name} could not be put in the tree")))?;
                false
            }
        };
        let change = self.save_incremental();
        let path = match to.trim_matches('/') {
            "" => name.to_owned(),
            folder => format!("{folder}/{name}"),
        };
        Ok(Placed {
            change,
            path,
            replaced,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use super::*;

    fn hash(fill: u8) -> String {
        hex::encode([fill; 32])
    }

    /// A namespace read back from what `change` saved over `base`.
    fn applied(base: &[u8], change: &[u8], author: &str) -> Namespace {
        let mut namespace = Namespace::from_bytes(author, base).unwrap();
        if !change.is_empty() {
            namespace.apply_incremental(change).unwrap();
        }
        namespace
    }

    /// The one file at `path` in `namespace`, as members read it.
    fn at(namespace: &mut Namespace, path: &[&str]) -> Option<(u64, Hash, u64)> {
        let mut ino = ROOT_INO;
        for folder in &path[..path.len().saturating_sub(1)] {
            ino = namespace.lookup(ino, folder)?.ino.into();
        }
        let node = namespace.lookup(ino, path.last()?)?;
        let ino_of_node: u64 = node.ino.into();
        assert_eq!(node.kind, FileType::RegularFile, "a file, not a folder");
        Some((
            ino_of_node,
            namespace.hash_for(ino_of_node)?,
            namespace.getattr(ino_of_node)?.size,
        ))
    }

    #[test]
    fn a_put_file_is_a_node_of_the_tree_its_members_read() {
        let mut namespace = Namespace::new("host").unwrap();
        let put = namespace
            .put_file("", "report.txt", &hash(1), 12, 7)
            .unwrap();
        assert_eq!(put.path, "report.txt");
        assert!(!put.replaced);
        let mut read = applied(&namespace.save(), &put.change, "member");
        let (_, content, size) = at(&mut read, &["report.txt"]).expect("the file is in the tree");
        assert_eq!(content, hash_of(&hash(1)).unwrap());
        assert_eq!(size, 12);
    }

    #[test]
    fn a_folder_path_makes_the_folders_and_a_second_put_takes_the_new_content() {
        let mut namespace = Namespace::new("host").unwrap();
        let first = namespace
            .put_file("notes/2026", "plan.md", &hash(2), 20, 8)
            .unwrap();
        assert_eq!(first.path, "notes/2026/plan.md");
        let again = namespace
            .put_file("/notes/2026/", "plan.md", &hash(3), 21, 9)
            .unwrap();
        assert!(again.replaced, "the same name takes the new content");
        assert_eq!(again.path, "notes/2026/plan.md");
        let mut read = applied(&namespace.save(), &[], "member");
        let (_, content, size) = at(&mut read, &["notes", "2026", "plan.md"]).expect("there");
        assert_eq!(content, hash_of(&hash(3)).unwrap());
        assert_eq!(size, 21);
    }

    #[test]
    fn a_name_no_file_has_or_a_folder_in_its_way_is_refused() {
        let mut namespace = Namespace::new("host").unwrap();
        for name in ["", ".", "..", "a/b"] {
            assert!(
                namespace.put_file("", name, &hash(4), 1, 1).is_err(),
                "{name:?}"
            );
        }
        namespace
            .put_file("docs", "inner.txt", &hash(5), 1, 1)
            .unwrap();
        assert!(
            namespace.put_file("", "docs", &hash(6), 1, 1).is_err(),
            "a folder is not a file"
        );
        assert!(
            namespace
                .put_file("docs/inner.txt", "x", &hash(7), 1, 1)
                .is_err(),
            "a file is not a folder to put one in"
        );
    }
}
