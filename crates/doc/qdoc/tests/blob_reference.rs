//! A document references a blob; it never carries one.
//!
//! This is the invariant a 146 kB photograph broke: an image used to be
//! stored inline, one change exceeded the publish ceiling and was
//! dropped, and because Automerge holds a change whose ancestors are
//! missing, every other machine's copy froze at that point — silently,
//! and for good. The bytes now live in a content-addressed store beside
//! the document and only the hash and a small descriptor go in it.
//!
//! Asserted on the *saved* bytes rather than on the in-memory model,
//! because what replicates is what `save` produces. A test that only
//! checked the model would still have passed while the wire carried the
//! photograph.

use qdoc::Document;

/// A tiny but real PNG: signature, IHDR with a known size, and enough
/// of a body that a sniffer can size it.
fn png() -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    v.extend_from_slice(&13u32.to_be_bytes());
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&7u32.to_be_bytes()); // width
    v.extend_from_slice(&11u32.to_be_bytes()); // height
    v.extend_from_slice(&[8, 6, 0, 0, 0]);
    v.extend_from_slice(&[0u8; 4]); // crc, unchecked here
    // A recognisable run that would be trivially findable if the bytes
    // were ever inlined into the document.
    v.extend(std::iter::repeat(0xA5).take(4096));
    v
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn a_saved_document_carries_the_reference_and_not_the_bytes() {
    let bytes = png();
    let mut doc = Document::new();
    let hash = doc.put_blob(&bytes);
    assert!(!hash.is_empty(), "put_blob returned no hash");

    let saved = doc.save();

    // The bytes are the thing that must not be here. A body of 4 kB of
    // one value is findable by any window scan, so this fails loudly if
    // inlining ever comes back.
    assert!(
        !contains(&saved, &bytes[..64]),
        "the saved document carries the image's own bytes ({} bytes saved)",
        saved.len()
    );
    assert!(
        !contains(&saved, &[0xA5u8; 64]),
        "the saved document carries the image's body"
    );

    // And the reference is. The hash is the address the content store is
    // keyed by, so it is the one thing that must survive.
    assert!(
        contains(&saved, hash.as_bytes()),
        "the saved document does not carry the blob's hash {hash}"
    );

    // Small: a reference plus a descriptor, nowhere near the payload.
    assert!(
        saved.len() < bytes.len() / 2,
        "a referencing document ({} bytes) should be far smaller than the \
         image it references ({} bytes)",
        saved.len(),
        bytes.len()
    );
}

#[test]
fn the_descriptor_travels_with_the_reference() {
    // The intrinsic size has to cross too. Without it a peer whose bytes
    // have not arrived guesses a size, and every paragraph below the
    // image moves when they land — on a shared document, while somebody
    // is typing into it.
    let mut doc = Document::new();
    let hash = doc.put_blob(&png());
    let saved = doc.save();

    // The descriptor is written as scalars beside the hash, so the mime
    // it sniffed is literal ASCII in the saved bytes. That is also the
    // needle anything reading a document off the wire can look for —
    // the `blob_*` names belong to the materialised snapshot and are
    // added at read time, so they never appear here.
    assert!(
        contains(&saved, b"image/png"),
        "the descriptor's mime did not survive the save"
    );

    let reopened = Document::load(&saved).expect("a saved document reloads");
    // The hash still addresses the content, which lives outside the
    // document in the store `put_blob` wrote it to.
    assert_eq!(
        reopened.blob(&hash).as_deref(),
        Some(png().as_slice()),
        "the hash no longer addresses the image's bytes"
    );
}
