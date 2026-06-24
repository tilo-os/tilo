//! Layer-1 generic format-provider contract + registry. Providers are pure
//! transforms between file bytes and canonical-document changes; in production
//! they run sandboxed (Phase 3). Here we define the contract + registry.

use crate::document::Document;

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

/// A format provider: a pure transform between file bytes and a canonical
/// `Document`. In production this trait object lives behind a sandbox IPC
/// boundary (Phase 3); the contract is identical either side.
pub trait FormatProvider: Send + Sync {
    fn schema_id(&self) -> &str;
    fn detect(&self, head: &[u8]) -> Confidence;
    fn import(&self, bytes: &[u8], doc: &mut Document) -> Result<(), CodecError>;
    fn export(&self, doc: &Document) -> Result<Vec<u8>, CodecError>;
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
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyMd;
    impl FormatProvider for DummyMd {
        fn schema_id(&self) -> &str {
            crate::schema::SCHEMA_ID
        }
        fn detect(&self, head: &[u8]) -> Confidence {
            if head.starts_with(b"# ") {
                Confidence::High
            } else {
                Confidence::Low
            }
        }
        fn import(&self, bytes: &[u8], doc: &mut Document) -> Result<(), CodecError> {
            let s = std::str::from_utf8(bytes).map_err(|e| CodecError::Parse(e.to_string()))?;
            let p = doc.insert_block(None, "paragraph");
            doc.splice_text(&p, 0, 0, s.trim());
            Ok(())
        }
        fn export(&self, doc: &Document) -> Result<Vec<u8>, CodecError> {
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
            crate::schema::SCHEMA_ID
        }
        fn detect(&self, _: &[u8]) -> Confidence {
            Confidence::High
        }
        fn import(&self, _: &[u8], _: &mut Document) -> Result<(), CodecError> {
            Ok(())
        }
        fn export(&self, _: &Document) -> Result<Vec<u8>, CodecError> {
            Ok(b"builtin".to_vec())
        }
    }

    #[test]
    fn import_export_roundtrip_via_provider() {
        let p = DummyMd;
        let mut d = Document::new();
        p.import(b"# Hello", &mut d).unwrap();
        let out = p.export(&d).unwrap();
        assert_eq!(out, b"# Hello");
    }

    #[test]
    fn registry_resolves_highest_tier() {
        let mut r = Registry::new();
        r.register("md", ProviderTier::Ephemeral, Box::new(DummyMd));
        r.register("md", ProviderTier::Builtin, Box::new(BuiltinMd));
        let prov = r.provider_for_ext("md").unwrap();
        // Builtin wins → its export returns b"builtin".
        let d = Document::new();
        assert_eq!(prov.export(&d).unwrap(), b"builtin");
    }

    #[test]
    fn detect_distinguishes_content() {
        let p = DummyMd;
        assert_eq!(p.detect(b"# Title"), Confidence::High);
        assert_eq!(p.detect(b"plain"), Confidence::Low);
    }
}
