//! A workspace's files as the page's search finds them: the tree the
//! workspace holds, and the words of each file whose content is here.
//!
//! A computer's search walks its disk. A page has none: a workspace's files
//! are its Files record, which this reads in the page's own store for the
//! workspace ([`WorkspaceRecords`]) and hands to the palette's source
//! ([`tilo_desktop_search::providers::tree`]), asking the workspace for the
//! content of each file it names. What arrives is read for its words — a text
//! file as it is, a PDF's pages through the engine that draws them — so a
//! search finds a file by what it says.
//!
//! Reading is lazy and cheap: the page calls [`WorkspaceFiles::refresh`] as it
//! draws, which walks the tree again only when the record moved and reads only
//! content that arrived since.

use std::cell::RefCell;
use std::sync::Arc;

use fs_protocol::tree::{FILES_APP, ROOT_ID, TREE_KIND, Tree};
use tilo_desktop_search::Held;
use tilo_desktop_search::providers::tree::{TreeFile, TreeFiles};
use ui::automerge::{AutoCommit, ChangeHash};
use ui::record::StoredRecord;
use ui::record::workspace::WorkspaceRecords;

/// The most of a file read for its words: a bigger file is still found by
/// its name, and the page keeps a workspace's files in its own memory.
const MAX_READ_BYTES: u64 = 4 * 1024 * 1024;

/// What the source last read, and what reading it again would take in.
struct Read {
    /// The heads of the tree when its files were last walked.
    heads: Vec<ChangeHash>,
    /// The content whose words were read, or were too large to read.
    read: Vec<String>,
}

/// A workspace's tree, read for the palette.
pub(crate) struct WorkspaceFiles {
    source: Arc<TreeFiles>,
    records: WorkspaceRecords,
    content: tilo_content::ContentStore,
    read: RefCell<Read>,
}

impl WorkspaceFiles {
    /// The files of `workspace`, read through the records the page keeps of it,
    /// from the content `content` holds.
    pub(crate) fn new(
        workspace: &str,
        records: WorkspaceRecords,
        content: tilo_content::ContentStore,
    ) -> Self {
        Self {
            source: Arc::new(TreeFiles::new(workspace)),
            records,
            content,
            read: RefCell::new(Read {
                heads: Vec::new(),
                read: Vec::new(),
            }),
        }
    }

    /// The palette's source, over the tree as this reads it.
    pub(crate) fn source(&self) -> Held {
        Held::of(Arc::clone(&self.source) as Arc<dyn tilo_desktop_search::Source + Send + Sync>)
    }

    /// The workspace's tree, as the record holds it now.
    fn tree(&self) -> Option<(StoredRecord, Tree)> {
        let record = self
            .records
            .open_record_at(
                FILES_APP,
                &collab_types::person_record_id(TREE_KIND),
                TREE_KIND,
                &|_doc: &mut AutoCommit| Ok(()),
            )
            .ok()?;
        let tree = record.read(|doc| {
            <fs_protocol::tree::FilesTree as tilo_resource::Resource>::read(doc).ok()
        })??;
        Some((record, tree))
    }

    /// Walk the tree again when it moved, and read the words of each file
    /// whose content arrived since.
    pub(crate) fn refresh(&self) {
        let Some((record, tree)) = self.tree() else {
            return;
        };
        let heads = record.heads();
        let mut read = self.read.borrow_mut();
        if read.heads != heads {
            read.heads = heads;
            self.source.set_files(files_of(&tree));
        }
        for node in tree.nodes.values() {
            let Some(hash) = node.hash.as_ref() else {
                continue;
            };
            if read.read.contains(hash) {
                continue;
            }
            if node.size > MAX_READ_BYTES {
                read.read.push(hash.clone());
                continue;
            }
            let Some(bytes) = self.content.read(hash) else {
                // Reading content the page lacks asks the workspace for it;
                // its words are read when it is here.
                continue;
            };
            read.read.push(hash.clone());
            if let Some(text) = words_of(&bytes) {
                self.source.note_text(hash, text);
            }
        }
    }
}

/// The files of `tree`, each with the path it is at.
fn files_of(tree: &Tree) -> Vec<TreeFile> {
    let mut files = Vec::with_capacity(tree.nodes.len());
    for (id, node) in &tree.nodes {
        if tree.trashed(id) {
            continue;
        }
        files.push(TreeFile {
            id: id.clone(),
            name: String::from_utf8_lossy(node.name.as_bytes()).into_owned(),
            path: path_of(tree, id),
            hash: node.hash.clone(),
            folder: node.kind == fs_protocol::files::FileNodeKind::Directory,
            size: node.size,
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files
}

/// Where `id` is in `tree`, as `docs/plan.md`.
fn path_of(tree: &Tree, id: &str) -> String {
    let mut names: Vec<String> = Vec::new();
    let mut at = id.to_owned();
    while at != ROOT_ID {
        let Some(node) = tree.node(&at) else {
            break;
        };
        names.push(String::from_utf8_lossy(node.name.as_bytes()).into_owned());
        match node.parent.clone() {
            Some(parent) if parent != at => at = parent,
            _ => break,
        }
    }
    names.reverse();
    names.join("/")
}

/// The words `bytes` are, for a search: a text file as it is, a PDF's pages
/// through the engine that draws them. `None` for content nothing reads.
fn words_of(bytes: &[u8]) -> Option<String> {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Some(text.to_owned());
    }
    let engine = tilo_pdf::Engine::shared().ok()?;
    let document = engine.open_bytes(bytes).ok()?;
    let mut words = String::new();
    for page in 0..document.page_count() {
        let Ok(layer) = document.text(page) else {
            continue;
        };
        words.push_str(layer.text());
        words.push('\n');
    }
    Some(words)
}
