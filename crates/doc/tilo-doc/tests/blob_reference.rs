//! A document references a blob; it never carries one.
//!
//! This is the invariant a 146 kB image broke: stored inline, one change
//! exceeded the publish ceiling and was dropped, and because automerge holds a
//! change whose ancestors are missing, every other machine's copy froze at that
//! point — silently, and for good. The bytes are a binary input the caller keeps
//! in its content store; only the hash and a small descriptor go in the document.
//!
//! Asserted on the *saved* bytes rather than on the in-memory model, because
//! what replicates is what `save` produces: a test that only checked the model
//! would still have passed while the wire carried the image.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_content::{ContentInputs, ContentStore};
use tilo_doc::Document;

/// A tiny but real PNG: signature, IHDR with a known size, and enough of a body
/// that a sniffer can size it.
fn png() -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    v.extend_from_slice(&13u32.to_be_bytes());
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&7u32.to_be_bytes()); // width
    v.extend_from_slice(&11u32.to_be_bytes()); // height
    v.extend_from_slice(&[8, 6, 0, 0, 0]);
    v.extend_from_slice(&[0u8; 4]); // crc, unchecked here
    // A recognisable run, trivially findable if the bytes were ever inlined.
    v.extend(std::iter::repeat_n(0xA5, 4096));
    v
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn saved_documents_carry_blob_references_with_mime_and_descriptor_not_bytes() {
    // Phase `a_saved_document_carries_the_reference_and_not_the_bytes`.
    {
        let bytes = png();
        let mut doc = Document::new();
        let mut inputs = ContentInputs::new();
        let hash = doc.attach(&bytes, &mut inputs);
        assert_eq!(
            inputs.get(&hash),
            Some(bytes.as_slice()),
            "the bytes go to the caller"
        );

        let saved = doc.save();

        // A body of 4 kB of one value is findable by any window scan, so this fails
        // loudly if inlining ever comes back.
        assert!(
            !contains(&saved, &bytes[..64]),
            "the saved document carries the image's own bytes ({} bytes saved)",
            saved.len()
        );
        assert!(
            !contains(&saved, &[0xA5u8; 64]),
            "the saved document carries the image's body"
        );

        // The hash is the address the content store is keyed by, so it is the one
        // thing that must survive.
        assert!(
            contains(&saved, hash.as_bytes()),
            "the saved document does not carry the blob's hash {hash}"
        );

        assert!(
            saved.len() < bytes.len() / 2,
            "a referencing document ({} bytes) should be far smaller than the image it \
             references ({} bytes)",
            saved.len(),
            bytes.len()
        );
    }
    // Phase `the_mime_survives_in_a_document_that_also_has_text`.
    {
        // The minimal case is not the case a reader meets: a real document has
        // paragraphs and block structure around the image, and automerge encodes a
        // bigger document differently.
        let mut doc = Document::new();
        let p = doc.insert_block(None, "paragraph");
        doc.splice_text(&p, 0, 0, "An opening paragraph with some length to it.");
        for line in ["a first list item", "a second list item"] {
            let b = doc.insert_block(Some(&p), "list_item");
            doc.splice_text(&b, 0, 0, line);
        }
        let store = ContentStore::memory();
        let mut inputs = ContentInputs::new();
        let hash = doc.attach(&png(), &mut inputs);
        store.put_inputs(&inputs).unwrap();
        let _img = doc.insert_block(Some(&p), "image");

        let saved = doc.save();
        assert!(
            contains(&saved, b"image/png"),
            "the mime is not findable in a {}-byte document with text in it",
            saved.len()
        );
        // The hash is deliberately *not* asserted here, and that is the finding this
        // test records. In a document holding nothing but a blob it is plain ASCII in
        // the saved bytes; add paragraphs and it stops being findable, because it is
        // a map key and automerge encodes keys differently once the document is worth
        // compressing. A scan for the hash therefore works on a toy document and
        // fails on a real one — the worst shape a check can have. Anything looking
        // for an image reference in bytes off the wire should look for the mime,
        // which is a scalar value and survives both.
        assert!(
            !contains(&saved, &[0xA5u8; 64]),
            "the image's bytes are in a document with text in it"
        );
        let reopened = Document::load(&saved).expect("a saved document reloads");
        assert!(
            reopened.referenced_content().contains(&hash),
            "the reference is gone once the document has text in it"
        );
        assert_eq!(
            store.read(&hash).as_deref(),
            Some(png().as_slice()),
            "the reference no longer resolves once the document has text in it"
        );
    }
    // Phase `the_descriptor_travels_with_the_reference`.
    {
        // The intrinsic size has to cross too. Without it a peer whose bytes have
        // not arrived guesses a size, and every paragraph below the image moves when
        // they land — on a shared document, while somebody is typing into it.
        let mut doc = Document::new();
        let hash = doc.attach(&png(), &mut ContentInputs::new());
        let saved = doc.save();

        // The descriptor is written as scalars beside the hash, so the sniffed mime
        // is literal ASCII in the saved bytes. The `blob_*` attrs belong to the
        // materialised snapshot and are added at read time, so they never appear.
        assert!(
            contains(&saved, b"image/png"),
            "the descriptor's mime did not survive the save"
        );

        let reopened = Document::load(&saved).expect("a saved document reloads");
        let meta = tilo_doc::model::blob_meta(reopened.as_ref().doc, reopened.as_ref().c, &hash)
            .expect("the descriptor survives the save");
        assert_eq!((meta.mime.as_str(), meta.w, meta.h), ("image/png", 7, 11));
    }
}

#[test]
fn every_version_s_inputs_stay_referenced() {
    // A content store may only collect what no version of any document names, so the document
    // must still list an image after it was taken out again.
    let mut doc = Document::new();
    let mut inputs = ContentInputs::new();
    let first = doc.attach(&png(), &mut inputs);
    let img = doc.insert_block(None, "image");
    doc.remove_block(&img);
    let second = doc.attach(b"a later, unrelated input", &mut inputs);
    let referenced = doc.referenced_content();
    assert!(referenced.contains(&first) && referenced.contains(&second));
}
