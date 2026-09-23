//! Built-in format providers bundled with `qdoc`, plus a registry preloaded
//! with them. Third-party apps register additional providers on top.
//!
//! `.docx`, `.odt`, `.ods`, `.xlsx` and `.odp` are zip archives of XML 1.0
//! documents; every codec here reads them through the one `quick-xml` the
//! workspace pins, pulling text via [`BytesText::xml10_content`], which
//! applies XML 1.0 end-of-line normalisation (`\r\n` and lone `\r` become
//! `\n`) — required by the XML 1.0 spec. Entity and character references are
//! not part of a text event: every reader that collects text also handles
//! `Event::GeneralRef` through [`resolve_general_ref`]. Neither OOXML nor
//! ODF carries line breaks as literal CR/LF (those are `<w:br/>` and
//! `<text:line-break/>` elements), so a raw CR in text content is whitespace
//! and folding it is correct. Use `xml11_content` only if a format ever
//! declares XML 1.1, which none of these do.

pub mod docx;
pub mod html;
pub mod markdown;
pub mod ods;
pub mod odt;
pub mod rtf;
pub mod safety;
pub mod sheet_common;
pub mod xlsx;
pub mod xml_escape;

pub use docx::DocxCodec;
pub use html::HtmlCodec;
pub use markdown::MarkdownCodec;
pub use ods::OdsCodec;
pub use odt::OdtCodec;
pub use rtf::RtfCodec;
pub use sheet_common::{ParsedSheet, ParsedWorkbook, SheetProvider};
pub use xlsx::XlsxCodec;

use crate::format::{ProviderTier, Registry};

