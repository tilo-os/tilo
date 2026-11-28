//! What a workspace's files take: the actions of `files.tree`.
//!
//! Every change of a file or folder is one of these, applied to the document of the chunk the
//! node lives in ([`crate::files::FileDocumentId::chunk`]): the mount writes them as a program
//! on the device changes its files, a holder of the tree writes them for an agent's call, and
//! a page writes them as a person puts files in. A change of the tree then names what it did
//! and who did it, as every other resource's change does, so grants decide it, History shows
//! it under its author, and Undo takes it back.
//!
//! An action names its node by key, never by path: a path is where the node is now, and a
//! concurrent move on another device would make it name something else.

use automerge::transaction::Transactable as _;
use automerge::{ObjId, ObjType, ReadDoc as _, ScalarValue};
use serde::{Deserialize, Serialize};
use tilo_actions::{ActionError, ActionKind};
use tilo_automerge::read::ReadTyped as _;
use tilo_resource::Doc;

use crate::files::{FileDocumentId, FileNameV1, FileNodeKind, TreeChunk};
use crate::tree::{
    AUTHOR_KEY, DOCUMENT_ID_KEY, HASH_KEY, KIND_KEY, MTIME_KEY, NAME_KEY, PARENT_KEY,
    PLACEMENT_NAME_KEY, PREV_PARENT_KEY, ROOT_ID, SIZE_KEY, TRASH_ID, tree_nodes,
};

/// The registry type every action of the tree is recorded and replayed under.
pub const FILE_ACTION_TYPE: &str = crate::tree::TREE_KIND;

/// The key a preview of a file's content is kept under: what a new version of its content
/// makes stale.
pub const THUMB_KEY: &str = "thumb";

/// The content hash a file had when it went to the trash, so a concurrent edit of it is seen.
pub const DELETED_HASH_KEY: &str = "deleted_hash";

/// One change of a workspace's files. See the module docs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
pub enum FileAction {
    /// Make the node `node` for the document `document_id`, named `name` in the folder `parent`.
    Create {
        node: String,
        document_id: FileDocumentId,
        parent: String,
        name: FileNameV1,
        kind: FileNodeKind,
        /// The hex digest of its content's manifest; none for a folder or an empty file.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hash: Option<String>,
        size: u64,
        mtime: u64,
        /// The name of whoever made it, as the device that wrote it knows them.
        author: String,
    },
    /// Give the file `node` new content: `hash` names its manifest, none for no bytes.
    Write {
        node: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hash: Option<String>,
        size: u64,
        mtime: u64,
        author: String,
    },
    /// Put `node` into the folder `parent` under `name`: a rename, a move, or both.
    Move {
        node: String,
        parent: String,
        name: FileNameV1,
        mtime: u64,
    },
    /// Put `node` in the trash, with what it holds.
    Trash { node: String },
    /// Take `node` out of the trash: into `parent`, else back into the folder it was in.
    /// Whoever holds the whole tree names `parent` when that folder is gone too, which one
    /// chunk's document cannot see.
    Restore {
        node: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
    },
    /// List the chunk `chunk` in the tree's own document (chunk 0), so everyone who opens the
    /// tree opens it too: what a writer does before it makes the first node in a chunk nobody
    /// listed yet.
    ListChunk { chunk: TreeChunk },
}

impl FileAction {
    /// The node it changes.
    #[must_use]
    pub fn node(&self) -> &str {
        match self {
            Self::Create { node, .. }
            | Self::Write { node, .. }
            | Self::Move { node, .. }
            | Self::Trash { node }
            | Self::Restore { node, .. } => node,
            Self::ListChunk { .. } => ROOT_ID,
        }
    }

