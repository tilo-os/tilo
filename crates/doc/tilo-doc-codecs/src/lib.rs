//! `tilo-doc-codecs` — file formats as import/export projections of qdoc document resources.
//!
//! A document or workbook lives as a versioned `qdoc` resource; a `.docx`, `.odt`, `.md`,
//! `.html`, `.rtf`, `.xlsx`, `.ods` or `.epub` file is only a way into or out of it. This crate
//! holds those projections and [`provider_for_ext`], which picks one by file extension. Each
//! format sits behind a cargo feature of the same name, so an app compiles only the formats it
//! imports or exports; an app that only edits resources depends on `qdoc` alone.
//!
//! Office packages keep what they do not model: every part other than the one mapped onto the
//! model is carried through export unchanged (see the `docx` and `odt` modules).
//!
//! `.docx`, `.odt`, `.ods` and `.xlsx` are zip archives of XML 1.0
//! documents; every codec here reads them through the one `quick-xml` the
//! workspace pins, pulling text via `BytesText::xml10_content`, which
//! applies XML 1.0 end-of-line normalisation (`\r\n` and lone `\r` become
//! `\n`) — required by the XML 1.0 spec. Entity and character references are
//! not part of a text event: every reader that collects text also handles
//! `Event::GeneralRef` through `resolve_general_ref`. Neither OOXML nor
//! ODF carries line breaks as literal CR/LF (those are `<w:br/>` and
//! `<text:line-break/>` elements), so a raw CR in text content is whitespace
//! and folding it is correct. Use `xml11_content` only if a format ever
//! declares XML 1.1, which none of these do.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

#[cfg(feature = "markdown")]
pub mod convert;
pub mod format;
pub mod xml_escape;

#[cfg(feature = "docx")]
pub mod docx;
#[cfg(feature = "epub")]
pub mod export_epub;
#[cfg(feature = "html")]
pub mod export_html;
#[cfg(feature = "rtf")]
pub mod export_rtf;
#[cfg(feature = "html")]
pub mod html;
#[cfg(feature = "markdown")]
pub mod markdown;
#[cfg(feature = "ods")]
pub mod ods;
#[cfg(feature = "odt")]
pub mod odt;
#[cfg(any(feature = "docx", feature = "odt"))]
mod office_meta;
#[cfg(feature = "office")]
mod office_props;
#[cfg(feature = "office")]
pub mod package;
#[cfg(any(feature = "docx", feature = "odt"))]
mod para_styles;
#[cfg(feature = "office")]
pub mod preserve;
#[cfg(any(feature = "html", feature = "docx", feature = "odt"))]
mod resolved;
#[cfg(feature = "rtf")]
pub mod rtf;
#[cfg(feature = "office")]
pub mod safety;
#[cfg(any(feature = "xlsx", feature = "ods"))]
pub mod sheet_common;
#[cfg(feature = "xlsx")]
pub mod xlsx;

#[cfg(test)]
#[cfg(feature = "all")]
mod bulk_equivalence;

#[cfg(feature = "docx")]
pub use docx::DocxCodec;
pub use format::{CodecError, FormatProvider};
#[cfg(feature = "html")]
pub use html::HtmlCodec;
#[cfg(feature = "markdown")]
pub use markdown::MarkdownCodec;
#[cfg(feature = "ods")]
pub use ods::OdsCodec;
#[cfg(feature = "odt")]
pub use odt::OdtCodec;
#[cfg(feature = "rtf")]
pub use rtf::RtfCodec;
#[cfg(feature = "office")]
pub use safety::ZipLimits;
#[cfg(any(feature = "xlsx", feature = "ods"))]
pub use sheet_common::{ParsedSheet, ParsedWorkbook, SheetProvider};
#[cfg(feature = "xlsx")]
pub use xlsx::XlsxCodec;

