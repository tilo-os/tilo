//! A workspace's files, found by words: the files of its tree ([`fs_protocol::tree`]), by their
//! names and paths and by what a text file or a text document says.
//!
//! A workspace has one folder, its Files record, and a search of the workspace finds what is in
//! it as it finds what its apps hold. A file is not a resource of an app: it is answered by its
//! path in the folder (`legal/contract.txt`), which a computer that logged in has at
//! `~/<workspace>/legal/contract.txt` and a page shows in Files. Its words come from the content
//! this host keeps of the tree ([`super::content`]), put together from the chunks its manifest
//! lists ([`fs_content::whole`]).

use std::collections::BTreeMap;

use fs_protocol::files::{FileNodeKind, TreeChunk};
use fs_protocol::tree::{FILES_APP, TREE_KIND, Tree, TreeChunks};
use tilo_actions::remote::Found;
use tilo_server_protocol::WorkspaceId;

use super::Worker;

use tilo_actions::remote::FOUND_FILE as FILE_KIND;

/// The most of one file read for its words: a larger one is still found by its name.
const MOST_READ_BYTES: u64 = 1024 * 1024;

/// One file of the tree, as a search ranks it.
struct File {
    path: String,
    name: String,
    words: Option<String>,
}

impl Worker {
    /// The files of `workspace_id`'s tree that `needle` finds, those it names first, then those
    /// whose words hold it, at most `most`.
    pub(super) fn found_files(
        &self,
        workspace_id: WorkspaceId,
        cluster_id: &str,
        needle: &str,
        most: usize,
    ) -> Vec<Found> {
        let needle = ui::search::fold_search_text(needle.trim());
        if needle.is_empty() || most == 0 {
            return Vec::new();
        }
        let Some(tree) = self.workspace_tree(workspace_id, cluster_id) else {
            return Vec::new();
        };
        let store = self.workspace_content(workspace_id);
        let files = files_of(&tree, |node| {
            let store = store.as_ref()?;
            let hash = node.hash.as_deref()?;
            if node.size > MOST_READ_BYTES {
                return None;
            }
            let file = fs_content::whole(store.read(hash)?, MOST_READ_BYTES, |chunk| {
                store.read(&hex::encode(chunk))
            })?;
            String::from_utf8(file).ok()
        });
        ranked(files, &needle, most)
    }
}

impl Worker {
    /// The tree of `workspace_id`'s folder (the session `cluster_id`) as this replica holds
    /// it: the tree's own record and each chunk of it held here. `None` while it holds none.
    pub(super) fn workspace_tree(
        &self,
        workspace_id: WorkspaceId,
        cluster_id: &str,
    ) -> Option<Tree> {
        let workspace = self.workspaces.get(&workspace_id)?;
        Some(tree_records(workspace.peer.engine(), cluster_id)?.tree())
    }
}

/// The records of a workspace's tree as the replica holds them: the tree's own
/// record and each chunk it lists, saved.
struct TreeRecords {
    index: automerge::AutoCommit,
    chunks: BTreeMap<TreeChunk, automerge::AutoCommit>,
}

impl TreeRecords {
    /// Take the tree's own record `index` and the chunks it lists, read by
    /// `read`.
    fn read(index: &[u8], read: impl Fn(TreeChunk) -> Option<Vec<u8>>) -> Option<Self> {
        let index: automerge::AutoCommit = automerge::AutoCommit::load(index).ok()?;
        let chunks = fs_protocol::tree::listed_chunks(&index)
            .into_iter()
            .filter_map(|chunk| Some((chunk, automerge::AutoCommit::load(&read(chunk)?).ok()?)))
            .collect();
        Some(Self { index, chunks })
    }

    /// The tree every record read holds.
    fn tree(self) -> Tree {
        let mut tree = TreeChunks::default();
        tree.read_index(&self.index);
        for (chunk, doc) in &self.chunks {
            tree.read_chunk(*chunk, doc);
        }
        tree.tree()
    }
}

/// The records of the tree of `cluster_id`, as the replica `engine` holds them.
fn tree_records(
    engine: &tilo_session_client::engine::Engine,
    cluster_id: &str,
) -> Option<TreeRecords> {
    let index = engine
        .record(
            cluster_id,
            FILES_APP,
            &collab_types::person_document_id(TREE_KIND),
        )
        .ok()
        .flatten()?;
    TreeRecords::read(&index, |chunk| {
        engine
            .record(
                cluster_id,
                FILES_APP,
                &fs_protocol::tree::chunk_document_id(chunk),
            )
            .ok()
            .flatten()
    })
}

/// Each live file of `tree` with its path, and its words as `words` reads them.
fn files_of(
    tree: &Tree,
    mut words: impl FnMut(&fs_protocol::tree::TreeEntry) -> Option<String>,
) -> Vec<File> {
    fs_protocol::tree::live_files(tree)
        .into_iter()
        .filter(|file| file.node.kind == FileNodeKind::File)
        .map(|file| File {
            path: file.path,
            name: file.name,
            words: words(file.node),
        })
        .collect()
}

