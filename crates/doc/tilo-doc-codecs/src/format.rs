//! Layer-1 generic format-provider contract + registry: providers are pure transforms between
//! file bytes and canonical-document changes; in production they run sandboxed.

use tilo_content::{ContentInputs, ContentSource};
use tilo_doc::model::{DocMut, DocRef};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    None,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProviderTier {
    Ephemeral = 0,
    Installed = 1,
    Builtin = 2,
}

#[derive(thiserror::Error, Debug)]
pub enum CodecError {
    #[error("parse error: {0}")]
    Parse(String),
    #[error("export error: {0}")]
    Export(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
}

/// A format provider: a pure transform between file bytes and a canonical document, run behind a
/// sandbox IPC boundary in production. Operating over the borrowed [`DocMut`]/[`DocRef`] views
/// means the same codec works whether the document lives in an owned `Document` (FS) or a host's doc (editor).
///
/// Binary content (embedded images, binary package parts) never goes into the document: import
/// hands it back as [`ContentInputs`] for the caller to keep in its content store, and export reads
/// it back through a [`ContentSource`].
pub trait FormatProvider: Send + Sync {
    fn schema_id(&self) -> &str;
    fn detect(&self, head: &[u8]) -> Confidence;
    /// Project `bytes` into `doc`. Binary content the file embeds is added to `inputs`; the document
    /// only references it, so the caller must persist `inputs` before keeping the document.
    ///
    /// # Errors
    /// Returns [`CodecError`] if `bytes` are malformed, exceed the codec's
    /// resource bounds, or use a construct the codec refuses to import.
    fn import(
        &self,
        bytes: &[u8],
        doc: &mut DocMut,
        inputs: &mut ContentInputs,
    ) -> Result<(), CodecError>;
    /// Project `doc` into this format, reading the binary content it references from `content`.
    /// Content missing there is left out (an image renders as its placeholder) rather than failing
    /// the export.
    ///
    /// # Errors
    /// Returns [`CodecError`] if the document cannot be serialised into this
    /// format, or if writing its container fails.
    fn export(&self, doc: DocRef, content: &dyn ContentSource) -> Result<Vec<u8>, CodecError>;
}

struct Entry {
    tier: ProviderTier,
    provider: Box<dyn FormatProvider>,
}

#[derive(Default)]
pub struct Registry {
    by_ext: std::collections::HashMap<String, Vec<Entry>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, ext: &str, tier: ProviderTier, provider: Box<dyn FormatProvider>) {
        self.by_ext
            .entry(ext.to_lowercase())
            .or_default()
            .push(Entry { tier, provider });
    }

    /// Highest-tier provider registered for `ext` (Builtin > Installed > Ephemeral).
    pub fn provider_for_ext(&self, ext: &str) -> Option<&dyn FormatProvider> {
        self.by_ext
            .get(&ext.to_lowercase())?
            .iter()
            .max_by_key(|e| e.tier)
            .map(|e| e.provider.as_ref())
    }

    /// Every registered extension (lowercased), sorted; the OS uses this as the source of truth for which files are editable documents.
    pub fn extensions(&self) -> Vec<String> {
        let mut v: Vec<String> = self.by_ext.keys().cloned().collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_doc::Document;

    struct DummyMd;
    impl FormatProvider for DummyMd {
        fn schema_id(&self) -> &str {
            tilo_doc::schema::SCHEMA_ID
        }
        fn detect(&self, head: &[u8]) -> Confidence {
            if head.starts_with(b"# ") {
                Confidence::High
            } else {
                Confidence::Low
            }
        }
        fn import(
            &self,
            bytes: &[u8],
            doc: &mut DocMut,
            _: &mut ContentInputs,
        ) -> Result<(), CodecError> {
            let s = std::str::from_utf8(bytes).map_err(|e| CodecError::Parse(e.to_string()))?;
            let p = doc.insert_block(None, "paragraph");
            doc.splice_text(&p, 0, 0, s.trim());
            Ok(())
        }
        fn export(&self, doc: DocRef, _: &dyn ContentSource) -> Result<Vec<u8>, CodecError> {
            let snap = doc.materialize();
            Ok(snap
                .blocks
                .iter()
                .map(|b| b.text.clone())
                .collect::<Vec<_>>()
                .join("\n")
                .into_bytes())
        }
    }

    struct BuiltinMd;
    impl FormatProvider for BuiltinMd {
        fn schema_id(&self) -> &str {
            tilo_doc::schema::SCHEMA_ID
        }
        fn detect(&self, _: &[u8]) -> Confidence {
            Confidence::High
        }
        fn import(
            &self,
            _: &[u8],
            _: &mut DocMut,
            _: &mut ContentInputs,
        ) -> Result<(), CodecError> {
            Ok(())
        }
        fn export(&self, _: DocRef, _: &dyn ContentSource) -> Result<Vec<u8>, CodecError> {
            Ok(b"builtin".to_vec())
        }
    }

    #[test]
    fn format_providers_detect_resolve_and_roundtrip() {
        // Phase `import_export_roundtrip_via_provider`.
        {
            let p = DummyMd;
            let mut d = Document::new();
            p.import(
                b"# Hello",
                &mut d.as_mut(),
                &mut tilo_content::ContentInputs::new(),
            )
            .unwrap();
            let out = p.export(d.as_ref(), &tilo_content::NoContent).unwrap();
            assert_eq!(out, b"# Hello");
        }
        // Phase `registry_resolves_highest_tier`.
        {
            let mut r = Registry::new();
            r.register("md", ProviderTier::Ephemeral, Box::new(DummyMd));
            r.register("md", ProviderTier::Builtin, Box::new(BuiltinMd));
            let prov = r.provider_for_ext("md").unwrap();
            // Builtin wins → its export returns b"builtin".
            let d = Document::new();
            assert_eq!(
                prov.export(d.as_ref(), &tilo_content::NoContent).unwrap(),
                b"builtin"
            );
        }
        // Phase `detect_distinguishes_content`.
        {
            let p = DummyMd;
            assert_eq!(p.detect(b"# Title"), Confidence::High);
            assert_eq!(p.detect(b"plain"), Confidence::Low);
        }
    }
}
