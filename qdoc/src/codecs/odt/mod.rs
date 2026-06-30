//! `.odt` (OpenDocument Text) codec. Lossless-preserving: the ODF container's
//! parts are carried verbatim except the `content.xml` body, which maps onto the
//! canonical model. Built across Phase 4: container → body parse → spans →
//! export → preservation.

pub mod comments;
pub mod container;
pub mod content;
pub mod export;
pub mod headers;
pub mod meta;
pub mod notes;
pub mod styles;

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
        // Build the style registry from styles.xml (named styles) + content.xml
        // (automatic styles); later styles win, so content overrides styles.xml.
        let mut reg = styles::StyleRegistry::default();
        if let Some(styles_xml) = container.get("styles.xml") {
            // styles.xml holds common/named styles → flag them as named.
            styles::parse_styles(styles_xml, &mut reg, true);
        }
        // content.xml holds automatic (anonymous) styles → not named unless they
        // carry an explicit display-name (e.g. styles this codec itself exports).
        styles::parse_styles(&content, &mut reg, false);
        // The container is threaded through so `draw:frame`/`draw:image` can read
        // the referenced `Pictures/*` bytes and store them as image-block blobs,
        // and so named paragraph styles are lifted into the qdoc styles container.
        content::parse_body(&content, &reg, &container, doc)?;
        // Document properties: parse `meta.xml`'s `office:meta` fields (dc:title,
        // dc:creator, …) into the canonical `meta.props` map.
        if let Some(meta_xml) = container.get("meta.xml") {
            meta::import_meta(meta_xml, doc);
        }
        // Page header/footer: parse the master-page `style:header`/`style:footer`
        // paragraphs (incl. `text:page-number` fields) from `styles.xml` into the
        // canonical header/footer regions.
        if let Some(styles_xml) = container.get("styles.xml") {
            headers::import_master_headers(styles_xml, doc);
        }
        // Preservation layer: stash every part except the (regenerated)
        // content.xml verbatim so export round-trips them losslessly. `meta.xml`
        // is preserved too so its unmodeled fields survive (export merges the
        // canonical properties back into a copy of it).
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
