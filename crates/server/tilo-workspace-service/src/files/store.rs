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
//! A node's readers are every member of the workspace: the tree is the workspace's files, and
//! the store's reader rule (a nested resource taking its parent's) is not here yet, so writing
//! no set would place the node in a document no one may read. Once that rule lands, a node keeps
//! no set of its own and takes its folder's.

use std::collections::{BTreeMap, BTreeSet};

use collab_types::stamp::Stamp;
use fs_protocol::file_action::FileAction;
use fs_protocol::files::{FileNameV1, FileNodeKind};
use fs_protocol::tree::{ROOT_ID, TRASH_ID, TreeEntry};
use serde_json::{Value as Json, json};
use tilo_resource_store::wire::{Command, Edit};
use tilo_resource_store::{
    Cell, Expected, Field, FieldKind, Resource, Schema, Type, Value, Verb, Writes,
};

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
            // The workspace's tree is every member's to read; a move stays in a document every
            // member reads. A folder's own readers come with the store's reader rule.
            readers: collab_types::EVERY_MEMBER.to_owned(),
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

/// The node's place as the tree names it: a folder's key, [`ROOT_ID`] or [`TRASH_ID`], from the
/// store resource's parent.
#[must_use]
pub fn tree_parent(parent: Option<&str>) -> String {
    match parent {
        Some(TREE_ROOT) | None => ROOT_ID.to_owned(),
        Some(TRASH_ROOT) => TRASH_ID.to_owned(),
        Some(folder) => folder.to_owned(),
    }
}

/// The node row a page's indexed resource is, as the fs side reads it.
#[must_use]
pub fn row(
    key: &str,
    held: &tilo_resource_store::Indexed,
    raw_name: Option<Vec<u8>>,
) -> tilo_fs::files_sync::NodeRow {
    let name = held
        .order
        .clone()
        .or_else(|| as_text(held.scalar("name")))
        .unwrap_or_default();
    tilo_fs::files_sync::NodeRow {
        key: key.to_owned(),
        parent: tree_parent(held.parent.as_deref()),
        name,
        raw_name,
        folder: matches!(held.scalar("folder"), Value::Bool(true)),
        hash: as_text(held.scalar("hash")),
        size: as_u64(held.scalar("size")),
        mtime: as_u64(held.scalar("mtime")),
        author: as_text(held.scalar("author")),
        trashed_from: as_text(held.scalar("trashed_from")),
    }
}

fn as_text(value: Value) -> Option<String> {
    value.as_text().map(str::to_owned)
}

fn as_u64(value: Value) -> u64 {
    match value {
        Value::Int(number) => u64::try_from(number).unwrap_or(0),
        _ => 0,
    }
}

