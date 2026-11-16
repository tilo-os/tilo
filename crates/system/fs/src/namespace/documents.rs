//! Document files: a file whose content lives as a CRDT rich-text document, read and written
//! in the format its name declares, instead of opaque bytes.

use super::{Hash, Namespace, NodeKind};
use automerge::{ObjId, ObjType, transaction::Transactable};
use tilo_automerge::read::{map_at, str_at};

/// A document node's field naming, by content id, the bytes last written to its file
/// ([`Namespace::document_written`]).
const VERBATIM: &str = "verbatim";
/// The content id of the document's projection when [`VERBATIM`] was written: the file
/// is those bytes while the projection still is this.
const VERBATIM_READS: &str = "verbatim_reads";

impl Namespace {
    /// Create a file whose content is a structured rich-text document stored as a CRDT (concurrent
    /// edits merge). `codec` is the format key (e.g. `odt`, `md`).
    pub fn create_document(
        &mut self,
        parent_ino: u64,
        name: impl AsRef<[u8]>,
        codec: &str,
        mtime: u64,
    ) -> Option<u64> {
        let parent_id = self.ino_to_id.get(&parent_ino)?.clone();
        let id = self.create_node(&parent_id, name, NodeKind::File, None, 0, mtime)?;
        self.set_field(&id, "codec", codec);
        let node = self.node(&id)?;
        let root = self.doc.put_object(&node, "doc", ObjType::Map).ok()?;
        tilo_doc::model::init(&mut self.doc, &root);
        Some(self.ino_for_id(&id))
    }

    pub fn document_codec(&self, ino: u64) -> Option<String> {
        let id = self.ino_to_id.get(&ino)?;
        str_at(&self.doc, &self.node(id)?, "codec")
    }

    pub(super) fn document_root(&self, id: &str) -> Option<ObjId> {
        map_at(&self.doc, &self.node(id)?, "doc")
    }

    /// Project a document into its file format, embedding the binary content it references from
    /// `content`. A codec normalizes (Markdown joins a paragraph's lines): what the file reads is
    /// [`Self::document_written`] while the document still projects as it did when they were
    /// written.
    pub fn export_document(
        &self,
        ino: u64,
        content: &dyn tilo_content::ContentSource,
    ) -> Option<Vec<u8>> {
        let id = self.ino_to_id.get(&ino)?.clone();
        self.project_document(&id, content)
    }

    /// The bytes last written to document `ino`'s file, by the content id the file's version keeps
    /// them under, and the content id of the document's projection then: the file is those bytes
    /// while the projection still is that.
    pub fn document_written(&self, ino: u64) -> Option<(Hash, String)> {
        let node = self.node(self.ino_to_id.get(&ino)?)?;
        let written = str_at(&self.doc, &node, VERBATIM)?.parse::<Hash>().ok()?;
        Some((written, str_at(&self.doc, &node, VERBATIM_READS)?))
    }

    /// Record that `written` (a content id the file's version keeps) is what was last written to
    /// document `ino`, whose projection is now `projected`; bytes that are the projection already
    /// need no record.
    pub fn set_document_written(
        &mut self,
        ino: u64,
        written: &Hash,
        bytes: &[u8],
        projected: &[u8],
    ) {
        let Some(id) = self.ino_to_id.get(&ino).cloned() else {
            return;
        };
        if bytes == projected {
            if let Some(node) = self.node(&id) {
                let _ = self.doc.delete(&node, VERBATIM);
                let _ = self.doc.delete(&node, VERBATIM_READS);
            }
            return;
        }
        self.set_field(&id, VERBATIM, written.to_string());
        self.set_field(&id, VERBATIM_READS, tilo_content::hash_bytes(projected));
    }

