//! A workspace's files as the page's search finds them: the tree the
//! workspace holds, and the words of each file whose content is here.
//!
//! A computer's search walks its disk. A page has none: a workspace's files
//! are its Files records, which this reads in the page's own store for the
//! workspace ([`WorkspaceResources`]) and hands to the palette's source
//! ([`tilo_desktop_search::providers::tree`]), asking the workspace for the
//! content of each file it names. What arrives is read for its words — a text
//! file as it is, a PDF's pages through the engine that draws them, fetched
//! the first time a PDF arrives ([`PDF_WORDS`]) — so a search finds a file by
//! what it says. Which files a tree has, and where each is, is the tree's own
//! to say ([`fs_protocol::tree::live_files`]), as it is for every host that
//! reads one.
//!
//! A tree is kept in chunks, each a record of its own: the tree's own record
//! names its root and lists the chunks, and each chunk holds the nodes whose
//! record ids name it. This opens a chunk as the tree lists it and reads each
//! record only when it moved, so a change of one chunk costs that chunk.
//!
//! Reading is lazy and cheap: the page calls [`WorkspaceFiles::refresh`] as it
//! draws, which walks the tree again only when a record moved and reads only
//! content that arrived since.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use fs_protocol::files::TreeChunk;
use fs_protocol::tree::{FILES_APP, TREE_KIND, Tree, TreeChunks, chunk_document_id, listed_chunks};
use tilo_desktop_search::Held;
use tilo_desktop_search::providers::tree::{TreeFile, TreeFiles};
use ui::automerge::{AutoCommit, ChangeHash};
use ui::resource::StoredResource;
use ui::resource::workspace::WorkspaceResources;

/// The most of a file read for its words: a bigger file is still found by
/// its name, and the page keeps a workspace's files in its own memory.
const MAX_READ_BYTES: u64 = 4 * 1024 * 1024;

/// Each chunk of the tree, as the page opened it: the record by its number.
type Chunks = BTreeMap<TreeChunk, Rc<StoredResource>>;

/// The heads of the tree's own record and of each of its chunks, in chunk order.
type Heads = Vec<(TreeChunk, Vec<ChangeHash>)>;

/// What the source last read, and what reading it again would take in.
struct Read {
    /// The heads of each record the tree is when its files were last walked:
    /// the tree's own record (chunk 0), and each chunk's.
    heads: Heads,
    /// The content whose words were read, or were too large to read.
    words: Vec<String>,
    /// Each chunk's record, opened as the tree lists it and kept, so the page
    /// follows it change by change rather than opening it again.
    chunks: Chunks,
    /// The hash and size of each file the tree holds, as last walked.
    files: Vec<(String, u64)>,
}

/// A workspace's tree, read for the palette.
pub(crate) struct WorkspaceFiles {
    source: Arc<TreeFiles>,
    records: WorkspaceResources,
    content: tilo_content::ContentStore,
    read: RefCell<Read>,
}

impl WorkspaceFiles {
    /// The files of the workspace `named`, read through the records the page
    /// keeps of it, from the content `content` holds: `named` is what its rows
    /// say the files are of, never the id its records are kept by.
    pub(crate) fn new(
        named: &str,
        records: WorkspaceResources,
        content: tilo_content::ContentStore,
    ) -> Self {
        Self {
            source: Arc::new(TreeFiles::new(named)),
            records,
            content,
            read: RefCell::new(Read {
                heads: Vec::new(),
                words: Vec::new(),
                chunks: BTreeMap::new(),
                files: Vec::new(),
            }),
        }
    }

    /// Say what the workspace is called now, as the shell shows it: what its
    /// files' rows say they are of.
    pub(crate) fn named(&self, name: &str) {
        self.source.set_workspace_name(name);
    }

    /// The palette's source, over the tree as this reads it.
    pub(crate) fn source(&self) -> Held {
        Held::of(Arc::clone(&self.source) as Arc<dyn tilo_desktop_search::Source + Send + Sync>)
    }

    /// The records the page keeps of the workspace, which the tree is one of.
    pub(crate) fn records(&self) -> &WorkspaceResources {
        &self.records
    }

    /// The content the workspace's files are kept in on this page.
    pub(crate) fn content(&self) -> &tilo_content::ContentStore {
        &self.content
    }

