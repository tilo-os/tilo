//! A workspace's files written into the device's one store: one resource per node, nested in the
//! folder that holds it, so a folder's listing is the index's query over its children and a move
//! is its parent field changing.
//!
//! Every change is one of the verbs
//! [`fs_protocol::tree::FilesTree`](fs_protocol::tree::FilesTree) declares — the mount, a person,
//! an agent and a tilolang app all call those same verbs — written as that verb on the node's
//! resource. A burst of copies is one [`Stamp`] with a
//! [`Group`](collab_types::stamp::Group) label and one [`Command::Batch`]: one undo step and one
//! history entry for the whole burst, where a person's single-file action is one step of its own.
//!
//! `name` is the indexed text every listing, sort, search and title reads: the name as people
//! read it, with a byte that is not UTF-8 as U+FFFD. A name that is not UTF-8 also keeps its
//! bytes exactly, in the node's content field `raw_name` ([`Edit::Set`] of the byte numbers), and
//! the fs side reads those bytes when they are there; a rename that makes the name valid again
//! removes the field. Nothing is ever escaped into `name`.
//!
//! A node's readers are empty: it reads as its parent does, and a move under a folder with other
//! readers moves its document with it.

use std::collections::BTreeMap;

use collab_types::stamp::Stamp;
use fs_protocol::file_action::FileAction;
use fs_protocol::files::{FileNameV1, FileNodeKind};
use fs_protocol::tree::{ROOT_ID, TRASH_ID, TreeEntry};
use serde_json::{Value as Json, json};
use tilo_resource_store::wire::{Command, Edit};
use tilo_resource_store::{Cell, Resource, Value};

/// The kind of a node of a workspace's tree: the system-published `files.node`, which a tilolang
/// app reads as any published type and whose verbs are the ones `fs_protocol` declares.
pub const NODE_KIND: &str = "files.node";

/// The resource every node at the top of a tree is nested in: the tree's own resource.
pub const TREE_ROOT: &str = "files:tree";

/// The resource a trashed node is nested in.
pub const TRASH_ROOT: &str = "files:trash";

/// The store resource a tree placement names: a folder's key, or the tree's root, or the Trash.
#[must_use]
pub fn parent_resource(parent: &str) -> String {
    match parent {
        ROOT_ID => TREE_ROOT.to_owned(),
        TRASH_ID => TRASH_ROOT.to_owned(),
        folder => folder.to_owned(),
    }
}

/// One node of the tree, as its verb writes it: the key that is its identity for good, where it
/// stands now, its name, and what it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// Its key: the identity the tree's document gives it, which a rename or a move never changes.
    pub key: String,
    /// Where it stands: a folder's key, or [`ROOT_ID`], or [`TRASH_ID`].
    pub parent: String,
    pub name: FileNameV1,
    pub folder: bool,
    pub hash: Option<String>,
    pub size: u64,
    pub mtime: u64,
    pub author: Option<String>,
    /// The folder it was trashed from, while it is in the Trash.
    pub trashed_from: Option<String>,
}

impl Node {
    /// The node a tree entry is, under the key it stands by.
    #[must_use]
    pub fn of(entry: &TreeEntry, key: &str) -> Self {
        Self {
            key: key.to_owned(),
            parent: entry.parent.clone().unwrap_or_else(|| ROOT_ID.to_owned()),
            name: entry.name.clone(),
            folder: matches!(entry.kind, FileNodeKind::Directory),
            hash: entry.hash.clone(),
            size: entry.size,
            mtime: entry.mtime,
            author: entry.author.clone(),
            trashed_from: None,
        }
    }

    /// Its name's bytes when the name is not UTF-8: what `raw_name` keeps, and `None` for a name
    /// `name` already holds whole.
    #[must_use]
    pub fn raw_name(&self) -> Option<&[u8]> {
        std::str::from_utf8(self.name.as_bytes())
            .is_err()
            .then_some(self.name.as_bytes())
    }