    /// Make `bytes` the file document `ino` is: import them, and keep them as what the file reads
    /// ([`Self::set_document_written`], `written` being their content id; `None` for no bytes).
    ///
    /// `false` when the document cannot be that file: its codec refuses the bytes, writes no file
    /// back, or reads empty bytes as something else. Such a file must stay the bytes written, so
    /// the caller turns it into a plain file ([`Self::make_plain_file`]); what this call changed
    /// in the document goes with it.
    pub fn write_document(
        &mut self,
        ino: u64,
        bytes: &[u8],
        written: Option<&Hash>,
        mtime: u64,
        content: &tilo_content::ContentStore,
    ) -> bool {
        if !self.import_document(ino, bytes, mtime, content) {
            return false;
        }
        let Some(projected) = self.export_document(ino, content) else {
            return false;
        };
        match written {
            Some(written) => self.set_document_written(ino, written, bytes, &projected),
            None if projected.is_empty() => {
                if let Some(node) = self.ino_to_id.get(&ino).and_then(|id| self.node(id)) {
                    let _ = self.doc.delete(&node, VERBATIM);
                    let _ = self.doc.delete(&node, VERBATIM_READS);
                }
            }
            None => return false,
        }
        true
    }

    /// Turn document `ino` into a plain file with no content: its codec, document and written
    /// bytes are dropped, and what the file holds from now on is content like any file's.
    pub fn make_plain_file(&mut self, ino: u64) -> bool {
        let Some(node) = self.ino_to_id.get(&ino).and_then(|id| self.node(id)) else {
            return false;
        };
        for key in ["codec", "doc", VERBATIM, VERBATIM_READS] {
            let _ = self.doc.delete(&node, key);
        }
        true
    }

    fn project_document(
        &self,
        id: &str,
        content: &dyn tilo_content::ContentSource,
    ) -> Option<Vec<u8>> {
        let codec = str_at(&self.doc, &self.node(id)?, "codec")?;
        let provider = tilo_doc_codecs::provider_for_ext(&codec)?;
        let root = self.document_root(id)?;
        let containers = tilo_doc::model::open(&self.doc, &root).ok()?;
        provider
            .export(
                tilo_doc::DocRef {
                    doc: &self.doc,
                    c: &containers,
                },
                content,
            )
            .ok()
    }

    /// Bring a document's content to what `bytes` says (a whole-file write) by reconciling it into
    /// the CRDT the document already is.
    ///
    /// The file states what the document should read, not what its CRDT should hold: blocks the
    /// file leaves unchanged keep their ids, a peer's concurrent edit merges, and a save adds only
    /// its own difference. The binary content the file embeds is kept in `content`.
    ///
    /// Returns whether the write happened: a malformed file leaves the document as it was.
    pub fn import_document(
        &mut self,
        ino: u64,
        bytes: &[u8],
        mtime: u64,
        content: &tilo_content::ContentStore,
    ) -> bool {
        let Some(id) = self.ino_to_id.get(&ino).cloned() else {
            return false;
        };
        let Some(node) = self.node(&id) else {
            return false;
        };
        let Some(codec) = str_at(&self.doc, &node, "codec") else {
            return false;
        };
        let Some(provider) = tilo_doc_codecs::provider_for_ext(&codec) else {
            return false;
        };
        let Some(root) = self.document_root(&id) else {
            return false;
        };
        // Parse into a throwaway document first: a malformed input must never touch the stored one.
        let mut probe = tilo_doc::Document::new();
        let mut inputs = tilo_content::ContentInputs::new();
        if provider
            .import(bytes, &mut probe.as_mut(), &mut inputs)
            .is_err()
        {
            return false;
        }
        let wanted = probe.as_ref().materialize();
        // Keep the file's binary content before any document refers to it.
        if content.put_inputs(&inputs).is_err() {
            return false;
        }
        // The write is one transaction: either it lands as the file says, or it is rolled back and
        // the document reads as it did.
        if tilo_doc::reconcile_under(&mut self.doc, &root, &wanted).is_err()
            || !tilo_doc::reads_as(&self.doc, &root, &wanted)
        {
            self.doc.rollback();
            return false;
        }
        self.set_field(&id, "mtime", mtime);
        true
    }
}