#[cfg(feature = "office")]
/// Decode the UTF-8 attribute values required by OOXML and ODF and resolve XML
/// entities. Keeping this here avoids tying leaf parsers to `quick-xml`'s
/// optional legacy-encoding feature, which is unified across the workspace.
pub fn unescape_attr(attribute: &quick_xml::events::attributes::Attribute<'_>) -> Option<String> {
    let raw = std::str::from_utf8(attribute.value.as_ref()).ok()?;
    quick_xml::escape::unescape(raw)
        .ok()
        .map(std::borrow::Cow::into_owned)
}

#[cfg(feature = "office")]
/// The character an entity or character reference in text content stands for.
/// `quick-xml` reports `&amp;`, `&#62;` and friends as their own
/// [`Event::GeneralRef`](quick_xml::events::Event::GeneralRef) rather than
/// folding them into the surrounding text, so a reader that wants the text has
/// to put them back. Only the five predefined entities and numeric references
/// exist in these formats (a DTD, the only way to declare more, is refused).
///
/// Anything else — an undeclared name like `&bogus;`, or a character reference
/// to no character such as `&#0;` — makes the part not well-formed, and there
/// is no character to recover: it yields `None`, so readers drop the reference
/// and keep the text on either side. Readers that copy XML through verbatim
/// (unmodeled metadata) copy the reference as written instead.
pub fn resolve_general_ref(reference: &quick_xml::events::BytesRef<'_>) -> Option<char> {
    if let Ok(Some(c)) = reference.resolve_char_ref() {
        return Some(c);
    }
    match reference.decode().ok()?.as_ref() {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => None,
    }
}

#[cfg(any(
    feature = "markdown",
    feature = "html",
    feature = "docx",
    feature = "odt"
))]
/// The canonical `fontFamily` mark for a named font: `"mono"`, `"serif"` or `"sans"`. Documents
/// carry the font's class, not a face the reader may not have installed, so every importer
/// classifies a face the same way.
pub(crate) fn generic_font_family(name: &str) -> &'static str {
    let n = name.to_ascii_lowercase();
    if n.contains("mono") || n.contains("courier") || n.contains("consol") {
        "mono"
    } else if (n.contains("serif") && !n.contains("sans"))
        || n.contains("times")
        || n.contains("georgia")
        || n.contains("garamond")
        || n.contains("roman")
    {
        "serif"
    } else {
        "sans"
    }
}

/// Document formats the platform opens as an editable rich-text resource — the OS source of
/// truth for "this file is an editable document", independent of which codecs a given binary
/// compiles (a file manager asks this without linking any of them).
pub const DOCUMENT_EXTENSIONS: &[&str] = &["md", "markdown", "odt", "docx", "rtf", "html", "htm"];

/// The extension and media type an office package stores picture `bytes`
/// under: PNG, JPEG and GIF as themselves, anything else as a binary part.
#[cfg(any(feature = "docx", feature = "odt"))]
pub(crate) fn office_image_part(bytes: &[u8]) -> (&'static str, &'static str) {
    tilo_content::image_format(bytes)
        .filter(|format| matches!(format.extension, "png" | "jpg" | "gif"))
        .map_or(("bin", "application/octet-stream"), |format| {
            (format.extension, format.mime)
        })
}

/// The provider this build compiles for `ext` (case-insensitive), if any.
pub fn provider_for_ext(ext: &str) -> Option<&'static dyn FormatProvider> {
    match ext.to_ascii_lowercase().as_str() {
        #[cfg(feature = "markdown")]
        "md" | "markdown" => Some(&MarkdownCodec),
        #[cfg(feature = "odt")]
        "odt" => Some(&OdtCodec),
        #[cfg(feature = "docx")]
        "docx" => Some(&DocxCodec),
        // RTF and HTML import through these codecs; their `export` returns Unsupported (writing
        // is `export_rtf`/`export_html`, over a snapshot), but they make the files openable.
        #[cfg(feature = "rtf")]
        "rtf" => Some(&RtfCodec),
        #[cfg(feature = "html")]
        "html" | "htm" => Some(&HtmlCodec),
        _ => None,
    }
}

/// Whether `ext` (case-insensitive) is one of [`DOCUMENT_EXTENSIONS`].
pub fn is_supported_ext(ext: &str) -> bool {
    DOCUMENT_EXTENSIONS
        .iter()
        .any(|e| e.eq_ignore_ascii_case(ext))
}

