//! A workspace folder's own history: the changes of the tree its files are, as the rows
//! History lists them.
//!
//! Every change of a file or folder is one of the tree's actions ([`fs_protocol::FileAction`]),
//! written into the document of the chunk the node lives in and named as every resource's
//! change is: what it did, on which node, and who did it (the person, or the agent session
//! that wrote for them). A folder's History reads those changes the way any resource's
//! History reads its document's ([`super::HistorySource`]), one chunk's document at a time,
//! and names each node by the path the whole tree gives it now. A file's History is the
//! changes whose item is its node.

use std::collections::BTreeSet;

use automerge::{AutoCommit, ChangeHash};
use fs_protocol::files::{FileDocumentId, TreeChunk};

use crate::history_source::{AuthorDirectory, HistorySource};
use crate::widget::history::HistoryEntry;

/// How many of a folder's changes [`entries`] lists at most: the newest. A folder can
/// hold more changes than one history pane shows, and the pane is what bounds it.
pub const LIMIT: usize = 500;

/// The documents a folder's changes are read from: the workspace's tree, its own document
/// (chunk 0) and each chunk of it.
pub trait FolderChunks {
    /// The whole tree as its documents hold it, every chunk's nodes together: what names
    /// each file and walks its path. `None` when the tree cannot be read now.
    fn tree(&self) -> Option<fs_protocol::tree::Tree>;

    /// The chunks whose documents hold the nodes of `tree`: each node lives in the chunk its
    /// document id names ([`FileDocumentId::chunk`]).
    fn chunks(&self, tree: &fs_protocol::tree::Tree) -> BTreeSet<TreeChunk> {
        tree.nodes
            .values()
            .filter_map(|node| node.document_id)
            .map(|document| document.chunk())
            .collect()
    }

    /// Read the document of chunk `chunk`. `None` when it cannot be read now.
    fn read_chunk<T>(&self, chunk: TreeChunk, read: impl FnOnce(&AutoCommit) -> T) -> Option<T>;
}

/// One change a folder's History lists, with the document it is kept in: what an undo or a
/// restore of it is applied to.
#[derive(Clone, Debug)]
pub struct FolderChange {
    /// The chunk whose document holds the change.
    pub chunk: TreeChunk,
    /// The change itself.
    pub hash: ChangeHash,
    /// The node it acted on, when it acted on one.
    pub node: Option<String>,
    /// Its row.
    pub entry: HistoryEntry,
}

/// Every change `folder` holds, oldest first, the newest [`LIMIT`] of them: each
/// node named by its path, and authored by whoever the change names.
#[must_use]
pub fn entries<R: FolderChunks>(folder: &R, authors: &AuthorDirectory) -> Vec<HistoryEntry> {
    changes(folder, authors, None)
        .into_iter()
        .map(|change| change.entry)
        .collect()
}

/// The changes `folder` holds, oldest first, the newest [`LIMIT`] of them; only
/// those of the file whose document id is `file`, when one is given.
#[must_use]
pub fn changes<R: FolderChunks>(
    folder: &R,
    authors: &AuthorDirectory,
    file: Option<FileDocumentId>,
) -> Vec<FolderChange> {
    let Some(tree) = folder.tree() else {
        return Vec::new();
    };
    let focus = match file {
        Some(file) => match tree
            .nodes
            .iter()
            .find(|(_, node)| node.document_id == Some(file))
        {
            Some((id, _)) => Some(id.clone()),
            None => return Vec::new(),
        },
        None => None,
    };
    let chunks = match file {
        Some(file) => BTreeSet::from([file.chunk()]),
        None => folder.chunks(&tree),
    };
    let source = HistorySource::new()
        .with_authors(authors.clone())
        .with_max_entries(LIMIT)
        .without_resource_genesis();
    let mut listed = Vec::new();
    for chunk in chunks {
        let Some(rows) = folder.read_chunk(chunk, |doc| chunk_changes(&source, doc)) else {
            continue;
        };
        for (hash, entry) in rows {
            let node = entry
                .context
                .as_ref()
                .and_then(|context| context.item.as_ref())
                .map(|item| item.id.clone());
            if focus.is_some() && node != focus {
                continue;
            }
            listed.push(FolderChange {
                chunk,
                hash,
                entry: named_by_path(entry, &tree),
                node,
            });
        }
    }
    listed.sort_by_key(|change| (change.entry.timestamp, change.entry.id.0));
    if listed.len() > LIMIT {
        listed.drain(..listed.len().saturating_sub(LIMIT));
    }
    listed
}

