//! Header/footer containers of documents written before the schema genesis
//! carried them are created on first write. Two replicas that each add the
//! first page header concurrently therefore each mint their own `root.header`
//! map; Automerge exposes one winner and the other header vanished from view.
//! Reads now join every candidate container and edits reach a block wherever
//! it lives, so neither side is lost.

use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ROOT};
use qdoc::DocMut;
use qdoc::model::{self, Region};

fn with_dm<R>(doc: &mut AutoCommit, f: impl FnOnce(&mut DocMut<'_>) -> R) -> R {
    let c = model::open(doc, &ROOT).unwrap();
    let mut ids = model::seeded_id_gen(doc, &c);
    let mut dm = DocMut {
        doc,
        c: &c,
        ids: &mut ids,
    };
    let out = f(&mut dm);
    doc.commit();
    out
}

/// A document authored before the regions came with the schema.
fn legacy_doc() -> AutoCommit {
    let mut doc = AutoCommit::new();
    let _ = model::init(&mut doc, &ROOT);
    doc.delete(&ROOT, qdoc::schema::KEY_HEADER).unwrap();
    doc.delete(&ROOT, qdoc::schema::KEY_FOOTER).unwrap();
    doc.commit();
    AutoCommit::load(&doc.save()).unwrap()
}

fn texts(doc: &AutoCommit, region: Region) -> Vec<String> {
    let c = model::open(doc, &ROOT).unwrap();
    model::materialize_region(doc, &c, region)
        .into_iter()
        .map(|b| b.text)
        .collect()
}

#[test]
fn concurrent_first_headers_on_a_legacy_document_both_survive() {
    for region in [Region::Header, Region::Footer] {
        for _ in 0..16 {
            let mut base = legacy_doc();
            let mut a = AutoCommit::load(&base.save()).unwrap();
            let mut b = AutoCommit::load(&base.save()).unwrap();
            b.set_actor(ActorId::random());
            let left = with_dm(&mut a, |dm| {
                let id = dm.insert_block_in(region, None, "paragraph");
                dm.splice_text_in(region, &id, 0, 0, "left");
                id
            });
            let right = with_dm(&mut b, |dm| {
                let id = dm.insert_block_in(region, None, "paragraph");
                dm.splice_text_in(region, &id, 0, 0, "right");
                id
            });
            a.merge(&mut b).unwrap();
            let mut shown = texts(&a, region);
            shown.sort();
            assert_eq!(shown, ["left", "right"], "{region:?}");
            assert_eq!(
                model::region_blocks_in_order(&a, &model::open(&a, &ROOT).unwrap(), region).len(),
                2
            );

            // Both blocks stay editable wherever their container lost, and a
            // new block lands alongside them.
            with_dm(&mut a, |dm| {
                dm.splice_text_in(region, &left, 4, 0, "!");
                dm.splice_text_in(region, &right, 5, 0, "?");
                dm.set_attr_in(region, &right, "align", "center");
                let _ = dm.insert_block_in(region, Some(&right), "paragraph");
            });
            let mut shown = texts(&a, region);
            shown.sort();
            assert_eq!(shown, ["", "left!", "right?"], "{region:?}");

            // Clearing a region clears every candidate container.
            with_dm(&mut a, |dm| dm.clear_region(region));
            assert!(texts(&a, region).is_empty(), "{region:?}");
        }
    }
}
