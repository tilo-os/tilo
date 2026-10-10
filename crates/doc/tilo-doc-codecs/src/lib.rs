//! `tilo-doc-codecs` — file formats as import/export projections of qdoc document resources.
//!
//! A document or workbook lives as a versioned `qdoc` resource; a `.docx`, `.odt`, `.md`,
//! `.html`, `.rtf`, `.xlsx`, `.ods` or `.epub` file is only a way into or out of it. This crate
//! holds those projections and the [`FormatProvider`] registry, which is also the extension
//! point third-party (wasm) providers plug into. Each format sits behind a cargo feature of the
//! same name, so an app compiles only the formats it imports or exports; an app that only edits
//! resources depends on `qdoc` alone.
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
pub use format::{CodecError, Confidence, FormatProvider, ProviderTier, Registry};
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

/// Spreadsheet formats the platform opens as a workbook resource, whatever this build compiles.
pub const SHEET_EXTENSIONS: &[&str] = &["xlsx", "ods"];

/// Document formats the platform opens as an editable rich-text resource — the OS source of
/// truth for "this file is an editable document", independent of which codecs a given binary
/// compiles (a file manager asks this without linking any of them).
pub const DOCUMENT_EXTENSIONS: &[&str] = &["md", "markdown", "odt", "docx", "rtf", "html", "htm"];

/// A registry preloaded with the built-in providers this build enables. Third-party providers
/// register on top.
pub fn builtin_registry() -> Registry {
    #[allow(
        unused_mut,
        reason = "a build with no format features registers nothing"
    )]
    let mut r = Registry::new();
    #[cfg(feature = "markdown")]
    {
        r.register("md", ProviderTier::Builtin, Box::new(MarkdownCodec));
        r.register("markdown", ProviderTier::Builtin, Box::new(MarkdownCodec));
    }
    #[cfg(feature = "odt")]
    r.register("odt", ProviderTier::Builtin, Box::new(OdtCodec));
    #[cfg(feature = "docx")]
    r.register("docx", ProviderTier::Builtin, Box::new(DocxCodec));
    // RTF is import-only via this codec (`export` returns Unsupported; writing is
    // `export_rtf`, over a snapshot) — registering it still makes `.rtf` openable.
    #[cfg(feature = "rtf")]
    r.register("rtf", ProviderTier::Builtin, Box::new(RtfCodec));
    // HTML is import-only via this codec (`export` returns Unsupported; writing is
    // `export_html`, over a snapshot) — this makes `.html`/`.htm` openable.
    #[cfg(feature = "html")]
    {
        r.register("html", ProviderTier::Builtin, Box::new(HtmlCodec));
        r.register("htm", ProviderTier::Builtin, Box::new(HtmlCodec));
    }
    r
}

/// Extensions of [`DOCUMENT_EXTENSIONS`], owned.
pub fn supported_extensions() -> Vec<String> {
    DOCUMENT_EXTENSIONS
        .iter()
        .map(|e| (*e).to_string())
        .collect()
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
    fn builtin_codec_registry_covers_every_supported_extension() {
        // Phase `registry_resolves_builtin_codecs`.
        {
            let r = builtin_registry();
            assert_eq!(
                r.provider_for_ext("odt").unwrap().schema_id(),
                tilo_doc::schema::SCHEMA_ID
            );
            assert_eq!(
                r.provider_for_ext("docx").unwrap().schema_id(),
                tilo_doc::schema::SCHEMA_ID
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
        // Phase `supported_extensions_match_the_full_registry`.
        {
            let exts = supported_extensions();
            let mut registered = builtin_registry().extensions();
            registered.sort();
            let mut declared = exts.clone();
            declared.sort();
            assert_eq!(
                registered, declared,
                "with every format enabled, the registry is exactly DOCUMENT_EXTENSIONS"
            );
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
