//! Format-provider build of the qdoc Markdown codec.
//!
//! Built with `build.sh` (`--target wasm32-unknown-unknown --release`). The
//! output is a core WebAssembly module speaking the ABI defined in
//! `format-provider-abi`; the tilo-format-host daemon runs it under wasmi.
//!
//! Canonical bytes are the Automerge `save()` output of `tilo_doc::Document`; the
//! host passes them through the WASM boundary for clients to reload.
//!
//! The crate is gated to `target_arch = "wasm32"`. Building it for the host
//! target yields an empty library.

#[cfg(target_arch = "wasm32")]
mod guest {
    use format_provider_abi::{CodecError, Confidence, Detected, Provider};
    use tilo_doc::Document;
    use tilo_doc_codecs::{FormatProvider, MarkdownCodec};

    struct Markdown;

    impl Provider for Markdown {
        fn detect(head: &[u8]) -> Detected {
            // qdoc's confidence enum maps onto a coarse (tier, score).
            let (tier, score) = match MarkdownCodec.detect(head) {
                tilo_doc_codecs::Confidence::None => (0, 0.0),
                tilo_doc_codecs::Confidence::Low => (0, 0.25),
                tilo_doc_codecs::Confidence::Medium => (1, 0.5),
                tilo_doc_codecs::Confidence::High => (2, 0.9),
            };
            Detected {
                confidence: Confidence { tier, score },
                mime: Some("text/markdown".to_owned()),
            }
        }

        fn import_doc(bytes: &[u8]) -> Result<Vec<u8>, CodecError> {
            let mut doc = Document::new();
            // Markdown embeds no binary content, and the provider ABI carries none.
            MarkdownCodec
                .import(
                    bytes,
                    &mut doc.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .map_err(|e| CodecError::parse(e.to_string()))?;
            // Canonical CRDT bytes = Automerge save() of the doc. The host
            // round-trips these opaquely; the editor / FS layer reloads them
            // via Document::load to get a live CRDT back.
            Ok(doc.save())
        }

        fn export_doc(canonical: &[u8]) -> Result<Vec<u8>, CodecError> {
            let doc = Document::load(canonical).map_err(|e| CodecError::parse(e.to_string()))?;
            MarkdownCodec
                .export(doc.as_ref(), &tilo_content::NoContent)
                .map_err(|e| CodecError::export(e.to_string()))
        }
    }

    format_provider_abi::export_provider!(Markdown);
}