    /// Its verb, as grants and History name it.
    #[must_use]
    pub const fn verb(&self) -> &'static str {
        match self {
            Self::Create {
                kind: FileNodeKind::Directory,
                ..
            } => verbs::NEW_FOLDER,
            Self::Create { .. } => verbs::NEW_FILE,
            Self::Write { .. } => verbs::WRITE,
            Self::Move { .. } => verbs::MOVE,
            Self::Trash { .. } => verbs::TRASH,
            Self::Restore { .. } => verbs::RESTORE,
            Self::ListChunk { .. } => verbs::LIST_CHUNK,
        }
    }

    /// Write it into `doc`, the document of the chunk its node lives in. The namespace a mount
    /// keeps writes through this too, so a change made on a disk and one made by a call are
    /// the same change.
    ///
    /// # Errors
    /// A node the document does not hold (for anything but [`Self::Create`]), a name no file
    /// may have, or a document that refused the write.
    pub fn write(&self, doc: &mut Doc) -> Result<(), ActionError> {
        if let Self::ListChunk { chunk } = self {
            crate::tree::list_chunk(doc, *chunk)?;
            return Ok(());
        }
        let nodes = tree_nodes(doc)?;
        match self {
            Self::Create { .. } => self.write_create(doc, &nodes),
            Self::Write { .. } => self.write_content(doc, &nodes),
            Self::Move {
                node,
                parent,
                name,
                mtime,
            } => {
                check_name(name)?;
                if parent == node {
                    return Err(ActionError::Refused("a folder cannot hold itself".into()));
                }
                let obj = node_obj(doc, &nodes, node)?;
                let current = doc
                    .str_at(&obj, PARENT_KEY)
                    .unwrap_or_else(|| ROOT_ID.to_owned());
                doc.put(&obj, PREV_PARENT_KEY, current)?;
                doc.put(&obj, PARENT_KEY, parent.as_str())?;
                doc.put(&obj, PLACEMENT_NAME_KEY, name.0.clone())?;
                doc.put(&obj, NAME_KEY, name.0.clone())?;
                doc.put(&obj, MTIME_KEY, *mtime)?;
                Ok(())
            }
            Self::Trash { .. } | Self::Restore { .. } => self.write_trash(doc, &nodes),
            Self::ListChunk { .. } => Ok(()),
        }
    }

    /// [`Self::write`] of a [`Self::Create`].
    fn write_create(&self, doc: &mut Doc, nodes: &ObjId) -> Result<(), ActionError> {
        let Self::Create {
            node,
            document_id,
            parent,
            name,
            kind,
            hash,
            size,
            mtime,
            author,
        } = self
        else {
            return Ok(());
        };
        check_name(name)?;
        // The node and its fields go into the document as one object: a put per field cost
        // a file ten times what making the whole node at once does, and a folder of ten
        // thousand files spent over a second on them.
        let field = |value: ScalarValue| automerge::hydrate::Value::Scalar(value);
        let mut fields = std::collections::HashMap::from([
            (PARENT_KEY, field(parent.as_str().into())),
            (PREV_PARENT_KEY, field(parent.as_str().into())),
            (NAME_KEY, field(name.0.clone().into())),
            (PLACEMENT_NAME_KEY, field(name.0.clone().into())),
            (
                KIND_KEY,
                field(
                    match kind {
                        FileNodeKind::Directory => "dir",
                        FileNodeKind::File => "file",
                    }
                    .into(),
                ),
            ),
            (SIZE_KEY, field((*size).into())),
            (MTIME_KEY, field((*mtime).into())),
            (AUTHOR_KEY, field(author.as_str().into())),
            (DOCUMENT_ID_KEY, field(document_id.0.to_string().into())),
        ]);
        if let Some(hash) = hash {
            fields.insert(HASH_KEY, field(hash.as_str().into()));
        }
        doc.batch_create_object(
            nodes,
            node.as_str(),
            &automerge::hydrate::Value::Map(fields.into()),
            false,
        )?;
        Ok(())
    }

    /// [`Self::write`] of a [`Self::Write`].
    fn write_content(&self, doc: &mut Doc, nodes: &ObjId) -> Result<(), ActionError> {
        let Self::Write {
            node,
            hash,
            size,
            mtime,
            author,
        } = self
        else {
            return Ok(());
        };
        let obj = node_obj(doc, nodes, node)?;
        match hash {
            Some(hash) => doc.put(&obj, HASH_KEY, hash.as_str())?,
            None => clear(doc, &obj, HASH_KEY)?,
        }
        clear(doc, &obj, THUMB_KEY)?;
        doc.put(&obj, SIZE_KEY, *size)?;
        doc.put(&obj, MTIME_KEY, *mtime)?;
        doc.put(&obj, AUTHOR_KEY, author.as_str())?;
        Ok(())
    }

    /// [`Self::write`] of a [`Self::Trash`] or a [`Self::Restore`].
    fn write_trash(&self, doc: &mut Doc, nodes: &ObjId) -> Result<(), ActionError> {
        match self {
            Self::Trash { node } => {
                let obj = node_obj(doc, nodes, node)?;
                match doc.str_at(&obj, HASH_KEY) {
                    Some(hash) => doc.put(&obj, DELETED_HASH_KEY, hash)?,
                    None => clear(doc, &obj, DELETED_HASH_KEY)?,
                }
                let current = doc
                    .str_at(&obj, PARENT_KEY)
                    .unwrap_or_else(|| ROOT_ID.to_owned());
                doc.put(&obj, PREV_PARENT_KEY, current)?;
                doc.put(&obj, PARENT_KEY, TRASH_ID)?;
            }
            Self::Restore { node, parent } => {
                let obj = node_obj(doc, nodes, node)?;
                if doc.str_at(&obj, PARENT_KEY).as_deref() != Some(TRASH_ID) {
                    return Err(ActionError::Refused("it is not in the trash".into()));
                }
                let back = parent
                    .clone()
                    .or_else(|| doc.str_at(&obj, PREV_PARENT_KEY))
                    .filter(|parent| parent != TRASH_ID)
                    .unwrap_or_else(|| ROOT_ID.to_owned());
                doc.put(&obj, PARENT_KEY, back)?;
            }
            _ => {}
        }
        Ok(())
    }
}

