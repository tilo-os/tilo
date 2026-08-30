//! WASM Component Model build of the qdoc Markdown codec.
//!
//! This crate is built with `--target wasm32-wasip2 --release`. The output
//! `.wasm` is a Component Model component implementing the
//! `qos:format-provider/format-provider` interface; the qos-format-host
//! daemon loads it via wasmtime 46.
//!
//! The guest binds itself to the WIT package shipped by `format-provider-wit`
//! through `wit-bindgen::generate!`. The implementation delegates to the existing
//! `qdoc::MarkdownCodec` host-Rust source — the same code that runs in-process
//! for the editor + builtin codecs path.
//!
//! "Canonical bytes" on the wire are the Automerge save() output of the
//! qdoc::Document; the host hands those back to clients verbatim so the
//! collaborating CRDT layer is preserved across the WASM boundary.
//!
//! The crate is gated to `target_arch = "wasm32"`. Building it for the host
//! target is a no-op (an empty lib) — the workspace can still validate
//! `cargo check` without breaking.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a test that unwraps is stating an invariant"
    )
)]

#[cfg(target_arch = "wasm32")]
mod guest {
    use qdoc::{Document, FormatProvider, MarkdownCodec};

    wit_bindgen::generate!({
        path: "../../format/format-provider-wit/wit",
        world: "provider",
    });

    use exports::qos::format_provider::format_provider::{CodecError, Confidence, Detected, Guest};

    struct Component;

    impl Guest for Component {
        fn detect(head: Vec<u8>) -> Detected {
            // qdoc's confidence enum maps onto a coarse (tier, score).
            let conf = MarkdownCodec.detect(&head);
            let (tier, score) = match conf {
                qdoc::Confidence::None => (0, 0.0),
                qdoc::Confidence::Low => (0, 0.25),
                qdoc::Confidence::Medium => (1, 0.5),
                qdoc::Confidence::High => (2, 0.9),
            };
            Detected {
                confidence: Confidence { tier, score },
                mime: Some("text/markdown".to_string()),
            }
        }

        fn import_doc(bytes: Vec<u8>) -> Result<Vec<u8>, CodecError> {
            let mut doc = Document::new();
            MarkdownCodec
                .import(&bytes, &mut doc.as_mut())
                .map_err(|e| CodecError {
                    kind: "parse".to_string(),
                    message: e.to_string(),
                })?;
            // Canonical CRDT bytes = Automerge save() of the doc. The host
            // round-trips these opaquely; the editor / FS layer reloads them
            // via Document::load to get a live CRDT back. `save` takes
            // `&mut self` so it can drain pending ops first.
            let mut doc = doc;
            Ok(doc.save())
        }

        fn export_doc(canonical: Vec<u8>) -> Result<Vec<u8>, CodecError> {
            let doc = Document::load(&canonical).map_err(|e| CodecError {
                kind: "parse".to_string(),
                message: e.to_string(),
            })?;
            MarkdownCodec.export(doc.as_ref()).map_err(|e| CodecError {
                kind: "export".to_string(),
                message: e.to_string(),
            })
        }
    }

    export!(Component);
}
