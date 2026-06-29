//! `.docx` (OOXML WordprocessingML) codec. Lossless-preserving: the OPC
//! package's parts are carried verbatim except `word/document.xml`, which maps
//! onto the canonical model. Mirrors the `.odt` codec: container → styles →
//! body parse → spans → export → preservation.

pub mod container;
pub mod content;
pub mod export;
pub mod styles;

use crate::format::{CodecError, Confidence, FormatProvider};
use crate::model::{DocMut, DocRef};
use crate::schema::SCHEMA_ID;

pub struct DocxCodec;

impl FormatProvider for DocxCodec {
    fn schema_id(&self) -> &str {
        SCHEMA_ID
    }

    fn detect(&self, head: &[u8]) -> Confidence {
        // OOXML is an OPC zip ("PK.."). The extension drives selection; this is a
        // weak content hint only (shared with other zip-based formats).
        if head.starts_with(b"PK") {
            Confidence::Low
        } else {
            Confidence::None
        }
    }

    fn import(&self, bytes: &[u8], doc: &mut DocMut) -> Result<(), CodecError> {
        let container = container::DocxContainer::read(bytes)?;
        let document = container
            .get("word/document.xml")
            .ok_or_else(|| CodecError::Parse("docx: missing word/document.xml".into()))?
            .to_vec();
        // Build the style + numbering registry the body parser resolves against.
        let mut reg = styles::StyleRegistry::default();
        if let Some(styles_xml) = container.get("word/styles.xml") {
            styles::parse_styles(styles_xml, &mut reg);
        }
        if let Some(numbering_xml) = container.get("word/numbering.xml") {
            styles::parse_numbering(numbering_xml, &mut reg);
        }
        content::parse_body(&document, &reg, doc)?;
        // Preservation layer: stash every part except the (regenerated)
        // word/document.xml verbatim so export round-trips them losslessly.
        for (name, part) in &container.parts {
            if name != "word/document.xml" {
                doc.set_preserved(&format!("docx/part/{name}"), part);
            }
        }
        Ok(())
    }

    fn export(&self, doc: DocRef) -> Result<Vec<u8>, CodecError> {
        export::export(doc)
    }
}