impl tilo_actions::Action for FileAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        self.write(doc)
    }

    /// Putting a file in the trash keeps it there to take back: a step like any other.
    fn action_kind(&self) -> ActionKind {
        ActionKind::Undoable
    }

    /// Its verb, on its node: what grants decide and History shows, whichever way it came.
    fn describe(&self, _doc: &Doc) -> Option<tilo_actions::Described> {
        Some(self.described())
    }
}

impl FileAction {
    /// What it does, as a change names it: its verb, on its node.
    #[must_use]
    pub fn described(&self) -> tilo_actions::Described {
        tilo_actions::Described::new(did(self.verb())).on(self.node(), self.node())
    }
}

/// The tree's verb `verb`, as a change names what it did.
#[must_use]
pub fn did(verb: &'static str) -> tilo_actions::Did {
    tilo_actions::Did::verb(
        tilo_actions::ActionKey::new(crate::tree::FILES_APP, crate::tree::TREE_KIND, verb),
        [],
    )
}

/// The entry a change of the tree's document `document` is written with: what its file
/// actions did (one, or a group of them), and who did them (`None`: the person at the device
/// that wrote it). Every writer of the tree names its changes with this, the mount as an
/// agent's call, so History reads one kind of entry and Undo takes back one kind of step.
#[must_use]
pub fn change_context(
    document: &str,
    did: tilo_actions::Described,
    invoker: Option<tilo_actions::ActionInvoker>,
) -> tilo_actions::ActionContext {
    let context =
        tilo_actions::ActionContext::for_resource(document, crate::tree::TREE_KIND, "Files")
            .with_app(crate::tree::FILES_APP, "Files")
            .with_described(did);
    match invoker {
        Some(invoker) => context.with_invoker(invoker),
        None => context,
    }
}

/// The workspace's tree as a node below the workspace: `files › files.tree › <tree document>`,
/// every chunk decided as the document it splits, as a receiver of a change of it names it.
#[must_use]
pub fn tree_node() -> tilo_actions::policy::TreePath {
    tilo_actions::policy::TreePath::root()
        .app(crate::tree::FILES_APP)
        .kind(crate::tree::TREE_KIND)
        .resource(collab_types::receipt::published_resource(
            &crate::tree::document_id(),
        ))
}