/// The node a verb leaves the store holding, from the action and the node as the store has it
/// now: `None` for a verb that names no node the store translates, or whose node is gone.
#[must_use]
pub fn node_after(
    action: &FileAction,
    before: Option<&tilo_resource_store::Indexed>,
) -> Option<Node> {
    let prior = || before;
    let name = || {
        prior()
            .and_then(|held| held.order.clone())
            .map(|order| FileNameV1(order.into_bytes()))
    };
    match action {
        FileAction::Create {
            node,
            parent,
            name,
            kind,
            hash,
            size,
            mtime,
            author,
            ..
        } => Some(Node {
            key: node.clone(),
            parent: parent.clone(),
            name: name.clone(),
            folder: matches!(kind, FileNodeKind::Directory),
            hash: hash.clone(),
            size: *size,
            mtime: *mtime,
            author: (!author.is_empty()).then(|| author.clone()),
            trashed_from: None,
        }),
        FileAction::Write {
            node,
            hash,
            size,
            mtime,
            author,
        } => {
            let held = prior()?;
            Some(Node {
                key: node.clone(),
                parent: tree_parent(held.parent.as_deref()),
                name: name()?,
                folder: matches!(held.scalar("folder"), Value::Bool(true)),
                hash: hash.clone(),
                size: *size,
                mtime: *mtime,
                author: (!author.is_empty()).then(|| author.clone()),
                trashed_from: as_text(held.scalar("trashed_from")),
            })
        }
        FileAction::Move {
            node,
            parent,
            name,
            mtime,
        } => {
            let held = prior()?;
            Some(Node {
                key: node.clone(),
                parent: parent.clone(),
                name: name.clone(),
                folder: matches!(held.scalar("folder"), Value::Bool(true)),
                hash: as_text(held.scalar("hash")),
                size: as_u64(held.scalar("size")),
                mtime: *mtime,
                author: as_text(held.scalar("author")),
                trashed_from: as_text(held.scalar("trashed_from")),
            })
        }
        FileAction::Trash { node } => {
            let held = prior()?;
            let parent = tree_parent(held.parent.as_deref());
            Some(Node {
                key: node.clone(),
                parent: TRASH_ID.to_owned(),
                name: name()?,
                folder: matches!(held.scalar("folder"), Value::Bool(true)),
                hash: as_text(held.scalar("hash")),
                size: as_u64(held.scalar("size")),
                mtime: as_u64(held.scalar("mtime")),
                author: as_text(held.scalar("author")),
                trashed_from: (parent != TRASH_ID).then_some(parent),
            })
        }
        FileAction::Restore { node, parent } => {
            let held = prior()?;
            let restored = parent
                .clone()
                .or_else(|| as_text(held.scalar("trashed_from")))
                .unwrap_or_else(|| ROOT_ID.to_owned());
            Some(Node {
                key: node.clone(),
                parent: restored,
                name: name()?,
                folder: matches!(held.scalar("folder"), Value::Bool(true)),
                hash: as_text(held.scalar("hash")),
                size: as_u64(held.scalar("size")),
                mtime: as_u64(held.scalar("mtime")),
                author: as_text(held.scalar("author")),
                trashed_from: None,
            })
        }
        FileAction::ListChunk { .. } => None,
    }
}

