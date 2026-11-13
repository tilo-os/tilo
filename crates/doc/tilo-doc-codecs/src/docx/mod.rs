//! `.docx` (OOXML `WordprocessingML`) codec. Lossless-preserving: OPC package parts are
//! carried verbatim except `word/document.xml`, which maps onto the canonical model.
//! Mirrors the `.odt` codec: container → styles → body parse → spans → export → preservation.

pub mod columns;
pub mod comments;
#[cfg(test)]
mod container;
pub mod content;
pub mod export;
pub mod headers;
pub mod meta;
pub mod notes;
mod paragraph;
mod section;
mod side_part;
pub mod styles;

use crate::format::{CodecError, FormatProvider};
use tilo_content::{ContentInputs, ContentSource};
use tilo_doc::model::{DocMut, DocRef};

pub(crate) const DOCUMENT_ROOT_KEY: &str = "docx/document-root";

pub struct DocxCodec;

impl FormatProvider for DocxCodec {
    fn import(
        &self,
        bytes: &[u8],
        doc: &mut DocMut,
        inputs: &mut ContentInputs,
    ) -> Result<(), CodecError> {
        doc.bulk(|doc| {
            let container = crate::package::ZipPackage::read(bytes)?;
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
            let mut foot_map = std::collections::BTreeMap::new();
            let mut end_map = std::collections::BTreeMap::new();
            if let Some(xml) = container.get("word/footnotes.xml") {
                foot_map = notes::import_notes_part(xml, tilo_doc::model::NoteKind::Footnote, doc);
            }
            if let Some(xml) = container.get("word/endnotes.xml") {
                end_map = notes::import_notes_part(xml, tilo_doc::model::NoteKind::Endnote, doc);
            }
            let comment_map = container
                .get(comments::COMMENTS_PART)
                .map(|xml| comments::import_comments_part(xml, doc))
                .unwrap_or_default();
            let image_map = content::build_image_map(&container);
            let link_map = content::build_link_map(&container);
            let maps = content::RefMaps {
                foot: &foot_map,
                end: &end_map,
                comment: &comment_map,
                links: &link_map,
            };
            content::parse_body(&document, &reg, &maps, &image_map, doc, inputs)?;
            if let Some(core_xml) = container.get("docProps/core.xml") {
                meta::import_core(core_xml, doc);
            }
            if let Some(app_xml) = container.get("docProps/app.xml") {
                meta::import_app(app_xml, doc);
            }
            let rels = container.get("word/_rels/document.xml.rels");
            let (hdr_part, ftr_part) = headers::resolve_default_parts(&document, rels);
            if let Some(part) = hdr_part
                && let Some(xml) = container.get(&part)
            {
                headers::import_region(xml, tilo_doc::model::Region::Header, doc);
            }
            if let Some(part) = ftr_part
                && let Some(xml) = container.get(&part)
            {
                headers::import_region(xml, tilo_doc::model::Region::Footer, doc);
            }
            columns::import_section_columns(&document, doc);
            for (name, part) in &container.parts {
                if name != "word/document.xml" {
                    crate::preserve::set_preserved(doc, &format!("docx/part/{name}"), part, inputs);
                }
            }
            if let Some(root) = crate::preserve::root_start_tag(&document) {
                crate::preserve::set_preserved(doc, DOCUMENT_ROOT_KEY, root.as_bytes(), inputs);
            }
            Ok(())
        })
    }

    fn export(&self, doc: DocRef, content: &dyn ContentSource) -> Result<Vec<u8>, CodecError> {
        export::export(doc, content)
    }
}
