//! A workspace's files, found by words: the files of its tree ([`fs_protocol::tree`]), by their
//! names and paths and by what a text file says.
//!
//! A workspace has one folder, its Files record, and a search of the workspace finds what is in
//! it as it finds what its apps hold. A file is not a resource of an app: it is answered by its
//! path in the folder (`legal/contract.txt`), which a computer that logged in has at
//! `~/<workspace>/legal/contract.txt` and a page shows in Files. Its words are indexed as the
//! tree's chunks land ([`super::files_index`]), so a find ranks them without opening the tree's
//! documents or reading a file.

use std::collections::BTreeMap;
use std::sync::Arc;

use fs_protocol::files::TreeChunk;
use fs_protocol::tree::{FILES_APP, TREE_KIND, Tree, TreeChunks};
use tilo_actions::remote::Found;
use tilo_server_protocol::WorkspaceId;

use super::files_index::File;
use super::{Ordering, Worker};

use tilo_actions::remote::FOUND_FILE as FILE_KIND;

impl Worker {
    /// The files of `workspace_id`'s tree that `needle` finds, those it names first, then those
    /// whose words hold it, at most `most`.
    pub(super) fn found_files(
        &mut self,
        workspace_id: WorkspaceId,
        cluster_id: &str,
        needle: &str,
        most: usize,
    ) -> Vec<Found> {
        let needle = ui::search::fold_search_text(needle.trim());
        if needle.is_empty() || most == 0 {
            return Vec::new();
        }
        let Some(engine) = self
            .workspaces
            .get(&workspace_id)
            .map(|workspace| Arc::clone(workspace.peer.engine()))
        else {
            return Vec::new();
        };
        let content = self.workspace_content(workspace_id);
        let counted = &self.shared.search;
        let Some(workspace) = self.workspaces.get_mut(&workspace_id) else {
            return Vec::new();
        };
        // What the index has not read yet: the documents whose write landed since the last pass
        // between requests. A document this replica cannot answer for yet is answered next time.
        while let Some(chunk) = workspace.files.next() {
            let known = workspace.files.known(chunk);
            let chunk_id = fs_protocol::tree::chunk_document_id(chunk);
            match engine.record_since(cluster_id, FILES_APP, &chunk_id, known) {
                Ok(Some((bytes, revision))) => {
                    counted.files_chunks.fetch_add(1, Ordering::Relaxed);
                    let files = workspace
                        .files
                        .read(chunk, &bytes, revision, content.as_ref());
                    counted
                        .files_read
                        .fetch_add(files as u64, Ordering::Relaxed);
                }
                Ok(None) if known.is_some() => workspace.files.unchanged(chunk),
                Ok(None) | Err(_) => {
                    workspace.files.pending(chunk);
                    break;
                }
            }
        }
        ranked(workspace.files.files(), &needle, most)
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

    fn file(path: &str, words: Option<&str>) -> File {
        File {
            path: path.to_owned(),
            name: path.rsplit('/').next().unwrap_or(path).to_owned(),
            words: words.map(Arc::from),
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
}
