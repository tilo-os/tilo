//! `.odt` (OpenDocument Text) codec. Lossless-preserving: the ODF container's
//! parts are carried verbatim except the `content.xml` body, which maps onto the
//! canonical model. Built across Phase 4: container → body parse → spans →
//! export → preservation.

pub mod container;
pub mod content;
pub mod export;

use crate::format::{CodecError, Confidence, FormatProvider};
use crate::model::{DocMut, DocRef};
use crate::schema::SCHEMA_ID;

pub struct OdtCodec;

impl FormatProvider for OdtCodec {
    fn schema_id(&self) -> &str {
        SCHEMA_ID
    }

    fn detect(&self, head: &[u8]) -> Confidence {
        // ODF is a zip ("PK..") whose first stored entry is the mimetype.
        if head.starts_with(b"PK") {
            Confidence::Low
        } else {
            Confidence::None
        }
    }

    fn import(&self, bytes: &[u8], doc: &mut DocMut) -> Result<(), CodecError> {
        let container = container::OdtContainer::read(bytes)?;
        let content = container
            .get("content.xml")
            .ok_or_else(|| CodecError::Parse("odt: missing content.xml".into()))?
            .to_vec();
        content::parse_body(&content, doc)?;
        // Preservation layer: stash every part except the (regenerated)
        // content.xml verbatim so export round-trips them losslessly.
        for (name, part) in &container.parts {
            if name != "content.xml" {
                doc.set_preserved(&format!("odt/part/{name}"), part);
            }
        }
        Ok(())
    }

    fn export(&self, doc: DocRef) -> Result<Vec<u8>, CodecError> {
        export::export(doc)
    }
}
