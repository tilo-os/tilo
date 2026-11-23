//! A document loaded a number of ops at a time is the one a single load with
//! the same change hashes builds.

use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::Transactable as _;
use automerge::{ActorId, AutoCommit, Automerge, LoadOptions, ObjType, ROOT, ReadDoc as _};
use tilo_automerge::ApplyIncoming as _;

fn document() -> Vec<u8> {
    let mut ada = AutoCommit::new().with_actor(ActorId::from(b"ada".as_slice()));
    let notes = ada.put_object(ROOT, "notes", ObjType::Map).unwrap();
    let list = ada.put_object(ROOT, "order", ObjType::List).unwrap();
    for at in 0..40_i64 {
        let note = ada
            .put_object(&notes, format!("note {at}"), ObjType::Map)
            .unwrap();
        let body = ada.put_object(&note, "body", ObjType::Text).unwrap();
        ada.splice_text(&body, 0, 0, &format!("the body of note {at}, typed out"))
            .unwrap();
        if at % 3 == 0 {
            ada.mark(
                &body,
                Mark::new("bold".into(), true, 0, 3),
                ExpandMark::After,
            )
            .unwrap();
        }
        ada.insert(&list, usize::try_from(at).unwrap(), format!("note {at}"))
            .unwrap();
        ada.increment_or_put(&note, "views", at);
        ada.commit();
    }
    let mut bea = ada.fork().with_actor(ActorId::from(b"bea".as_slice()));
    for at in 0..10 {
        let (_, note) = bea.get(&notes, format!("note {at}")).unwrap().unwrap();
        let (_, body) = bea.get(&note, "body").unwrap().unwrap();
        bea.splice_text(&body, 4, 4, "").unwrap();
        bea.delete(&list, 0).unwrap();
        bea.commit();
    }
    ada.put(ROOT, "title", "concurrent").unwrap();
    ada.commit();
    ada.merge_incoming(&mut bea).unwrap();
    ada.save()
}

trait IncrementOrPut {
    fn increment_or_put(&mut self, obj: &automerge::ObjId, key: &str, by: i64);
}

impl IncrementOrPut for AutoCommit {
    fn increment_or_put(&mut self, obj: &automerge::ObjId, key: &str, by: i64) {
        self.put(obj, key, automerge::ScalarValue::counter(0))
            .unwrap();
        self.increment(obj, key, by).unwrap();
    }
}

#[test]
fn a_document_loaded_in_slices_is_the_document_one_load_builds() {
    let bytes = document();
    let verified = Automerge::load(&bytes).unwrap();
    let hashes = verified.change_hashes().to_vec();
    let whole =
        Automerge::load_with_options(&bytes, LoadOptions::new().known_change_hashes(&hashes))
            .unwrap();
    for slice in [1, 7, 64, 100_000] {
        let mut load = Automerge::load_in_slices(&bytes, hashes.clone())
            .unwrap()
            .expect("one document chunk with its hashes");
        let mut steps = 0;
        while !load.step(slice) {
            steps += 1;
        }
        let (ops, loaded) = load.progress();
        assert_eq!(ops, loaded);
        let sliced = load.finish().unwrap();
        assert!(slice > 1 || steps > 100, "{steps} steps of one op");
        assert_eq!(sliced.get_heads(), whole.get_heads(), "slices of {slice}");
        assert_eq!(sliced.change_hashes(), whole.change_hashes());
        assert_eq!(sliced.save(), whole.save(), "slices of {slice}");
        assert_eq!(sliced.hydrate(None), whole.hydrate(None));
        let (_, notes) = sliced.get(ROOT, "notes").unwrap().unwrap();
        let (_, note) = sliced.get(&notes, "note 3").unwrap().unwrap();
        let (_, body) = sliced.get(&note, "body").unwrap().unwrap();
        assert_eq!(sliced.marks(&body).unwrap().len(), 1, "the mark index");
        // The loaded document takes changes as one loaded whole does.
        let mut sliced = AutoCommit::load(&sliced.save()).unwrap();
        sliced.put(ROOT, "after", 1).unwrap();
        sliced.commit();
    }
}

#[test]
fn bytes_that_are_not_one_document_or_wrong_hashes_load_as_before() {
    let bytes = document();
    let hashes = Automerge::load(&bytes).unwrap().change_hashes().to_vec();
    let mut more = bytes.clone();
    let mut doc = AutoCommit::load(&bytes).unwrap();
    doc.put(ROOT, "more", 1).unwrap();
    doc.commit();
    more.extend(doc.save_incremental());
    assert!(
        Automerge::load_in_slices(&more, hashes.clone())
            .unwrap()
            .is_none()
    );
    assert!(
        Automerge::load_in_slices(&bytes, hashes[1..].to_vec())
            .unwrap()
            .is_none()
    );
}
