//! Files carrying out someone else's call on the workspace's own folder: an agent's rename,
//! move, copy or new folder there is done to the workspace's tree as that agent, so the files'
//! History names the agent who asked, not Files. The mount then shows the tree as it is now,
//! as it shows any change of it.
//!
//! What the tree cannot do by itself (bytes nobody stored yet, a folder copied with what it
//! holds, the trash on this disk) Files does through the mount as before.

use std::path::{Path, PathBuf};

use fs_protocol::FileAction;
use fs_protocol::files::{FileDocumentId, FileNameV1, FileNodeKind};
use fs_protocol::tree::{ROOT_ID, Tree};
use ui::history_mode::folder::FolderHistorySource;

use crate::file_ops::{Change, FileOp, Outcome};

/// Carry `op` out on the tree `folder` shows, as `invoker`, when every path it names is a node
/// of that tree under one of the workspace roots `roots`. `None` when the tree cannot do it:
/// Files does it through the mount.
pub fn carry_out(
    op: &FileOp,
    roots: &[PathBuf],
    folder: &FolderHistorySource,
    invoker: &ui::tilo_actions::ActionInvoker,
) -> Option<Outcome> {
    let tree = folder.tree()?;
    let now = ui::time::unix_now_ms();
    let (actions, changes) = roots
        .iter()
        .find_map(|root| plan(op, &Placed { tree: &tree, root }, now))?;
    Some(match folder.act(&actions, invoker) {
        Ok(()) => Outcome::Done(changes),
        Err(reason) => Outcome::Failed {
            reason,
            partial: Vec::new(),
        },
    })
}

/// The workspace's tree, and where its root is on this disk.
struct Placed<'a> {
    tree: &'a Tree,
    root: &'a Path,
}

impl Placed<'_> {
    /// The node at `path`, a path under the root.
    fn node_at(&self, path: &str) -> Option<String> {
        let relative = Path::new(path).strip_prefix(self.root).ok()?;
        let mut node = ROOT_ID.to_owned();
        for part in relative.components() {
            let name = part.as_os_str().to_str()?;
            node = self.child(&node, name)?;
        }
        Some(node)
    }

    /// The live child of `parent` named `name`.
    fn child(&self, parent: &str, name: &str) -> Option<String> {
        self.tree
            .children(parent)
            .into_iter()
            .find(|(id, node)| node.name.as_bytes() == name.as_bytes() && !self.tree.trashed(id))
            .map(|(id, _)| id.to_owned())
    }

    fn is_folder(&self, node: &str) -> bool {
        self.tree
            .node(node)
            .is_some_and(|entry| entry.kind == FileNodeKind::Directory)
    }

    /// `name` in the folder `folder` is free.
    fn free(&self, folder: &str, name: &str) -> bool {
        self.child(folder, name).is_none()
    }
}

fn name_of(path: &str) -> Option<&str> {
    Path::new(path).file_name()?.to_str()
}

fn join(folder: &str, name: &str) -> String {
    PathBuf::from(folder).join(name).display().to_string()
}