    /// Its resource as the store holds it: the indexed fields a listing reads, its parent and its
    /// order (its name, so a folder lists in name order).
    #[must_use]
    pub fn resource(&self) -> Resource {
        let mut fields: BTreeMap<String, Cell> = BTreeMap::new();
        fields.insert(
            "name".to_owned(),
            Cell::One(Value::Text(self.name.display_lossy())),
        );
        fields.insert("folder".to_owned(), Cell::One(Value::Bool(self.folder)));
        let size = i64::try_from(self.size).unwrap_or(i64::MAX);
        fields.insert("size".to_owned(), Cell::One(Value::Int(size)));
        fields.insert(
            "hash".to_owned(),
            Cell::One(self.hash.clone().map_or(Value::Null, Value::Text)),
        );
        let mtime = i64::try_from(self.mtime).unwrap_or(i64::MAX);
        fields.insert("mtime".to_owned(), Cell::One(Value::Int(mtime)));
        fields.insert(
            "author".to_owned(),
            Cell::One(self.author.clone().map_or(Value::Null, Value::Text)),
        );
        fields.insert(
            "trashed_from".to_owned(),
            Cell::One(self.trashed_from.clone().map_or(Value::Null, Value::Text)),
        );
        Resource {
            // A nested node reads as its parent does; the store takes its parent's readers.
            readers: String::new(),
            parent: Some(parent_resource(&self.parent)),
            order: Some(self.name.display_lossy()),
            fields,
        }
    }
}

/// The write one verb makes on one node, as the store carries it out: its resource, and — where
/// the verb names the node — the content field that keeps a name no text can hold.
///
/// `now` is the node as the tree reads it after the verb: the name it is called, the folder it
/// stands in, and what it holds.
#[must_use]
pub fn write(action: &FileAction, now: &Node, stamp: &Stamp) -> Option<Command> {
    if matches!(action, FileAction::ListChunk { .. }) {
        return None;
    }
    let put = Command::Put {
        kind: NODE_KIND.to_owned(),
        id: now.key.clone(),
        resource: now.resource(),
        stamp: stamp.clone(),
    };
    match raw_name_edit(action, now, stamp) {
        Some(edit) => Some(Command::Batch(vec![put, edit])),
        None => Some(put),
    }
}

