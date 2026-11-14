//! A workspace's files as the page's search finds them: the tree the
//! workspace holds, and the words of each file whose content is here.
//!
//! A computer's search walks its disk. A page has none: a workspace's files
//! are its Files record, which this reads in the page's own store for the
//! workspace ([`WorkspaceRecords`]) and hands to the palette's source
//! ([`tilo_desktop_search::providers::tree`]), asking the workspace for the
//! content of each file it names. What arrives is read for its words — a text
//! file as it is, a PDF's pages through the engine that draws them, fetched
//! the first time a PDF arrives ([`PDF_WORDS`]) — so a search finds a file by
//! what it says.
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

    /// The records the page keeps of the workspace, which the tree is one of.
    pub(crate) fn records(&self) -> &WorkspaceRecords {
        &self.records
    }

    /// The content the workspace's files are kept in on this page.
    pub(crate) fn content(&self) -> &tilo_content::ContentStore {
        &self.content
    }

    /// The workspace's tree record, opened: on its way while the workspace's hosts have not
    /// sent it or said they hold none ([`ui::record::RecordOpenError::Arriving`]).
    pub(crate) fn tree_record(&self) -> Result<StoredRecord, ui::record::RecordOpenError> {
        self.records.open_record_at(
            FILES_APP,
            &collab_types::person_record_id(TREE_KIND),
            TREE_KIND,
            &|_doc: &mut AutoCommit| Ok(()),
        )
    }

    /// The workspace's tree, as the record holds it now.
    fn tree(&self) -> Option<(StoredRecord, Tree)> {
        let record = self.tree_record().ok()?;
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
            let content = &self.content;
            let Some(bytes) = content.read(hash).and_then(|bytes| {
                fs_content::whole(bytes, MAX_READ_BYTES, |chunk| {
                    content.read(&hex::encode(chunk))
                })
            }) else {
                // Reading content the page lacks asks the workspace for it;
                // its words are read when it is here.
                continue;
            };
            match words_of(&bytes) {
                Words::Read(text) => {
                    read.read.push(hash.clone());
                    if let Some(text) = text {
                        self.source.note_text(hash, text);
                    }
                }
                // Read again once the code that reads it is here.
                Words::Coming => {}
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

/// What reading a file's content for its words came to.
enum Words {
    /// Its words, or `None` for content nothing reads.
    Read(Option<String>),
    /// A PDF, whose reader the page has yet to fetch ([`PDF_WORDS`]).
    Coming,
}

/// The words `bytes` are, for a search: a text file as it is, a PDF's pages
/// through the engine that draws them.
fn words_of(bytes: &[u8]) -> Words {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Words::Read(Some(text.to_owned()));
    }
    if !bytes.starts_with(b"%PDF-") {
        return Words::Read(None);
    }
    match PDF_WORDS.get() {
        Some(code) => Words::Read((code.read)(bytes)),
        None => Words::Coming,
    }
}

/// The code that reads a PDF's words: the PDF engine is the largest part of
/// it, and only a workspace holding a PDF needs it, so the browser desktop
/// fetches it as its own module the first time one arrives
/// (`deferred-pdf-words`; linked otherwise).
#[derive(Clone, Copy)]
pub struct PdfWords {
    read: fn(&[u8]) -> Option<String>,
}

/// The PDF reader this build links.
#[must_use]
pub fn builtin_pdf_words() -> PdfWords {
    PdfWords { read: pdf_words }
}

ui::lazy_code! {
    /// The code that reads a PDF's words.
    pub static PDF_WORDS: PdfWords = builtin_pdf_words, feature = "deferred-pdf-words";
}

/// A PDF's pages' words, through the engine the viewer draws with.
fn pdf_words(bytes: &[u8]) -> Option<String> {
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