/// The rows of one chunk's document, each with the change it is.
fn chunk_changes(source: &HistorySource, doc: &AutoCommit) -> Vec<(ChangeHash, HistoryEntry)> {
    let mut doc = doc.clone();
    let rows = source.entries(&mut doc);
    let hashes: std::collections::HashMap<crate::HistoryEntryId, ChangeHash> = doc
        .get_changes_meta(&[])
        .into_iter()
        .map(|change| {
            (
                super::entry_id_from_hash(&change.hash.to_string()),
                change.hash,
            )
        })
        .collect();
    rows.into_iter()
        .filter_map(|row| Some((*hashes.get(&row.id)?, row)))
        .collect()
}

/// `entry`, the node it acted on named by the path `tree` gives it now rather than by its key.
fn named_by_path(mut entry: HistoryEntry, tree: &fs_protocol::tree::Tree) -> HistoryEntry {
    let Some(context) = entry.context.as_mut() else {
        return entry;
    };
    let Some(item) = context.item.as_mut() else {
        return entry;
    };
    if tree.node(&item.id).is_none() {
        return entry;
    }
    let path = fs_protocol::tree::path_of(tree, &item.id);
    item.label.clone_from(&path);
    let summary = file_sentence(context, &path).or_else(|| super::sentence::step_summary(context));
    if let Some(summary) = summary {
        entry.target = super::sentence::step_target(context, &summary);
        entry.summary = summary;
    }
    entry
}

/// What a change of one file did, as History says it ("Edited docs/plan.md"): the verb the
/// change's file actions share, or the one that says the most of a file made, deleted or put
/// back in the same change. `None` for a change that is not of the tree.
fn file_sentence(context: &tilo_actions::ActionContext, path: &str) -> Option<String> {
    let tree_verb = |did: &tilo_actions::Did| match did {
        tilo_actions::Did::Verb(verb) if verb.action.kind == fs_protocol::tree::TREE_KIND => {
            Some(verb.action.action.clone())
        }
        _ => None,
    };
    let verbs: Vec<String> = match context.what.as_ref()? {
        tilo_actions::Did::Group(group) => group.distinct.iter().filter_map(tree_verb).collect(),
        did => tree_verb(did).into_iter().collect(),
    };
    use fs_protocol::file_action::verbs;
    let verb = [
        verbs::NEW_FOLDER,
        verbs::NEW_FILE,
        verbs::TRASH,
        verbs::RESTORE,
        verbs::MOVE,
        verbs::WRITE,
    ]
    .into_iter()
    .find(|verb| verbs.iter().any(|did| did == verb))?;
    Some(crate::i18n::widget_format(
        &format!("history-folder-did-{verb}"),
        &[("file", path.into())],
    ))
}