/// The tree's actions `op` is, and the changes Files keeps of it. `None` for an operation
/// the tree cannot do by itself, or one on a path that is not a node of it.
fn plan(op: &FileOp, placed: &Placed<'_>, now: u64) -> Option<(Vec<FileAction>, Vec<Change>)> {
    let mut actions = Vec::new();
    let mut changes = Vec::new();
    match op {
        FileOp::NewFolder { parent, name } => {
            let folder = placed.node_at(parent)?;
            (placed.is_folder(&folder) && placed.free(&folder, name)).then_some(())?;
            let document_id = FileDocumentId::in_chunk(placed.tree.chunk_for_new());
            actions.push(FileAction::Create {
                node: fs_protocol::tree::document_node_id(document_id),
                document_id,
                parent: folder,
                name: FileNameV1::from(name.as_str()),
                kind: FileNodeKind::Directory,
                hash: None,
                size: 0,
                mtime: now,
                author: String::new(),
            });
            changes.push(Change::Created {
                path: join(parent, name),
            });
        }
        FileOp::Rename { path, name } => {
            let node = placed.node_at(path)?;
            let parent = placed.tree.node(&node)?.parent.clone()?;
            placed.free(&parent, name).then_some(())?;
            let to = join(Path::new(path).parent()?.to_str()?, name);
            actions.push(FileAction::Move {
                node,
                parent,
                name: FileNameV1::from(name.as_str()),
                mtime: now,
            });
            changes.push(Change::Moved {
                from: path.clone(),
                to,
            });
        }
        FileOp::Move {
            paths,
            folder,
            create: false,
        } => {
            let into = placed.node_at(folder)?;
            placed.is_folder(&into).then_some(())?;
            for path in paths {
                let node = placed.node_at(path)?;
                let name = name_of(path)?;
                placed.free(&into, name).then_some(())?;
                actions.push(FileAction::Move {
                    node,
                    parent: into.clone(),
                    name: FileNameV1::from(name),
                    mtime: now,
                });
                changes.push(Change::Moved {
                    from: path.clone(),
                    to: join(folder, name),
                });
            }
        }
        FileOp::Copy { paths, folder } => {
            let into = placed.node_at(folder)?;
            placed.is_folder(&into).then_some(())?;
            for path in paths {
                let node = placed.node_at(path)?;
                let entry = placed.tree.node(&node)?;
                // A folder's copy is a copy of everything in it: Files makes that one.
                (entry.kind == FileNodeKind::File).then_some(())?;
                let name = name_of(path)?;
                placed.free(&into, name).then_some(())?;
                // The copy names the same content: no byte is copied.
                let document_id = FileDocumentId::in_chunk(placed.tree.chunk_for_new());
                actions.push(FileAction::Create {
                    node: fs_protocol::tree::document_node_id(document_id),
                    document_id,
                    parent: into.clone(),
                    name: FileNameV1::from(name),
                    kind: FileNodeKind::File,
                    hash: entry.hash.clone(),
                    size: entry.size,
                    mtime: now,
                    author: String::new(),
                });
                changes.push(Change::Copied {
                    from: path.clone(),
                    to: join(folder, name),
                });
            }
        }
        _ => return None,
    }
    (!actions.is_empty()).then_some((actions, changes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs_protocol::tree::Placing;
    use ui::automerge::AutoCommit;

    fn tree() -> (Tree, String) {
        let mut doc = AutoCommit::new();
        let mut tree = Tree::default();
        let docs = fs_protocol::tree::place(
            (&mut doc, 1),
            &mut tree,
            ROOT_ID,
            "docs",
            Placing::Folder,
            (1, "ada"),
        )
        .unwrap();
        let hash = "a".repeat(64);
        fs_protocol::tree::place(
            (&mut doc, 1),
            &mut tree,
            ROOT_ID,
            "plan.md",
            Placing::File {
                hash: &hash,
                size: 3,
            },
            (2, "ada"),
        )
        .unwrap();
        (tree, docs.id)
    }

    /// A rename, a move and a copy in the folder are the tree's actions, a copy naming the same
    /// content; a name taken in the folder is left to Files.
    #[test]
    fn operations_in_the_folder_are_the_trees_actions() {
        let (tree, docs) = tree();
        let placed = Placed {
            tree: &tree,
            root: Path::new("/w"),
        };
        let (actions, changes) = plan(
            &FileOp::Copy {
                paths: vec!["/w/plan.md".into()],
                folder: "/w/docs".into(),
            },
            &placed,
            9,
        )
        .unwrap();
        assert!(matches!(
            &actions[..],
            [FileAction::Create { parent, hash: Some(hash), size: 3, .. }]
                if *parent == docs && *hash == "a".repeat(64)
        ));
        assert_eq!(
            changes,
            [Change::Copied {
                from: "/w/plan.md".into(),
                to: "/w/docs/plan.md".into()
            }]
        );
        let (actions, _) = plan(
            &FileOp::Rename {
                path: "/w/plan.md".into(),
                name: "final.md".into(),
            },
            &placed,
            9,
        )
        .unwrap();
        assert!(matches!(&actions[..], [FileAction::Move { parent, .. }] if parent == ROOT_ID));
        assert!(
            plan(
                &FileOp::Rename {
                    path: "/w/plan.md".into(),
                    name: "docs".into(),
                },
                &placed,
                9,
            )
            .is_none(),
            "a taken name is Files' to settle"
        );
        assert!(
            plan(
                &FileOp::Trash {
                    paths: vec!["/w/plan.md".into()]
                },
                &placed,
                9
            )
            .is_none()
        );
        assert!(
            plan(
                &FileOp::Rename {
                    path: "/elsewhere/plan.md".into(),
                    name: "x".into(),
                },
                &placed,
                9,
            )
            .is_none()
        );
    }
}
