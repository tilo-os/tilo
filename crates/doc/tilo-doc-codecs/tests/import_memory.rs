//! A document built by an import holds about what the same document holds once saved and loaded
//! again — the import leaves nothing behind but the document.
//!
//! Automerge 0.8 logs a patch event for every op of every change it applies, into the document's
//! patch log whether or not anything reads it, and an import writes its content as changes. Kept,
//! those events cost around ten times the document itself.

use automerge::{AutoCommit, ROOT};
use tilo_doc::{DocMut, Document, IdGen};
use tilo_doc_codecs::{FormatProvider, MarkdownCodec};

use crate::{heap, synth};

/// Heap bytes this thread still holds after `build`, and what it built.
fn held<T>(build: impl FnOnce() -> T) -> (T, isize) {
    let before = heap::held();
    let value = build();
    (value, heap::held().wrapping_sub(before))
}

/// A loaded document builds its block-order index on the first structural edit, an importer
/// while inserting; one inserted block gives both the index.
fn with_one_more_block(doc: &mut Document) {
    doc.insert_block(None, "paragraph");
}

#[allow(
    clippy::cast_precision_loss,
    reason = "heap sizes far below 2^52, compared as a ratio"
)]
fn assert_holds_like_loaded(what: &str, imported: isize, saved: &[u8]) {
    let (loaded, loaded_bytes) = held(|| {
        let mut doc = Document::load(saved).unwrap();
        with_one_more_block(&mut doc);
        doc
    });
    drop(loaded);
    let ratio = imported as f64 / loaded_bytes as f64;
    assert!(
        ratio < 1.25,
        "{what}: the imported document holds {imported} bytes, {ratio:.2}x the {loaded_bytes} \
         the same document holds after save/load"
    );
}

#[test]
fn imports_hold_what_a_loaded_document_does() {
    // Phase `an_owned_import_holds_what_a_loaded_document_does`.
    {
        let md = synth::markdown(150_000);
        let (mut doc, imported) = held(|| {
            let mut doc = Document::new();
            MarkdownCodec
                .import(
                    &md,
                    &mut doc.as_mut(),
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap();
            with_one_more_block(&mut doc);
            doc
        });
        let saved = doc.save();
        drop(doc);
        assert_holds_like_loaded("owned", imported, &saved);
    }
    // Phase `an_import_into_a_new_host_document_holds_what_a_loaded_document_does`.
    {
        let md = synth::markdown(150_000);
        let (mut am, imported) = held(|| {
            let mut am = AutoCommit::new();
            let c = tilo_doc::model::init(&mut am, &ROOT);
            let mut ids = IdGen::new(am.get_actor().to_hex_string());
            let mut dm = DocMut {
                doc: &mut am,
                c: &c,
                ids: &mut ids,
            };
            MarkdownCodec
                .import(&md, &mut dm, &mut tilo_content::ContentInputs::new())
                .unwrap();
            dm.insert_block(None, "paragraph");
            am.commit();
            am
        });
        let saved = am.save();
        drop(am);
        assert_holds_like_loaded("host", imported, &saved);
    }
}
