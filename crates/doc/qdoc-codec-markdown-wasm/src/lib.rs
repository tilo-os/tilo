//! Format-provider build of the qdoc Markdown codec.
//!
//! Built with `build.sh` (`--target wasm32-unknown-unknown --release`). The
//! output is a core WebAssembly module speaking the ABI defined in
//! `format-provider-abi`; the tilo-format-host daemon runs it under wasmi.
//!
//! Canonical bytes are the Automerge `save()` output of `qdoc::Document`; the
//! host passes them through the WASM boundary for clients to reload.
//!
//! The crate is gated to `target_arch = "wasm32"`. Building it for the host
//! target yields an empty library.

#[cfg(target_arch = "wasm32")]
mod guest {
    use format_provider_abi::{CodecError, Confidence, Detected, Provider};
    use qdoc::{Document, FormatProvider, MarkdownCodec};

    struct Markdown;

    impl Provider for Markdown {
        fn detect(head: &[u8]) -> Detected {
            // qdoc's confidence enum maps onto a coarse (tier, score).
            let (tier, score) = match MarkdownCodec.detect(head) {
                qdoc::Confidence::None => (0, 0.0),
                qdoc::Confidence::Low => (0, 0.25),
                qdoc::Confidence::Medium => (1, 0.5),
                qdoc::Confidence::High => (2, 0.9),
            };
            Detected {
                confidence: Confidence { tier, score },
                mime: Some("text/markdown".to_owned()),
            }
        }

        fn import_doc(bytes: &[u8]) -> Result<Vec<u8>, CodecError> {
            let mut doc = Document::new();
            MarkdownCodec
                .import(bytes, &mut doc.as_mut())
                .map_err(|e| CodecError::parse(e.to_string()))?;
            // Canonical CRDT bytes = Automerge save() of the doc. The host
            // round-trips these opaquely; the editor / FS layer reloads them
            // via Document::load to get a live CRDT back.
            Ok(doc.save())
        }

        fn export_doc(canonical: &[u8]) -> Result<Vec<u8>, CodecError> {
            let doc = Document::load(canonical).map_err(|e| CodecError::parse(e.to_string()))?;
            MarkdownCodec
                .export(doc.as_ref())
                .map_err(|e| CodecError::export(e.to_string()))
        }
    }

    format_provider_abi::export_provider!(Markdown);
}
