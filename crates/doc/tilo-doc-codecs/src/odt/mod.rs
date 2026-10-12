//! `.odt` (`OpenDocument` Text) codec. Lossless-preserving: the ODF container's
//! parts are carried verbatim except the `content.xml` body, which maps onto the
//! canonical model.

pub mod changes;
pub mod columns;
pub mod comments;
pub mod container;
pub mod content;
pub mod export;
pub mod headers;
pub mod language;
pub mod meta;
pub mod notes;
mod plain_paragraphs;
pub mod styles;

use crate::format::{CodecError, Confidence, FormatProvider};
use tilo_content::{ContentInputs, ContentSource};
use tilo_doc::model::{DocMut, DocRef};
use tilo_doc::schema::SCHEMA_ID;

/// Preservation-layer keys of the imported `content.xml` root start tag, the declarations
/// before its automatic styles, and the automatic styles' content.
pub(crate) const CONTENT_ROOT_KEY: &str = "odt/content-root";
pub(crate) const CONTENT_PRELUDE_KEY: &str = "odt/content-prelude";
pub(crate) const CONTENT_AUTOMATIC_STYLES_KEY: &str = "odt/content-automatic-styles";

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

    fn import(
        &self,
        bytes: &[u8],
        doc: &mut DocMut,
        inputs: &mut ContentInputs,
    ) -> Result<(), CodecError> {
        doc.bulk(|doc| {
            let container = container::OdtContainer::read(bytes)?;
            let content = container
                .get("content.xml")
                .ok_or_else(|| CodecError::Parse("odt: missing content.xml".into()))?
                .to_vec();
            // Build the style registry from styles.xml (named) + content.xml (automatic); later styles win, so content overrides styles.xml.
            let mut reg = styles::StyleRegistry::default();
            if let Some(styles_xml) = container.get("styles.xml") {
                styles::parse_styles(styles_xml, &mut reg, true);
            }
            // content.xml holds automatic (anonymous) styles, named only if they carry an explicit display-name (e.g. styles this codec itself exports).
            styles::parse_styles(&content, &mut reg, false);
            // The container is threaded through so `draw:frame`/`draw:image` can read the
            // referenced `Pictures/*` bytes and so named paragraph styles are lifted into the qdoc styles container.
            content::parse_body(&content, &reg, &container, doc, inputs)?;
            if let Some(meta_xml) = container.get("meta.xml") {
                meta::import_meta(meta_xml, doc);
            }
            // Page header/footer: parse the master-page `style:header`/`style:footer` paragraphs (incl. `text:page-number` fields) from `styles.xml`.
            if let Some(styles_xml) = container.get("styles.xml") {
                headers::import_master_headers(styles_xml, doc);
            }
            if let Some(styles_xml) = container.get("styles.xml") {
                columns::import_page_layout_columns(styles_xml, doc);
            }
            // Document default language: parse the paragraph-family `style:default-style`'s `fo:language`/`fo:country` from `styles.xml`.
            if let Some(styles_xml) = container.get("styles.xml") {
                language::import_default_language(styles_xml, doc);
            }
            // Preservation layer: stash every part except the (regenerated) content.xml verbatim so
            // export round-trips them losslessly; meta.xml is included so its unmodeled fields survive.
            for (name, part) in &container.parts {
                if name != "content.xml" {
                    crate::preserve::set_preserved(doc, &format!("odt/part/{name}"), part, inputs);
                }
            }
            // What surrounds the body in content.xml is written back around the regenerated
            // body: the body content carried through verbatim refers to it.
            if let Some(head) = content::content_head(&content) {
                crate::preserve::set_preserved(doc, CONTENT_ROOT_KEY, head.root.as_bytes(), inputs);
                crate::preserve::set_preserved(
                    doc,
                    CONTENT_PRELUDE_KEY,
                    head.prelude.as_bytes(),
                    inputs,
                );
                crate::preserve::set_preserved(
                    doc,
                    CONTENT_AUTOMATIC_STYLES_KEY,
                    head.automatic_styles.as_bytes(),
                    inputs,
                );
            }
            Ok(())
        })
    }

    fn export(&self, doc: DocRef, content: &dyn ContentSource) -> Result<Vec<u8>, CodecError> {
        export::export(doc, content)
    }
}
