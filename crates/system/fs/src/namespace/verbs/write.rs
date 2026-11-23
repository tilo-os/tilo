//! A file written into a workspace's tree by a path in the workspace: its content is kept by
//! the workspace already, and the tree gets the node that names it.
//!
//! Whoever holds the tree's documents writes it with no folder mounted: a workspace's server
//! while every device of its people is off. A path is read as the verbs read one ([`super`]),
//! and nothing is written over unless the caller says so.

use crate::node::FileType;

use super::super::{hash_of, plain};
use super::{Changed, join, names};
use crate::namespace::Namespace;

/// A file written into the tree.
pub struct Written {
    /// What the write changed: the file, as its one entry, and the folders made on the way.
    pub changed: Changed,
    /// Whether a file of that name was there and took the new content.
    pub replaced: bool,
    /// What the tree named that file's content by before it took the new one.
    pub was: Option<String>,
}

impl Namespace {
    /// Write the file `name` into the folder `folder`, making the folders on the way that are
    /// not there yet: its content is what the workspace keeps under `content`, `size` bytes
    /// long. A file of that name is left as it is and the write refused, unless `replace`,
    /// which gives that file the new content.
    ///
    /// # Errors
    /// A path that is none of the workspace, a name no file may have, an address that is none,
    /// a file in the way of the folder, a folder of that name, a text document the tree itself
    /// keeps, or a name that is taken when `replace` is not asked.
    pub fn write_file(
        &mut self,
        folder: &str,
        name: &str,
        (content, size): (&str, u64),
        replace: bool,
        mtime: u64,
    ) -> Result<Written, String> {
        if !plain(name) {
            return Err(format!("{name:?} is no file's name"));
        }
        let base = names(folder)?.join("/");
        let path = join(&base, name);
        let hash = hash_of(content).ok_or("the content's address is not one")?;
        // Checked whole before anything is written, the folders on the way included.
        if let Some(parent) = self.folder_if_there(&base)?
            && let Some(attr) = self.lookup(parent, name)
        {
            if attr.kind == FileType::Directory {
                return Err(format!("{path:?} is a folder"));
            }
            if !replace {
                return Err(format!(
                    "{path:?} is there already: `replace: true` writes over it"
                ));
            }
        }
        let before = self.before_change();
        let mut made = Vec::new();
        let parent = self.folder_made(&base, mtime, &mut made)?;
        let (ino, replaced, was) = if let Some(attr) = self.lookup(parent, name) {
            let ino: u64 = attr.ino.into();
            let was = self.hash_for(ino).map(|hash| hex::encode(hash.as_bytes()));
            if !self.set_content(ino, Some(&hash), size, mtime) {
                return Err(format!("{path:?} could not take the file"));
            }
            (ino, true, was)
        } else {
            let ino = self
                .create_file(parent, name, Some(&hash), size, mtime)
                .ok_or_else(|| format!("{path:?} could not be written"))?;
            (ino, false, None)
        };
        let from = replaced.then(|| path.clone());
        let touched = self.touched(ino, from, Some(path), false)?;
        Ok(Written {
            changed: Changed {
                writes: self.document_writes(&before),
                entries: vec![touched],
                made,
            },
            replaced,
            was,
        })
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    fn hash(fill: u8) -> String {
        hex::encode([fill; 32])
    }

    #[test]
    fn a_file_is_written_once_and_over_another_only_when_asked() {
        let mut tree = Namespace::new("server").unwrap();
        let report = "Tasks/12 Plan/report.md";
        let written = tree
            .write_file("Tasks/12 Plan", "report.md", (&hash(1), 10), false, 5)
            .unwrap();
        assert_eq!(written.changed.made, ["Tasks", "Tasks/12 Plan"]);
        let file = written.changed.entries.first().unwrap().clone();
        assert_eq!(file.to.as_deref(), Some(report));
        assert!(file.from.is_none() && !file.folder);
        assert!(!written.replaced && written.was.is_none());
        assert!(
            written
                .changed
                .writes
                .iter()
                .any(|write| !write.changes.is_empty())
        );
        assert_eq!(tree.file_at(report).unwrap().content, Some(hash(1)));

        // The name is taken: refused, and the file is as it was.
        assert!(
            tree.write_file("Tasks/12 Plan", "report.md", (&hash(2), 20), false, 6)
                .is_err()
        );
        assert_eq!(tree.file_at(report).unwrap().content, Some(hash(1)));

        // Asked to, it gives the same file the new content and says what it held.
        let over = tree
            .write_file("Tasks/12 Plan", "report.md", (&hash(2), 20), true, 6)
            .unwrap();
        assert!(over.replaced && over.changed.made.is_empty());
        assert_eq!(over.was, Some(hash(1)));
        let again = over.changed.entries.first().unwrap();
        assert_eq!(again.id, file.id, "the same node");
        assert_eq!(again.from.as_deref(), Some(report));
        let now = tree.file_at(report).unwrap();
        assert_eq!((now.content, now.size), (Some(hash(2)), 20));

        // A path stays in the workspace, and a refused write makes no folder on its way.
        for outside in ["/tmp", "Tasks/../elsewhere", "..", "./Tasks"] {
            assert!(
                tree.write_file(outside, "x.md", (&hash(3), 1), true, 7)
                    .is_err(),
                "{outside:?}"
            );
        }
        for name in ["", "..", "a/b.md"] {
            assert!(
                tree.write_file("", name, (&hash(3), 1), true, 7).is_err(),
                "{name:?}"
            );
        }
        assert!(
            tree.write_file("Tasks", "12 Plan", (&hash(3), 1), true, 7)
                .is_err(),
            "a folder is not written over"
        );
        assert!(
            tree.write_file(report, "x.md", (&hash(3), 1), true, 7)
                .is_err(),
            "a file is in the way of the folder"
        );
        assert!(
            tree.write_file("New/deep", "x.md", ("no address", 1), false, 7)
                .is_err()
        );
        let top = tree.list_folder("", 0, 10).unwrap();
        assert_eq!(top.total, 1, "nothing but Tasks was made");
        assert_eq!(tree.list_folder("Tasks/12 Plan", 0, 10).unwrap().total, 1);
    }
}