/// Who last changed each node whose changes `doc` holds: the changing actor (hex) and the
/// entry the change was written with, which names its invoker. What a folder's row shows as
/// the node's author, read in one pass over the document's changes.
#[must_use]
pub fn last_change_by_node(
    doc: &AutoCommit,
) -> std::collections::HashMap<String, (String, tilo_actions::ActionContext)> {
    let mut changes: Vec<(i64, String, tilo_actions::ActionContext)> = doc
        .committed_document()
        .get_changes_meta(&[])
        .into_iter()
        .filter_map(|change| {
            let context = change
                .message
                .as_deref()
                .and_then(tilo_actions::ActionContext::from_change_message)?;
            Some((change.timestamp, change.actor.to_hex_string(), context))
        })
        .collect();
    changes.sort_by_key(|(time, _, _)| *time);
    let mut last = std::collections::HashMap::new();
    for (_, actor, context) in changes {
        if let Some(item) = context.item.as_ref() {
            last.insert(item.id.clone(), (actor, context));
        }
    }
    last
}

/// Every change one document's folder holds, oldest first: a tree whose nodes are in that
/// one document, as a tree was before it was chunked.
#[must_use]
pub fn entries_in(doc: &AutoCommit, authors: &AuthorDirectory) -> Vec<HistoryEntry> {
    entries(&OneDocument(doc), authors)
}

/// Every change a folder kept in chunks holds, oldest first: `index` is the tree's own document
/// and `chunks` the documents of the chunks this device holds, each read on its own. Chunk
/// documents are never merged into one: one writer's changes to two chunks share its
/// actor, and one document cannot hold both.
#[must_use]
pub fn entries_in_chunks(
    index: &AutoCommit,
    chunks: &[(TreeChunk, AutoCommit)],
    authors: &AuthorDirectory,
) -> Vec<HistoryEntry> {
    entries(&Chunked { index, chunks }, authors)
}

/// A folder held as the tree's own record and the chunk documents beside it.
struct Chunked<'a> {
    index: &'a AutoCommit,
    chunks: &'a [(TreeChunk, AutoCommit)],
}

impl FolderChunks for Chunked<'_> {
    fn tree(&self) -> Option<fs_protocol::tree::Tree> {
        let mut tree = fs_protocol::tree::TreeChunks::default();
        tree.read_index(self.index);
        for (chunk, doc) in self.chunks {
            tree.read_chunk(*chunk, doc);
        }
        Some(tree.tree())
    }

    fn read_chunk<T>(&self, chunk: TreeChunk, read: impl FnOnce(&AutoCommit) -> T) -> Option<T> {
        self.chunks
            .iter()
            .find(|(held, _)| *held == chunk)
            .map(|(_, doc)| read(doc))
    }
}

/// A folder held in one document: every node together, as a tree was before it was chunked.
struct OneDocument<'a>(&'a AutoCommit);

