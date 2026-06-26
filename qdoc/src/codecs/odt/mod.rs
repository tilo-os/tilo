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
            .ok_or_else(|| CodecError::Parse("odt: missing content.xml".into()))?;
        content::parse_body(content, doc)?;
        // Verbatim part preservation for lossless export lands in Phase 4 Task 5.
        Ok(())
    }

    fn export(&self, doc: DocRef) -> Result<Vec<u8>, CodecError> {
        export::export(doc)
    }
}