    /// The workspace's tree record, opened: on its way while the workspace's hosts have not
    /// sent it or said they hold none ([`ui::resource::ResourceOpenError::Arriving`]).
    pub(crate) fn tree_document(&self) -> Result<StoredResource, ui::resource::ResourceOpenError> {
        self.records.open_resource_at(
            FILES_APP,
            &collab_types::person_document_id(TREE_KIND),
            TREE_KIND,
            &|_doc: &mut AutoCommit| Ok(()),
        )
    }

    /// The tree's own record and the record of each chunk it lists, with each record's heads:
    /// every chunk is opened as the tree lists it, and a chunk still on its way here is asked
    /// for again on a later frame.
    fn opened(&self, read: &mut Read) -> Option<(StoredResource, Chunks, Heads)> {
        let index = self.tree_document().ok()?;
        let listed = index.read(listed_chunks)?;
        let mut chunks = BTreeMap::new();
        let mut heads = vec![(0, index.heads())];
        for chunk in listed {
            let record = if let Some(record) = read.chunks.get(&chunk) {
                Rc::clone(record)
            } else {
                let opened = self
                    .records
                    .open_resource_at(
                        FILES_APP,
                        &chunk_document_id(chunk),
                        TREE_KIND,
                        &|_doc: &mut AutoCommit| Ok(()),
                    )
                    .ok()?;
                let record = Rc::new(opened);
                read.chunks.insert(chunk, Rc::clone(&record));
                record
            };
            heads.push((chunk, record.heads()));
            chunks.insert(chunk, record);
        }
        Some((index, chunks, heads))
    }

    /// The whole tree as its records hold it.
    fn tree_held(index: &StoredResource, chunks: &Chunks) -> Option<Tree> {
        let mut tree = TreeChunks::default();
        index.read(|doc| tree.read_index(doc))?;
        for (chunk, record) in chunks {
            record.read(|doc| tree.read_chunk(*chunk, doc));
        }
        Some(tree.tree())
    }

    /// The workspace's tree as its records hold it now, with the record of each chunk that
    /// holds nodes: for a caller that writes into it, as a drop does.
    pub(crate) fn tree_for_writing(&self) -> Option<(StoredResource, Chunks, Tree)> {
        let mut read = self.read.try_borrow_mut().ok()?;
        let (index, chunks, _) = self.opened(&mut read)?;
        let tree = Self::tree_held(&index, &chunks)?;
        Some((index, chunks, tree))
    }

    /// The workspace's tree as its records hold it now.
    pub(crate) fn tree_now(&self) -> Option<Tree> {
        let mut read = self.read.try_borrow_mut().ok()?;
        let (index, chunks, _) = self.opened(&mut read)?;
        Self::tree_held(&index, &chunks)
    }

    /// Walk the tree again when a record moved, and read the words of each file
    /// whose content arrived since.
    pub(crate) fn refresh(&self) {
        let Ok(mut read) = self.read.try_borrow_mut() else {
            return;
        };
        let Some((index, chunks, heads)) = self.opened(&mut read) else {
            return;
        };
        if read.heads != heads {
            let Some(tree) = Self::tree_held(&index, &chunks) else {
                return;
            };
            read.heads = heads;
            read.files = fs_protocol::tree::live_files(&tree)
                .into_iter()
                .filter(|file| file.node.kind == fs_protocol::files::FileNodeKind::File)
                .filter_map(|file| file.node.hash.clone().map(|hash| (hash, file.node.size)))
                .collect();
            self.source.set_files(files_of(&tree));
        }
        let files = read.files.clone();
        for (hash, size) in files {
            if read.words.contains(&hash) {
                continue;
            }
            if size > MAX_READ_BYTES {
                read.words.push(hash);
                continue;
            }
            let content = &self.content;
            let Some(bytes) = content.read(&hash).and_then(|bytes| {
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
                    read.words.push(hash.clone());
                    if let Some(text) = text {
                        self.source.note_text(&hash, text);
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
    fs_protocol::tree::live_files(tree)
        .into_iter()
        .map(|file| TreeFile {
            id: file.id.to_owned(),
            name: file.name,
            path: file.path,
            hash: file.node.hash.clone(),
            folder: file.node.kind == fs_protocol::files::FileNodeKind::Directory,
            size: file.node.size,
        })
        .collect()
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