impl FolderChunks for OneDocument<'_> {
    fn tree(&self) -> Option<fs_protocol::tree::Tree> {
        <fs_protocol::tree::FilesTree as tilo_resource::Resource>::read(self.0).ok()
    }

    /// The one document, whichever chunk a node's document id names.
    fn chunks(&self, _: &fs_protocol::tree::Tree) -> BTreeSet<TreeChunk> {
        BTreeSet::from([0])
    }

    fn read_chunk<T>(&self, _chunk: TreeChunk, read: impl FnOnce(&AutoCommit) -> T) -> Option<T> {
        Some(read(self.0))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use automerge::AutoCommit;
    use fs_protocol::FileAction;
    use fs_protocol::files::TreeChunk;
    use fs_protocol::tree::{Placing, ROOT_ID, Tree};
    use tilo_actions::ActionInvoker;

    use super::{FolderChunks, changes, entries};
    use crate::history_source::AuthorDirectory;

    /// A tree kept in two chunks, each a document of its own, as a page holds them.
    struct TwoChunks {
        chunks: BTreeMap<TreeChunk, AutoCommit>,
        tree: Tree,
    }

    impl FolderChunks for TwoChunks {
        fn tree(&self) -> Option<Tree> {
            Some(self.tree.clone())
        }

        fn read_chunk<T>(
            &self,
            chunk: TreeChunk,
            read: impl FnOnce(&AutoCommit) -> T,
        ) -> Option<T> {
            self.chunks.get(&chunk).map(read)
        }
    }

    /// Write `action` into `doc` as the executor does for the call of agent `agent`: one
    /// change, named.
    fn apply_as_agent(doc: &mut AutoCommit, action: &FileAction, agent: &str) {
        let invoker = ActionInvoker::agent(agent);
        let context = fs_protocol::file_action::change_context("tree", action.described(), None)
            .with_invoker(invoker);
        let encoded = tilo_actions::EncodedAction {
            type_id: fs_protocol::file_action::FILE_ACTION_TYPE.to_owned(),
            payload: serde_json::to_vec(action).unwrap(),
        };
        tilo_actions::apply_local_with_context(doc, action, encoded, None, context).unwrap();
    }

    /// A folder in the first chunk holding a file, and a file at the root of the second,
    /// each written once by an agent after it was placed.
    fn two_chunks() -> (TwoChunks, String) {
        let mut first = AutoCommit::new();
        let mut second = AutoCommit::new();
        let mut tree = Tree::default();
        let docs = fs_protocol::tree::place(
            (&mut first, 1),
            &mut tree,
            ROOT_ID,
            "docs",
            Placing::Folder,
            (1, "ada"),
        )
        .unwrap();
        first.commit();
        let hash = "a".repeat(64);
        let file = Placing::File {
            hash: &hash,
            size: 1,
        };
        let plan = fs_protocol::tree::place(
            (&mut second, 2),
            &mut tree,
            &docs.id,
            "plan.md",
            file,
            (2, "ada"),
        )
        .unwrap();
        let notes = fs_protocol::tree::place(
            (&mut second, 2),
            &mut tree,
            ROOT_ID,
            "notes.txt",
            file,
            (3, "ada"),
        )
        .unwrap();
        second.commit();
        // Two agents, so History keeps each change a row of its own.
        for (node, agent) in [(&plan.id, "helper"), (&notes.id, "helper-2")] {
            apply_as_agent(
                &mut second,
                &FileAction::Write {
                    node: node.clone(),
                    hash: Some("b".repeat(64)),
                    size: 2,
                    mtime: 9,
                    author: "ada".into(),
                },
                agent,
            );
        }
        (
            TwoChunks {
                chunks: [(1, first), (2, second)].into_iter().collect(),
                tree,
            },
            plan.id,
        )
    }

    /// A tree spread over two chunks lists every change of a node, each named by the path
    /// the whole tree gives it, and authored by the agent that made it.
    #[test]
    fn a_folder_names_each_change_by_its_path_and_its_author() {
        let (two, _) = two_chunks();
        let rows = entries(&two, &AuthorDirectory::new());
        let mut labels: Vec<String> = rows
            .iter()
            .filter_map(|row| row.target.as_ref().map(|target| target.label.clone()))
            .collect();
        labels.sort();
        assert_eq!(labels, ["docs/plan.md", "notes.txt"], "{rows:#?}");
        assert!(
            rows.iter().any(|row| row.summary == "Edited docs/plan.md"),
            "a change says what it did to the file: {rows:#?}"
        );
        assert!(
            rows.iter().filter(|row| row.target.is_some()).all(|row| row
                .context
                .as_ref()
                .and_then(|context| context.invoker.as_ref())
                .is_some_and(|invoker| format!("{:?}", invoker.acting).contains("helper"))),
            "the agent is each change's author: {rows:#?}"
        );
    }

    /// A file's History is the changes whose item is its node.
    #[test]
    fn a_files_history_is_the_changes_of_its_node() {
        let (two, plan) = two_chunks();
        let document = two.tree.node(&plan).unwrap().document_id.unwrap();
        let listed = changes(&two, &AuthorDirectory::new(), Some(document));
        assert_eq!(listed.len(), 1, "{listed:#?}");
        assert_eq!(listed[0].node.as_deref(), Some(plan.as_str()));
        assert_eq!(listed[0].chunk, 2);
    }
}
