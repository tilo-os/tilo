//! A workspace's files, found by words: the files of its tree ([`fs_protocol::tree`]), by their
//! names and paths and by what a text file or a text document says.
//!
//! A workspace has one folder, its Files record, and a search of the workspace finds what is in
//! it as it finds what its apps hold. A file is not a resource of an app: it is answered by its
//! path in the folder (`legal/contract.txt`), which a computer that logged in has at
//! `~/<workspace>/legal/contract.txt` and a page shows in Files. Its words come from the content
//! this host keeps of the tree ([`super::content`]), put together from the chunks its manifest
//! lists ([`fs_content::whole`]), or, for a text document, from the tree itself
//! ([`fs_protocol::tree::document_file`]).

use fs_protocol::files::FileNodeKind;
use fs_protocol::tree::{FILES_APP, ROOT_ID, TREE_KIND, Tree};
use tilo_actions::remote::Found;
use tilo_relay_protocol::WorkspaceId;

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
        let Some(bytes) = self.workspaces.get(&workspace_id).and_then(|workspace| {
            workspace
                .peer
                .engine()
                .record(
                    cluster_id,
                    FILES_APP,
                    &collab_types::person_record_id(TREE_KIND),
                )
                .ok()
                .flatten()
        }) else {
            return Vec::new();
        };
        let Ok(doc) = automerge::AutoCommit::load(&bytes) else {
            return Vec::new();
        };
        let Ok(tree) = <fs_protocol::tree::FilesTree as ui::tilo_resource::Resource>::read(&doc)
        else {
            return Vec::new();
        };
        let store = self.workspace_content(workspace_id);
        let files = files_of(&tree, |id, node| {
            if node.codec.is_some() {
                let file = fs_protocol::tree::document_file(&doc, id, &tilo_content::NoContent)?;
                return String::from_utf8(file).ok();
            }
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

/// Each live file of `tree` with its path, and its words as `words` reads them.
fn files_of(
    tree: &Tree,
    mut words: impl FnMut(&str, &fs_protocol::tree::TreeEntry) -> Option<String>,
) -> Vec<File> {
    let mut files = Vec::new();
    for (id, node) in &tree.nodes {
        if node.kind != FileNodeKind::File || !reaches_root(tree, id) {
            continue;
        }
        files.push(File {
            path: path_of(tree, id),
            name: String::from_utf8_lossy(node.name.as_bytes()).into_owned(),
            words: words(id, node),
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files
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
        })
        .collect()
}

/// Whether `id` hangs under the tree's root: a node moved to the trash, or under a folder that
/// was, is no file of the folder.
fn reaches_root(tree: &Tree, id: &str) -> bool {
    let mut at = id;
    for _ in 0..tree.nodes.len().saturating_add(1) {
        if at == ROOT_ID {
            return true;
        }
        match tree.node(at).and_then(|node| node.parent.as_deref()) {
            Some(parent) if parent != at => at = parent,
            _ => return false,
        }
    }
    false
}

/// Where `id` is in the folder, as `legal/contract.txt`.
fn path_of(tree: &Tree, id: &str) -> String {
    let mut names: Vec<String> = Vec::new();
    let mut at = id;
    while at != ROOT_ID {
        let Some(node) = tree.node(at) else {
            break;
        };
        names.push(String::from_utf8_lossy(node.name.as_bytes()).into_owned());
        match node.parent.as_deref() {
            Some(parent) if parent != at => at = parent,
            _ => break,
        }
    }
    names.reverse();
    names.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
