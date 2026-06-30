//! Built-in format providers bundled with `qdoc`, plus a registry preloaded
//! with them. Third-party apps register additional providers on top (Phase 3).

pub mod docx;
pub mod markdown;
pub mod odt;
pub mod rtf;

pub use docx::DocxCodec;
pub use markdown::MarkdownCodec;
pub use odt::OdtCodec;
pub use rtf::RtfCodec;

use crate::format::{ProviderTier, Registry};

/// A registry preloaded with the built-in codecs at `Builtin` tier.
pub fn builtin_registry() -> Registry {
    let mut r = Registry::new();
    r.register("md", ProviderTier::Builtin, Box::new(MarkdownCodec));
    r.register("markdown", ProviderTier::Builtin, Box::new(MarkdownCodec));
    r.register("odt", ProviderTier::Builtin, Box::new(OdtCodec));
    r.register("docx", ProviderTier::Builtin, Box::new(DocxCodec));
    // RTF is import-only via this codec (its `export` returns Unsupported; RTF
    // writing lives in the editor's `docrtf` crate). Registering it still makes
    // `.rtf` an openable document type.
    r.register("rtf", ProviderTier::Builtin, Box::new(RtfCodec));
    r
}

/// Extensions the built-in codecs handle (`md`, `markdown`, `odt`, `docx`,
/// `rtf`) — the OS source of truth for "this file is an editable document".
pub fn supported_extensions() -> Vec<String> {
    builtin_registry().extensions()
}

/// Whether `ext` (case-insensitive, no dot) is a built-in document format.
pub fn is_supported_ext(ext: &str) -> bool {
    builtin_registry().provider_for_ext(ext).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AttrVal, DocSnapshot, Document, FormatProvider};
    use std::collections::BTreeMap;

    type Norm = Vec<(String, String, Vec<(usize, usize, String)>, BTreeMap<String, AttrVal>)>;

    /// Structural view of a document, ignoring block ids (which differ per replica).
    fn norm(snap: &DocSnapshot) -> Norm {
        snap.blocks
            .iter()
            .map(|b| {
                let mut m: Vec<_> = b.marks.iter().map(|x| (x.start, x.end, x.name.clone())).collect();
                m.sort();
                (b.kind.clone(), b.text.clone(), m, b.attrs.clone())
            })
            .collect()
    }

    #[test]
    fn registry_resolves_builtin_codecs() {
        let r = builtin_registry();
        assert_eq!(r.provider_for_ext("odt").unwrap().schema_id(), crate::schema::SCHEMA_ID);
        assert_eq!(r.provider_for_ext("docx").unwrap().schema_id(), crate::schema::SCHEMA_ID);
        assert!(r.provider_for_ext("md").is_some());
        assert!(r.provider_for_ext("MD").is_some(), "extension lookup is case-insensitive");
        assert!(r.provider_for_ext("markdown").is_some());
        assert!(r.provider_for_ext("DOCX").is_some(), "extension lookup is case-insensitive");
        assert!(r.provider_for_ext("rtf").is_some(), "RTF import is now a built-in codec");
        assert!(r.provider_for_ext("RTF").is_some(), "extension lookup is case-insensitive");
        assert!(r.provider_for_ext("xyz").is_none(), "unregistered type resolves to None");
    }

    #[test]
    fn supported_extensions_is_registry_driven() {
        let exts = supported_extensions();
        for want in ["md", "markdown", "odt", "docx", "rtf"] {
            assert!(exts.iter().any(|e| e == want), "{want} should be a supported document format");
        }
        assert!(is_supported_ext("ODT"), "case-insensitive");
        assert!(is_supported_ext("RTF"), "RTF import is now supported (case-insensitive)");
        assert!(!is_supported_ext("xyz"), "an unregistered extension is not supported");
    }

    use proptest::prelude::*;

    /// Random documents over the subset both codecs model (headings, paragraphs,
    /// unordered list items, **strong** / *em*).
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