/// Decode the UTF-8 attribute values required by OOXML and ODF and resolve XML
/// entities. Keeping this here avoids tying leaf parsers to `quick-xml`'s
/// optional legacy-encoding feature, which is unified across the workspace.
pub(crate) fn unescape_attr(
    attribute: &quick_xml::events::attributes::Attribute<'_>,
) -> Option<String> {
    let raw = std::str::from_utf8(attribute.value.as_ref()).ok()?;
    quick_xml::escape::unescape(raw)
        .ok()
        .map(std::borrow::Cow::into_owned)
}

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
pub(crate) fn resolve_general_ref(reference: &quick_xml::events::BytesRef<'_>) -> Option<char> {
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

/// The spreadsheet codec for a file extension (`xlsx`/`ods`, case-insensitive) — separate
/// from [`builtin_registry`] since spreadsheets speak [`SheetProvider`], not [`crate::format::FormatProvider`].
pub fn sheet_provider_for_ext(ext: &str) -> Option<Box<dyn SheetProvider>> {
    match ext.to_ascii_lowercase().as_str() {
        "xlsx" => Some(Box::new(XlsxCodec)),
        "ods" => Some(Box::new(OdsCodec)),
        _ => None,
    }
}

pub fn is_supported_sheet_ext(ext: &str) -> bool {
    sheet_provider_for_ext(ext).is_some()
}

pub fn builtin_registry() -> Registry {
    let mut r = Registry::new();
    r.register("md", ProviderTier::Builtin, Box::new(MarkdownCodec));
    r.register("markdown", ProviderTier::Builtin, Box::new(MarkdownCodec));
    r.register("odt", ProviderTier::Builtin, Box::new(OdtCodec));
    r.register("docx", ProviderTier::Builtin, Box::new(DocxCodec));
    // RTF is import-only via this codec (`export` returns Unsupported; writing is
    // `export_rtf`, over a snapshot) — registering it still makes `.rtf` openable.
    r.register("rtf", ProviderTier::Builtin, Box::new(RtfCodec));
    // HTML is import-only via this codec (`export` returns Unsupported; writing is
    // `export_html`, over a snapshot) — this makes `.html`/`.htm` openable.
    r.register("html", ProviderTier::Builtin, Box::new(HtmlCodec));
    r.register("htm", ProviderTier::Builtin, Box::new(HtmlCodec));
    r
}

/// Extensions the built-in codecs handle — the OS source of truth for "this file is an editable document".
pub fn supported_extensions() -> Vec<String> {
    builtin_registry().extensions()
}

pub fn is_supported_ext(ext: &str) -> bool {
    builtin_registry().provider_for_ext(ext).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AttrVal, DocSnapshot, Document, FormatProvider};
    use std::collections::BTreeMap;

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
                (b.kind.clone(), b.text.clone(), m, b.attrs.clone())
            })
            .collect()
    }

    #[test]
    fn registry_resolves_builtin_codecs() {
        let r = builtin_registry();
        assert_eq!(
            r.provider_for_ext("odt").unwrap().schema_id(),
            crate::schema::SCHEMA_ID
        );
        assert_eq!(
            r.provider_for_ext("docx").unwrap().schema_id(),
            crate::schema::SCHEMA_ID
        );
        assert!(r.provider_for_ext("md").is_some());
        assert!(
            r.provider_for_ext("MD").is_some(),
            "extension lookup is case-insensitive"
        );
        assert!(r.provider_for_ext("markdown").is_some());
        assert!(
            r.provider_for_ext("DOCX").is_some(),
            "extension lookup is case-insensitive"
        );
        assert!(
            r.provider_for_ext("rtf").is_some(),
            "RTF import is now a built-in codec"
        );
        assert!(
            r.provider_for_ext("RTF").is_some(),
            "extension lookup is case-insensitive"
        );
        assert!(
            r.provider_for_ext("html").is_some(),
            "HTML import is now a built-in codec"
        );
        assert!(
            r.provider_for_ext("htm").is_some(),
            "the .htm alias is registered too"
        );
        assert!(
            r.provider_for_ext("HTML").is_some(),
            "extension lookup is case-insensitive"
        );
        assert!(
            r.provider_for_ext("xyz").is_none(),
            "unregistered type resolves to None"
        );
    }

    #[test]
    fn supported_extensions_is_registry_driven() {
        let exts = supported_extensions();
        for want in ["md", "markdown", "odt", "docx", "rtf", "html", "htm"] {
            assert!(
                exts.iter().any(|e| e == want),
                "{want} should be a supported document format"
            );
        }
        assert!(is_supported_ext("ODT"), "case-insensitive");
        assert!(
            is_supported_ext("RTF"),
            "RTF import is now supported (case-insensitive)"
        );
        assert!(
            is_supported_ext("HTML"),
            "HTML import is now supported (case-insensitive)"
        );
        assert!(
            is_supported_ext("HTM"),
            "the .htm alias is supported (case-insensitive)"
        );
        assert!(
            !is_supported_ext("xyz"),
            "an unregistered extension is not supported"
        );
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
            MarkdownCodec.import(src.as_bytes(), &mut d.as_mut()).unwrap();
            let m1 = norm(&d.materialize());

            let odt = OdtCodec.export(d.as_ref()).unwrap();
            let mut d2 = Document::new();
            OdtCodec.import(&odt, &mut d2.as_mut()).unwrap();
            let m2 = norm(&d2.materialize());

            prop_assert_eq!(m1, m2);
        }

        /// The same guarantee for `.docx`: a Markdown-built document round-trips
        /// through a full `.docx` export+import with an identical model.
        #[test]
        fn docx_preserves_markdown_model(src in doc_strategy()) {
            let mut d = Document::new();
            MarkdownCodec.import(src.as_bytes(), &mut d.as_mut()).unwrap();
            let m1 = norm(&d.materialize());

            let docx = DocxCodec.export(d.as_ref()).unwrap();
            let mut d2 = Document::new();
            DocxCodec.import(&docx, &mut d2.as_mut()).unwrap();
            let m2 = norm(&d2.materialize());

            prop_assert_eq!(m1, m2);
        }
    }
}