/// The scope a share of the tree's node `node` is given at in `workspace`: the tree's own node,
/// per item, so a person let into one file reads it and nothing beside it.
#[must_use]
pub fn node_scope(workspace: &str, node: &str) -> tilo_actions::policy::TreePath {
    tilo_actions::policy::TreePath::workspace(workspace)
        .joined(&tree_node())
        .item(node)
}

/// The tree node `scope` is a share of, when it is one of a workspace's tree's nodes.
#[must_use]
pub fn node_of_scope(scope: &tilo_actions::policy::TreePath) -> Option<&str> {
    use tilo_actions::policy::Level;
    let (workspace, rest) = scope.segments().split_first()?;
    if workspace.level() != Some(Level::Workspace) {
        return None;
    }
    let tree = tree_node();
    let named = tree.segments();
    if rest.len() != named.len().saturating_add(1) {
        return None;
    }
    let (head, item) = rest.split_at(named.len());
    if head != named {
        return None;
    }
    let item = item.first()?;
    if item.level() != Some(Level::Item) {
        return None;
    }
    item.id()
}

/// The node below the workspace the file action `verb` on the tree's node `node` is decided
/// at, or on the whole tree with no node named. It is the node a receiver of the change
/// decides it at (`collab_types::receipt::claim_node`), so a mount writing a file, a call
/// writing one where the tree is kept and every receiver of either answer at one node.
#[must_use]
pub fn verb_node(node: Option<&str>, verb: &str) -> tilo_actions::policy::TreePath {
    let tree = tree_node();
    match node {
        Some(node) => tree.item(node),
        None => tree,
    }
    .action(verb)
}

/// The verbs of the tree, as grants and History name them.
pub mod verbs {
    // The verbs a `files#open` given content takes, named once where a call naming that open is
    // checked as asking for them.
    pub const NEW_FOLDER: &str = collab_types::FILES_OPEN_VERBS[0];
    pub const NEW_FILE: &str = collab_types::FILES_OPEN_VERBS[1];
    pub const WRITE: &str = collab_types::FILES_OPEN_VERBS[2];
    pub const MOVE: &str = "move";
    pub const TRASH: &str = "trash";
    pub const RESTORE: &str = "restore";
    pub const LIST_CHUNK: &str = "list-chunk";
    /// Every verb, in the order the surface lists them.
    pub const ALL: [&str; 7] = [
        NEW_FOLDER, NEW_FILE, WRITE, MOVE, TRASH, RESTORE, LIST_CHUNK,
    ];
}

fn check_name(name: &FileNameV1) -> Result<(), ActionError> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes == b"."
        || bytes == b".."
        || bytes.contains(&b'/')
        || bytes.contains(&0)
        || bytes.len() > crate::files::MAX_FILE_NAME_BYTES
    {
        return Err(ActionError::Refused(format!(
            "{:?} is no file's name",
            String::from_utf8_lossy(bytes)
        )));
    }
    Ok(())
}

/// Delete `key` of `obj` when it holds one: a key that was never written is left alone, so
/// the change carries no operation for it.
fn clear(doc: &mut Doc, obj: &ObjId, key: &str) -> Result<(), ActionError> {
    if doc.get(obj, key)?.is_some() {
        doc.delete(obj, key)?;
    }
    Ok(())
}