/// The schema the system publishes for a workspace's tree: a node is a `files.node` nested in a
/// `files.node`, and each verb of [`fs_protocol`](fs_protocol::file_action::verbs) writes its
/// fields. Every store that judges a tree's writes declares it, so a device, an agent and a
/// tilolang app all write the same type without an app declaring it.
#[must_use]
pub fn schema() -> Schema {
    let fields: Vec<Field> = [
        ("name", Expected::Text, false),
        ("folder", Expected::Bool, false),
        ("size", Expected::Int, false),
        ("hash", Expected::Text, true),
        ("mtime", Expected::Int, false),
        ("author", Expected::Text, true),
        ("trashed_from", Expected::Text, true),
    ]
    .into_iter()
    .map(|(name, value, optional)| Field {
        name: name.to_owned(),
        kind: FieldKind::Scalar,
        value,
        optional,
    })
    .collect();
    let all: BTreeSet<String> = fields.iter().map(|field| field.name.clone()).collect();
    let writes = BTreeMap::from([(
        NODE_KIND.to_owned(),
        Writes {
            fields: all,
            sets_only: false,
        },
    )]);
    Schema {
        types: vec![Type {
            name: NODE_KIND.to_owned(),
            fields,
            nested: vec![("children".to_owned(), NODE_KIND.to_owned())],
        }],
        verbs: fs_protocol::file_action::verbs::ALL
            .iter()
            .filter(|verb| **verb != fs_protocol::file_action::verbs::LIST_CHUNK)
            .map(|verb| Verb {
                name: (*verb).to_owned(),
                writes: writes.clone(),
                nested: BTreeSet::from([NODE_KIND.to_owned()]),
            })
            .collect(),
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
        assert_eq!(resource.readers, collab_types::EVERY_MEMBER);
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

    /// A store holding the tree, with the system's node type declared: what a device's workspace
    /// service is when it writes files.
    fn tree_store() -> tilo_resource_store::Store {
        let mut store = tilo_resource_store::Store::new();
        store.declare(schema());
        store
    }

    /// The writes `count` files named `prefix` under `parent` make, as one batch.
    fn a_copy_of(count: usize, parent: &str, prefix: &str) -> Command {
        let stamp = stamp();
        let mut commands = Vec::with_capacity(count);
        for n in 0..count {
            let name = format!("{prefix}{n}.txt");
            let entry = TreeEntry {
                parent: Some(parent.to_owned()),
                name: FileNameV1(name.clone().into_bytes()),
                kind: FileNodeKind::File,
                hash: None,
                size: u64::try_from(n).unwrap_or(u64::MAX),
                mtime: 1,
                author: Some("person:me".to_owned()),
                document_id: None,
            };
            let now = Node::of(&entry, &format!("{prefix}-{n}"));
            let action = FileAction::Create {
                node: now.key.clone(),
                document_id: fs_protocol::files::FileDocumentId::new(),
                parent: parent.to_owned(),
                name: FileNameV1(name.into_bytes()),
                kind: FileNodeKind::File,
                hash: None,
                size: now.size,
                mtime: 1,
                author: "person:me".to_owned(),
            };
            commands.push(write(&action, &now, &stamp).expect("a create writes its node"));
        }
        Command::Batch(commands)
    }

    /// The page of `parent`'s children, as a folder listing asks for it.
    fn page(parent: &str, limit: usize) -> tilo_resource_store::Query {
        use tilo_resource_store::Filter;
        tilo_resource_store::Query {
            kind: NODE_KIND.to_owned(),
            filters: vec![Filter::Is(
                tilo_resource_store::PARENT.to_owned(),
                tilo_resource_store::Value::Text(parent_resource(parent)),
            )],
            order: tilo_resource_store::Order::default(),
            after: None,
            limit,
        }
    }

    /// A folder of ten thousand files opens in under the walk's budget, and a page far down it
    /// reads in a scroll's.
    #[test]
    fn a_folder_of_ten_thousand_opens_and_scrolls_in_time() {
        /// A folder's first page must open in this long.
        const OPEN: std::time::Duration = std::time::Duration::from_millis(289);
        /// A page far down a folder must read in this long.
        const SCROLL: std::time::Duration = std::time::Duration::from_millis(25);
        let mut store = tree_store();
        let batch = a_copy_of(10_000, ROOT_ID, "file-");
        tilo_resource_store::wire::write(&mut store, batch).expect("the copy is written");
        let started = std::time::Instant::now();
        let first = store.query(&page(ROOT_ID, 10_000), &|_| true);
        let open = started.elapsed();
        assert_eq!(first.resources.len(), 10_000);
        assert!(open <= OPEN, "ten thousand children opened in {open:?}");
        // The first half's last cursor starts a page a reader far down the list shows.
        let half = store.query(&page(ROOT_ID, 5_000), &|_| true);
        let mut below = page(ROOT_ID, 50);
        below.after = half.next;
        let started = std::time::Instant::now();
        let shown = store.query(&below, &|_| true);
        let scroll = started.elapsed();
        assert_eq!(shown.resources.len(), 50);
        assert!(scroll <= SCROLL, "a page far down read in {scroll:?}");
    }

    /// A copy of ten thousand files writes faster than a thousand nodes a second: the rate a
    /// workspace's files are put at, not a wall clock a shared machine can spoil.
    #[test]
    fn a_copy_of_ten_thousand_files_writes_over_a_thousand_a_second() {
        let mut store = tree_store();
        let batch = a_copy_of(10_000, ROOT_ID, "file-");
        let started = std::time::Instant::now();
        let written = tilo_resource_store::wire::write(&mut store, batch).expect("the copy");
        let took = started.elapsed();
        assert!(!written.is_empty());
        let per_second = 10_000.0 / took.as_secs_f64().max(f64::MIN_POSITIVE);
        assert!(
            per_second >= 1_000.0,
            "a copy wrote {per_second:.0} nodes a second ({took:?})"
        );
        assert_eq!(store.count(NODE_KIND, &[], &|_| true), 10_000);
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