/// The content field `raw_name` as the verb leaves it: the name's exact bytes where the name it
/// gives is not UTF-8, and removed where a rename makes it valid again. A write that does not
/// name the node leaves the field as it is.
fn raw_name_edit(action: &FileAction, now: &Node, stamp: &Stamp) -> Option<Command> {
    let edits = match (action, now.raw_name()) {
        // A name no text holds: its exact bytes, beside the text people read.
        (FileAction::Create { .. } | FileAction::Move { .. }, Some(bytes)) => vec![Edit::Set {
            field: "raw_name".to_owned(),
            value: Json::Array(bytes.iter().map(|byte| json!(byte)).collect()),
        }],
        // A rename to a name the text holds whole: the field it kept is gone. A create of such
        // a name never made one.
        (FileAction::Move { .. }, None) => vec![Edit::Remove {
            field: "raw_name".to_owned(),
        }],
        _ => return None,
    };
    Some(Command::Edit {
        kind: NODE_KIND.to_owned(),
        id: now.key.clone(),
        edits,
        stamp: stamp.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_resource_store::wire::Command;

    fn stamp() -> Stamp {
        Stamp {
            verb: "new-folder".to_owned(),
            invoker: "person:me".to_owned(),
            invocation: "i-1".to_owned(),
            ..Stamp::default()
        }
    }

    fn entry(name: &[u8], folder: bool) -> TreeEntry {
        TreeEntry {
            parent: Some(ROOT_ID.to_owned()),
            name: FileNameV1(name.to_vec()),
            kind: if folder {
                FileNodeKind::Directory
            } else {
                FileNodeKind::File
            },
            hash: None,
            size: 0,
            mtime: 0,
            author: None,
            document_id: None,
        }
    }

    /// The node's write in a verb's command: the `Put` itself, or the first of the batch a
    /// name's content rides with.
    fn put(command: &Command) -> (&str, &Resource, &Stamp) {
        let command = match command {
            Command::Batch(commands) => commands.first().expect("a batch holds its write"),
            other => other,
        };
        match command {
            Command::Put {
                kind,
                resource,
                stamp,
                ..
            } => (kind.as_str(), resource, stamp),
            other => panic!("a write is a Put: {other:?}"),
        }
    }

    #[test]
    fn a_folder_is_nested_in_the_tree_and_listed_by_its_name() {
        let now = Node::of(&entry(b"Books", true), "n1");
        let command = write(
            &FileAction::Create {
                node: "n1".to_owned(),
                document_id: fs_protocol::files::FileDocumentId::new(),
                parent: ROOT_ID.to_owned(),
                name: FileNameV1(b"Books".to_vec()),
                kind: FileNodeKind::Directory,
                hash: None,
                size: 0,
                mtime: 0,
                author: String::new(),
            },
            &now,
            &stamp(),
        )
        .expect("a create writes its node");
        let (kind, resource, stamp) = put(&command);
        assert_eq!(kind, NODE_KIND);
        assert_eq!(resource.parent.as_deref(), Some(TREE_ROOT));
        assert_eq!(resource.order.as_deref(), Some("Books"));
        assert!(resource.readers.is_empty(), "it reads as its parent does");
        assert_eq!(
            resource.fields.get("folder").and_then(Cell::one),
            Some(&Value::Bool(true))
        );
        assert_eq!(stamp.verb, "new-folder", "the verb names the write");
    }

    #[test]
    fn a_name_no_text_holds_keeps_its_bytes_in_the_content() {
        let raw = b"caf\xe9.txt";
        let mut now = Node::of(&entry(raw, false), "n2");
        let create = FileAction::Create {
            node: "n2".to_owned(),
            document_id: fs_protocol::files::FileDocumentId::new(),
            parent: ROOT_ID.to_owned(),
            name: FileNameV1(raw.to_vec()),
            kind: FileNodeKind::File,
            hash: None,
            size: 0,
            mtime: 0,
            author: String::new(),
        };
        let command = write(&create, &now, &stamp()).expect("a create writes its node");
        let Command::Batch(commands) = &command else {
            panic!("the exact bytes are written beside the node: {command:?}");
        };
        assert_eq!(commands.len(), 2, "the node, and its raw name");
        assert!(matches!(&commands[1], Command::Edit { edits, .. }
            if matches!(&edits[0], Edit::Set { field, value }
                if field == "raw_name" && value.as_array().map(Vec::len) == Some(raw.len()))));
        // `name` is the text people read: the byte no text holds is the replacement character.
        assert_eq!(
            now.resource().fields["name"].one().and_then(Value::as_text),
            Some("caf\u{FFFD}.txt"),
            "the text people read holds a replacement character"
        );
        // A rename to a name every text holds removes the field again.
        now.name = FileNameV1(b"cafe.txt".to_vec());
        let move_ = FileAction::Move {
            node: "n2".to_owned(),
            parent: "n9".to_owned(),
            name: FileNameV1(b"cafe.txt".to_vec()),
            mtime: 1,
        };
        let renamed = write(&move_, &now, &stamp()).expect("a move writes its node");
        let Command::Batch(commands) = &renamed else {
            panic!("the removal of the raw name rides with it: {renamed:?}");
        };
        assert!(matches!(&commands[1], Command::Edit { edits, .. }
            if matches!(&edits[0], Edit::Remove { field } if field == "raw_name")));
    }

    #[test]
    fn a_move_re_parents_the_node_and_a_trash_nests_it_in_the_trash() {
        let mut now = Node::of(&entry(b"Atlas.epub", false), "n3");
        // The node as the tree reads it after the move: in the folder it went to.
        now.parent = "n1".to_owned();
        let moved = write(
            &FileAction::Move {
                node: "n3".to_owned(),
                parent: "n1".to_owned(),
                name: FileNameV1(b"Atlas.epub".to_vec()),
                mtime: 2,
            },
            &now,
            &stamp(),
        )
        .expect("a move writes its node");
        let (_, resource, _) = put(&moved);
        assert_eq!(resource.parent.as_deref(), Some("n1"), "re-parented");
        now.parent = TRASH_ID.to_owned();
        let trashed = write(
            &FileAction::Trash {
                node: "n3".to_owned(),
            },
            &now,
            &stamp(),
        )
        .expect("a trash writes its node");
        let (_, resource, _) = put(&trashed);
        assert_eq!(resource.parent.as_deref(), Some(TRASH_ROOT));
    }

    #[test]
    fn the_chunk_bookkeeping_is_no_write_of_a_node() {
        assert!(
            write(
                &FileAction::ListChunk { chunk: 1 },
                &Node::of(&entry(b"x", false), "n4"),
                &stamp()
            )
            .is_none()
        );
    }
}
