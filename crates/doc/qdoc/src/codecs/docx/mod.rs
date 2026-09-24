//! `.docx` (OOXML `WordprocessingML`) codec. Lossless-preserving: OPC package parts are
//! carried verbatim except `word/document.xml`, which maps onto the canonical model.
//! Mirrors the `.odt` codec: container → styles → body parse → spans → export → preservation.

pub mod columns;
pub mod comments;
pub mod container;
pub mod content;
pub mod export;
pub mod headers;
pub mod meta;
pub mod notes;
mod section;
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
        // OOXML is an OPC zip ("PK.."); a weak content hint shared with other zip-based formats.
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
        let mut reg = styles::StyleRegistry::default();
        if let Some(styles_xml) = container.get("word/styles.xml") {
            styles::parse_styles(styles_xml, &mut reg);
            if let Some(lang) = styles::doc_default_language(styles_xml) {
                doc.set_language(&lang);
            }
        }
        if let Some(numbering_xml) = container.get("word/numbering.xml") {
            styles::parse_numbering(numbering_xml, &mut reg);
        }
        // Footnotes/endnotes are parsed first so the body parser can wire references onto the
        // canonical note via the returned `docx-id → note-id` maps.
        let mut foot_map = std::collections::BTreeMap::new();
        let mut end_map = std::collections::BTreeMap::new();
        if let Some(xml) = container.get("word/footnotes.xml") {
            foot_map = notes::import_notes_part(xml, crate::model::NoteKind::Footnote, doc);
        }
        if let Some(xml) = container.get("word/endnotes.xml") {
            end_map = notes::import_notes_part(xml, crate::model::NoteKind::Endnote, doc);
        }
        // Comments are parsed first so the body parser can wire range markers onto the
        // canonical comment via the returned `docx-id → comment-id` map.
        let mut comment_map = std::collections::BTreeMap::new();
        if let Some(xml) = container.get(comments::COMMENTS_PART) {
            comment_map = comments::import_comments_part(xml, doc);
        }
        // Resolve each `r:embed` rel id to its media part's bytes for the body parser to store.
        let image_map = content::build_image_map(&container);
        content::parse_body(
            &document,
            &reg,
            &foot_map,
            &end_map,
            &comment_map,
            &image_map,
            doc,
        )?;
        // Document properties from `docProps/core.xml` + `docProps/app.xml` into `meta.props`.
        if let Some(core_xml) = container.get("docProps/core.xml") {
            meta::import_core(core_xml, doc);
        }
        if let Some(app_xml) = container.get("docProps/app.xml") {
            meta::import_app(app_xml, doc);
        }
        // Resolve the default header/footer parts referenced by the body's `w:sectPr` and parse
        // each (incl. PAGE fields) into the canonical regions.
        let rels = container.get("word/_rels/document.xml.rels");
        let (hdr_part, ftr_part) = headers::resolve_default_parts(&document, rels);
        if let Some(part) = hdr_part
            && let Some(xml) = container.get(&part)
        {
            headers::import_region(xml, crate::model::Region::Header, doc);
        }
        if let Some(part) = ftr_part
            && let Some(xml) = container.get(&part)
        {
            headers::import_region(xml, crate::model::Region::Footer, doc);
        }
        // Parse the body `w:sectPr`'s `w:cols` num/space into the document-wide column setting.
        columns::import_section_columns(&document, doc);
        // Preservation layer: stash every part except the regenerated word/document.xml verbatim,
        // including docProps (export merges canonical properties back into a copy of them).
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
