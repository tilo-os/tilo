//! A workspace's folder as a find ranks it, kept as the tree's chunks land.
//!
//! The folder is the workspace's Files document: the tree's own document lists the chunks, and each
//! chunk holds its file nodes ([`fs_protocol::tree`]). Reading that whole for every find is what
//! a person waited on after they searched a word a hundred files say: every find put the tree's
//! documents together again and read each file's bytes. So the tree's documents are read as they
//! land, between requests, and each file's words are kept beside its node: a find ranks what is
//! kept without opening a chunk document or a file.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use automerge::AutoCommit;
use fs_protocol::files::{FileNodeKind, TreeChunk};
use fs_protocol::tree::{self, TreeChunks, TreeEntry};
use tilo_content::ContentStore;

use super::tree_put::sealed::FileSealing;

/// The most of one file read for its words: a larger one is still found by its name.
const MOST_READ_BYTES: u64 = 1024 * 1024;

/// One file of the tree, as a search ranks it.
pub(super) struct File {
    pub(super) path: String,
    pub(super) name: String,
    pub(super) words: Option<Arc<str>>,
}

/// A workspace's folder, as a find ranks it: the tree's nodes by chunk, the revision each chunk
/// was read at, and the words of each file.
#[derive(Default)]
pub(super) struct FilesIndex {
    /// The tree's nodes, by chunk, and the chunks its own document lists.
    tree: TreeChunks,
    /// The chunks read, for telling which wait on a first read.
    read: BTreeSet<TreeChunk>,
    /// The words of each file node, as its chunk read them; `None` for a file with none.
    words: HashMap<String, Option<Arc<str>>>,
    /// The chunks whose write landed since they were read.
    stale: BTreeSet<TreeChunk>,
    /// The chunks this replica could not read yet: looked at again when a write lands, not
    /// before, so a document that never arrives holds no pass open.
    given_up: BTreeSet<TreeChunk>,
    /// Whether the tree's own document, which lists the chunks, was read.
    listed: bool,
}

impl FilesIndex {
    /// A write to `document` landed: the chunk it belongs to is read again, and the tree's own
    /// document lists its chunks again. A chunk the listing does not name yet is read from a
    /// listing that predates it, so the tree's own document is read again for that too.
    pub(super) fn landed(&mut self, document: &str) {
        if let Some(chunk) = tree::chunk_of_record(document) {
            if chunk == 0 || !self.tree.listed().contains(&chunk) {
                self.listed = false;
            }
            self.given_up.remove(&chunk);
            self.stale.insert(chunk);
        }
    }

    /// Look for the folder's documents again where the index has none of them: a find over a
    /// workspace whose folder was founded after an earlier one gave up on the tree that was not
    /// there yet reads it now, and one that read the tree before a chunk was written reads the
    /// chunk now, whether or not its write was heard since.
    pub(super) fn look_for_the_tree(&mut self) {
        if !self.listed || self.tree.listed().is_empty() {
            self.given_up.remove(&0);
            self.stale.insert(0);
        }
        for chunk in self.tree.listed() {
            if !self.read.contains(&chunk) {
                self.given_up.remove(&chunk);
            }
        }
    }

    /// The next chunk document to read: the tree's own document first, then each chunk it lists
    /// that was not read or changed since. `None` when nothing waits.
    pub(super) fn next(&self) -> Option<TreeChunk> {
        if (!self.listed && !self.given_up.contains(&0)) || self.stale.contains(&0) {
            return Some(0);
        }
        self.tree.listed().into_iter().find(|chunk| {
            self.stale.contains(chunk)
                || (!self.read.contains(chunk) && !self.given_up.contains(chunk))
        })
    }

    /// Whether the index has read every document it can now.
    pub(super) fn settled(&self) -> bool {
        self.next().is_none()
    }

    /// The document this replica cannot answer for yet: looked at again when a write lands.
    pub(super) fn pending(&mut self, chunk: TreeChunk) {
        self.stale.remove(&chunk);
        self.given_up.insert(chunk);
    }

    /// Read chunk `chunk` of the tree as `bytes` holds it, with `content` for a file's bytes.
    /// The words of the files whose node changed since are read again, and how many were.
    pub(super) fn read(
        &mut self,
        chunk: TreeChunk,
        bytes: &[u8],
        content: Option<&ContentStore>,
        sealing: Option<&FileSealing>,
    ) -> usize {
        self.read.insert(chunk);
        self.stale.remove(&chunk);
        self.given_up.remove(&chunk);
        let Ok(doc) = AutoCommit::load(bytes) else {
            if chunk == 0 {
                self.listed = true;
            }
            return 0;
        };
        if chunk == 0 {
            self.tree.read_index(&doc);
            self.listed = true;
            return 0;
        }
        let old = self.tree.chunk(chunk).cloned().unwrap_or_default();
        self.tree.read_chunk(chunk, &doc);
        let new = self.tree.chunk(chunk).cloned().unwrap_or_default();
        let mut read = 0_usize;
        for (id, entry) in &new {
            if entry.kind != FileNodeKind::File {
                self.words.remove(id);
                continue;
            }
            if old.get(id) == Some(entry) && self.words.contains_key(id) {
                continue;
            }
            self.words
                .insert(id.clone(), words_of(entry, content, sealing));
            read = read.saturating_add(1);
        }
        for id in old.keys() {
            if !new.contains_key(id) {
                self.words.remove(id);
            }
        }
        read
    }