/// The files `needle` (folded) finds: those whose name or path says it, then those whose words
/// do, at most `most`.
fn ranked(files: Vec<File>, needle: &str, most: usize) -> Vec<Found> {
    let says = |text: &str| ui::search::fold_search_text(text).contains(needle);
    let (named, rest): (Vec<File>, Vec<File>) = files
        .into_iter()
        .partition(|file| says(&file.name) || says(&file.path));
    named
        .into_iter()
        .chain(
            rest.into_iter()
                .filter(|file| file.words.as_deref().is_some_and(says)),
        )
        .take(most)
        .map(|file| Found {
            resource: file.path.clone(),
            kind: FILE_KIND.to_owned(),
            label: file.path,
            open: None,
            arguments: serde_json::Map::new(),
            app: Some(FILES_APP.to_owned()),
            provider: None,
            when: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable as _;
    use automerge::{ObjType, ROOT};

    fn file(path: &str, words: Option<&str>) -> File {
        File {
            path: path.to_owned(),
            name: path.rsplit('/').next().unwrap_or(path).to_owned(),
            words: words.map(str::to_owned),
        }
    }

    #[test]
    fn a_file_is_found_by_its_name_then_by_what_it_says() {
        let files = || {
            vec![
                file("handbook.md", Some("We ship on Fridays. Expenses: net 30.")),
                file("legal/contract.txt", Some("Payment terms: net 30.")),
                file("legal/notice.pdf", None),
                file("config.json", Some("{\"retention_days\": 90}")),
            ]
        };
        let paths = |found: Vec<Found>| -> Vec<String> {
            found.into_iter().map(|found| found.resource).collect()
        };
        assert_eq!(
            paths(ranked(files(), "contract", 10)),
            ["legal/contract.txt"]
        );
        assert_eq!(
            paths(ranked(files(), "net 30", 10)),
            ["handbook.md", "legal/contract.txt"]
        );
        assert_eq!(
            paths(ranked(files(), "legal", 10)),
            ["legal/contract.txt", "legal/notice.pdf"],
            "a folder's name finds what is in it"
        );
        assert_eq!(paths(ranked(files(), "net 30", 1)), ["handbook.md"]);
        assert!(ranked(files(), "harbour", 10).is_empty());
        let found = ranked(files(), "contract", 10).remove(0);
        assert_eq!(
            (found.kind.as_str(), found.app.as_deref()),
            (FILE_KIND, Some(FILES_APP))
        );
    }
    /// The records of a workspace's folder, as a member reads them: the tree's own
    /// record names the chunks, and each chunk is read for the files it holds.
    #[test]
    fn a_folder_reads_from_the_chunks_its_own_record_lists() {
        let names = |tree: &Tree| -> Vec<String> {
            tree.children(fs_protocol::tree::ROOT_ID)
                .into_iter()
                .map(|(_, node)| String::from_utf8_lossy(node.name.as_bytes()).into_owned())
                .collect()
        };
        let records = TreeRecords::read(&index_of(&[1, 2]), |chunk| {
            Some(chunk_of(&format!("f{chunk}.txt")))
        })
        .expect("the tree's own record reads");
        assert_eq!(names(&records.tree()), ["f1.txt", "f2.txt"]);

        // A chunk this member holds none of is no part of the folder: what it finds
        // is what it can read.
        let records = TreeRecords::read(&index_of(&[1, 2]), |chunk| {
            (chunk == 1).then(|| chunk_of("here.txt"))
        })
        .expect("the tree's own record reads");
        assert_eq!(names(&records.tree()), ["here.txt"]);
    }

    /// A chunk record holding the file `name` under the workspace's root.
    fn chunk_of(name: &str) -> Vec<u8> {
        let mut doc = automerge::AutoCommit::new();
        let nodes = doc
            .put_object(ROOT, fs_protocol::tree::NODES_KEY, ObjType::Map)
            .unwrap();
        // A node's id is its record's, which no other chunk holds.
        let node = doc
            .put_object(&nodes, format!("device/{name}"), ObjType::Map)
            .unwrap();
        for (key, value) in [
            (fs_protocol::tree::PARENT_KEY, fs_protocol::tree::ROOT_ID),
            (fs_protocol::tree::NAME_KEY, name),
            (fs_protocol::tree::KIND_KEY, "file"),
        ] {
            doc.put(&node, key, value).unwrap();
        }
        doc.save()
    }

    /// The tree's own record, listing the chunks `chunks`.
    fn index_of(chunks: &[u32]) -> Vec<u8> {
        let mut doc = automerge::AutoCommit::new();
        for chunk in chunks {
            fs_protocol::tree::list_chunk(&mut doc, *chunk).unwrap();
        }
        doc.save()
    }
}