#[cfg(test)]
#[cfg(feature = "all")]
mod tests {
    use super::*;
    use crate::FormatProvider;
    use std::collections::BTreeMap;
    use tilo_doc::{AttrVal, DocSnapshot, Document};

    type Norm = Vec<(
        String,
        String,
        Vec<(usize, usize, String)>,
        BTreeMap<String, AttrVal>,
    )>;

    /// Structural view of a document, ignoring block ids (which differ per replica).
    fn norm(snap: &DocSnapshot) -> Norm {
        snap.blocks
            .iter()
            .map(|b| {
                let mut m: Vec<_> = b
                    .marks
                    .iter()
                    .map(|x| (x.start, x.end, x.name.clone()))
                    .collect();
                m.sort();
                // What a codec keeps for its own export is not part of the canonical model.
                let mut attrs = b.attrs.clone();
                attrs.retain(|k, _| !crate::preserve::is_codec_private_attr(k));
                (b.kind.clone(), b.text.clone(), m, attrs)
            })
            .collect()
    }

    #[test]
    fn every_supported_extension_has_a_provider() {
        for ext in DOCUMENT_EXTENSIONS {
            assert!(provider_for_ext(ext).is_some(), "{ext} has no provider");
            assert!(
                provider_for_ext(&ext.to_ascii_uppercase()).is_some(),
                "extension lookup is case-insensitive"
            );
            assert!(is_supported_ext(&ext.to_ascii_uppercase()));
        }
        assert!(provider_for_ext("xyz").is_none());
        assert!(!is_supported_ext("xyz"));
    }

    use proptest::prelude::*;

    /// Random documents over the subset both codecs model (headings, paragraphs, unordered list items, **strong** / *em*).
    fn doc_strategy() -> impl Strategy<Value = String> {
        let inline = prop_oneof![
            "[a-z]{1,5}".prop_map(|w| w),
            "[a-z]{1,5}".prop_map(|w| format!("**{w}**")),
            "[a-z]{1,5}".prop_map(|w| format!("*{w}*")),
        ];
        let line = prop_oneof![
            proptest::collection::vec(inline, 1..4).prop_map(|ws| ws.join(" ")),
            "[a-z]{1,5}".prop_map(|w| format!("# {w}")),
            "[a-z]{1,5}".prop_map(|w| format!("- {w}")),
        ];
        proptest::collection::vec(line, 0..6).prop_map(|lines| lines.join("\n\n") + "\n")
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

        /// A document built from Markdown survives a full `.odt` export+import
        /// with an identical model — the cross-codec canonical-model guarantee.
        #[test]
        fn odt_preserves_markdown_model(src in doc_strategy()) {
            let mut d = Document::new();
            MarkdownCodec.import(src.as_bytes(), &mut d.as_mut(), &mut tilo_content::ContentInputs::new()).unwrap();
            let m1 = norm(&d.materialize());

            let odt = OdtCodec.export(d.as_ref(), &tilo_content::NoContent).unwrap();
            let mut d2 = Document::new();
            OdtCodec.import(&odt, &mut d2.as_mut(), &mut tilo_content::ContentInputs::new()).unwrap();
            let m2 = norm(&d2.materialize());

            prop_assert_eq!(m1, m2);
        }

        /// The same guarantee for `.docx`: a Markdown-built document round-trips
        /// through a full `.docx` export+import with an identical model.
        #[test]
        fn docx_preserves_markdown_model(src in doc_strategy()) {
            let mut d = Document::new();
            MarkdownCodec.import(src.as_bytes(), &mut d.as_mut(), &mut tilo_content::ContentInputs::new()).unwrap();
            let m1 = norm(&d.materialize());

            let docx = DocxCodec.export(d.as_ref(), &tilo_content::NoContent).unwrap();
            let mut d2 = Document::new();
            DocxCodec.import(&docx, &mut d2.as_mut(), &mut tilo_content::ContentInputs::new()).unwrap();
            let m2 = norm(&d2.materialize());

            prop_assert_eq!(m1, m2);
        }
    }
}