    /// The files of the folder, by the path each is at, as the tree currently reads.
    pub(super) fn files(&self) -> Vec<File> {
        let tree = self.tree.tree();
        tree::live_files(&tree)
            .into_iter()
            .filter(|file| file.node.kind == FileNodeKind::File)
            .map(|file| File {
                path: file.path,
                name: file.name,
                words: self.words.get(file.id).cloned().flatten(),
            })
            .collect()
    }
}

/// The words of the file node `entry`: the bytes `content` keeps under its hash.
fn words_of(
    entry: &TreeEntry,
    content: Option<&ContentStore>,
    sealing: Option<&FileSealing>,
) -> Option<Arc<str>> {
    let store = content?;
    let hash = entry.hash.as_deref()?;
    if entry.size > MOST_READ_BYTES {
        return None;
    }
    let file = fs_content::whole_with(
        store.read(hash)?,
        MOST_READ_BYTES,
        |chunk| store.read(&hex::encode(chunk)),
        |group, ciphertext| sealing?.open(group, ciphertext),
    )?;
    String::from_utf8(file).ok().map(Arc::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable as _;
    use automerge::{ObjType, ROOT};

    /// The tree's own document, listing `chunks`.
    fn index_of(chunks: &[u32]) -> Vec<u8> {
        let mut doc = AutoCommit::new();
        for chunk in chunks {
            tree::list_chunk(&mut doc, *chunk).unwrap();
        }
        doc.save()
    }

    /// A chunk document holding the file `name` under the workspace's root.
    fn chunk_of(name: &str) -> Vec<u8> {
        let mut doc = AutoCommit::new();
        let nodes = doc.put_object(ROOT, tree::NODES_KEY, ObjType::Map).unwrap();
        let node = doc
            .put_object(&nodes, format!("device/{name}"), ObjType::Map)
            .unwrap();
        for (key, value) in [
            (tree::PARENT_KEY, tree::ROOT_ID),
            (tree::NAME_KEY, name),
            (tree::KIND_KEY, "file"),
        ] {
            doc.put(&node, key, value).unwrap();
        }
        doc.save()
    }

    #[test]
    fn a_chunk_is_read_once_and_a_file_again_only_when_its_node_changed() {
        let mut index = FilesIndex::default();
        assert_eq!(
            index.next(),
            Some(0),
            "the tree's own document is read first"
        );
        assert_eq!(index.read(0, &index_of(&[1]), None, None), 0);
        assert_eq!(index.next(), Some(1), "the chunk it lists is read next");
        assert_eq!(index.read(1, &chunk_of("a.txt"), None, None), 1);
        assert!(index.settled());
        assert_eq!(
            index.read(1, &chunk_of("a.txt"), None, None),
            0,
            "a chunk read again with the nodes it had reads no file again"
        );
        assert_eq!(index.files()[0].path, "a.txt");

        // A node that changed is read again, and the file it was is no longer kept.
        assert_eq!(index.read(1, &chunk_of("b.txt"), None, None), 1);
        let files = index.files();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "b.txt");
    }

    #[test]
    fn a_document_that_cannot_be_read_yet_waits_for_a_write_to_land() {
        let mut index = FilesIndex::default();
        assert_eq!(index.next(), Some(0));
        index.pending(0);
        assert!(
            index.settled(),
            "a document this replica cannot answer for holds no pass open"
        );
        index.landed(&tree::chunk_document_id(0));
        assert_eq!(
            index.next(),
            Some(0),
            "the write landing reads the document again"
        );
    }

    #[test]
    fn a_chunk_the_listing_does_not_name_yet_lists_it_again() {
        let mut index = FilesIndex::default();
        assert_eq!(index.read(0, &index_of(&[]), None), 0);
        assert!(index.settled(), "the tree names no chunk yet");
        // A write to a chunk the listing read before it existed names it in the index.
        index.landed(&tree::chunk_document_id(1));
        assert_eq!(
            index.next(),
            Some(0),
            "the tree's own document is read to list the chunk"
        );
        assert_eq!(index.read(0, &index_of(&[1]), None), 0);
        assert_eq!(index.next(), Some(1), "the chunk it names is read next");
    }

    #[test]
    fn a_find_reads_the_tree_that_was_not_there_yet_again() {
        let mut index = FilesIndex::default();
        assert_eq!(index.next(), Some(0));
        index.pending(0);
        assert!(index.settled(), "the tree is not here yet");
        // The folder was founded since: the next find looks for the tree again.
        index.look_for_the_tree();
        assert_eq!(
            index.next(),
            Some(0),
            "a find looks for a tree it never read"
        );
        // The tree read before its chunks were listed is read again too.
        assert_eq!(index.read(0, &index_of(&[]), None), 0);
        index.look_for_the_tree();
        assert_eq!(
            index.next(),
            Some(0),
            "a find looks again at a tree that lists no chunk"
        );
        assert_eq!(index.read(0, &index_of(&[1]), None), 0);
        index.pending(1);
        index.look_for_the_tree();
        assert_eq!(
            index.next(),
            Some(1),
            "a find looks again at a chunk it never read"
        );
        assert_eq!(index.read(1, &chunk_of("a.txt"), None), 1);
        index.look_for_the_tree();
        assert!(
            index.settled(),
            "what the folder listed and the index read is not looked for again"
        );
    }
}