fn node_obj(doc: &Doc, nodes: &ObjId, node: &str) -> Result<ObjId, ActionError> {
    doc.obj_at(nodes, node, ObjType::Map)
        .ok_or_else(|| ActionError::Refused(format!("the tree holds no node {node}")))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tilo_actions::ActionArgs;
    use tilo_actions::{ActionRegistry, Did, Verified};
    use tilo_resource::{Replica, Resource as _};

    use super::*;
    use crate::tree::{FilesTree, Placing, Tree, place};

    /// The action the declaration `id` builds from `args`, as the API builds it for a caller.
    fn declared(id: &str, args: &serde_json::Value) -> FileAction {
        let declaration = FilesTree::declarations()
            .into_iter()
            .find(|declaration| declaration.id == id)
            .unwrap();
        declaration
            .build_action(
                &ActionArgs::bind_json(&declaration.params, args.as_object().unwrap()).unwrap(),
                7,
            )
            .unwrap()
    }

    /// A tree holding the folder `docs` with the file `plan.md` in it, as a page puts them in.
    fn tree_with_a_file() -> (Doc, String, String) {
        let mut doc = Doc::new();
        let mut tree = Tree::default();
        let docs = place(
            (&mut doc, 1),
            &mut tree,
            ROOT_ID,
            "docs",
            Placing::Folder,
            (5, "page"),
        )
        .unwrap();
        let hash = "ab".repeat(32);
        let plan = place(
            (&mut doc, 1),
            &mut tree,
            &docs.id,
            "plan.md",
            Placing::File {
                hash: &hash,
                size: 12,
            },
            (6, "page"),
        )
        .unwrap();
        doc.commit();
        (doc, docs.id, plan.id)
    }

    #[test]
    fn an_agent_moves_a_file_by_its_key_and_the_change_names_the_verb_and_the_agent() {
        let (doc, _, plan) = tree_with_a_file();
        let mut replica = Replica::<FilesTree>::from_doc(doc)
            .in_app("so.tilo.model", "Model")
            .as_agent_for("helper", "ada");
        let moved = declared(
            verbs::MOVE,
            &json!({"node": plan, "parent": ROOT_ID, "name": "final.md"}),
        );
        replica.apply_action(&moved).unwrap();
        let state = replica.state().unwrap();
        let node = state.node(&plan).unwrap();
        assert_eq!(node.name, "final.md");
        assert_eq!(node.parent.as_deref(), Some(ROOT_ID));

        let last = replica.versions().pop().unwrap();
        let context = last.context.expect("the change names what it did");
        let Some(Did::Verb(verb)) = &context.what else {
            panic!("a verb, not {:?}", context.what);
        };
        assert_eq!(verb.action.action, verbs::MOVE);
        let invoker = context.invoker.expect("the change names who did it");
        assert!(
            format!("{:?}", invoker.acting).contains("helper"),
            "the agent is the actor: {invoker:?}"
        );

        // A device that receives the change replays the action and gets the same change.
        let registry = ActionRegistry::with(tilo_resource::group::register::<FilesTree>);
        let mut peer = replica.doc().clone();
        assert_eq!(
            tilo_actions::verify_change(&mut peer, last.hash, &registry).unwrap(),
            Verified::Reproduced
        );
    }

    #[test]
    fn trash_and_restore_put_a_folder_back_where_it_was() {
        let (doc, docs, plan) = tree_with_a_file();
        let mut replica = Replica::<FilesTree>::from_doc(doc)
            .in_app("so.tilo.model", "Model")
            .as_author("ada");
        replica
            .apply_action(&declared(verbs::TRASH, &json!({"node": docs})))
            .unwrap();
        let state = replica.state().unwrap();
        assert!(state.trashed(&docs));
        assert!(!crate::tree::reaches_root(&state, &plan));
        let restore = declared(verbs::RESTORE, &json!({"node": docs}));
        replica.apply_action(&restore).unwrap();
        let state = replica.state().unwrap();
        assert_eq!(state.node(&docs).unwrap().parent.as_deref(), Some(ROOT_ID));
        assert!(crate::tree::reaches_root(&state, &plan));
        // A node out of the trash is not restored again.
        assert!(replica.apply_action(&restore).is_err());
    }

    #[test]
    fn a_name_no_file_may_have_and_a_node_the_tree_lacks_are_refused() {
        let (mut doc, _, plan) = tree_with_a_file();
        for name in ["", "..", "a/b"] {
            let moved = FileAction::Move {
                node: plan.clone(),
                parent: ROOT_ID.into(),
                name: FileNameV1::from(name),
                mtime: 1,
            };
            assert!(moved.write(&mut doc).is_err(), "{name:?}");
        }
        assert!(
            FileAction::Trash {
                node: "missing".into()
            }
            .write(&mut doc)
            .is_err()
        );
        let read = FilesTree::read(&doc).unwrap();
        assert_eq!(read.node(&plan).unwrap().name, "plan.md");
    }
}
