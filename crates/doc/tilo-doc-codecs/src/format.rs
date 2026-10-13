//! The format-provider contract: providers are pure transforms between file bytes and
//! canonical-document changes.

use tilo_content::{ContentInputs, ContentSource};
use tilo_doc::model::{DocMut, DocRef};

#[derive(thiserror::Error, Debug)]
pub enum CodecError {
    #[error("parse error: {0}")]
    Parse(String),
    #[error("export error: {0}")]
    Export(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
}

/// A format provider: a pure transform between file bytes and a canonical document. Operating over the borrowed [`DocMut`]/[`DocRef`] views
/// means the same codec works whether the document lives in an owned `Document` (FS) or a host's doc (editor).
///
/// Binary content (embedded images, binary package parts) never goes into the document: import
/// hands it back as [`ContentInputs`] for the caller to keep in its content store, and export reads
/// it back through a [`ContentSource`].
pub trait FormatProvider: Send + Sync {
    /// Project `bytes` into `doc`. Binary content the file embeds is added to `inputs`; the document
    /// only references it, so the caller must persist `inputs` before keeping the document.
    ///
    /// # Errors
    /// Returns [`CodecError`] if `bytes` are malformed, exceed the codec's
    /// resource bounds, or use a construct the codec refuses to import.
    fn import(
        &self,
        bytes: &[u8],
        doc: &mut DocMut,
        inputs: &mut ContentInputs,
    ) -> Result<(), CodecError>;
    /// Project `doc` into this format, reading the binary content it references from `content`.
    /// Content missing there is left out (an image renders as its placeholder) rather than failing
    /// the export.
    ///
    /// # Errors
    /// Returns [`CodecError`] if the document cannot be serialised into this
    /// format, or if writing its container fails.
    fn export(&self, doc: DocRef, content: &dyn ContentSource) -> Result<Vec<u8>, CodecError>;
}
