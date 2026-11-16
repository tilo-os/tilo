//! A large document read within `away::deferring` is verified by the host's
//! verifier first, and loads with the hashes it computed.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::rc::Rc;

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ChangeHash};

use crate::{Applied, Dependencies, Rejected, away, in_page};

/// What a verifier is handed to answer with.
type Done = Box<dyn FnOnce(Result<Vec<ChangeHash>, String>)>;

/// A verifier that keeps what it is asked, to answer when the test says.
#[derive(Default)]
struct Held {
    asked: RefCell<Vec<(Vec<u8>, Done)>>,
}

impl away::Verifier for Held {
    fn verify(&self, bytes: Vec<u8>, done: Done) {
        self.asked.borrow_mut().push((bytes, done));
    }
}

impl Held {
    fn installed() -> Rc<Self> {
        away::reset();
        let held = Rc::new(Self::default());
        away::verify_with(held.clone());
        held
    }

    /// Answer every request as a worker verifying it would.
    fn answer(&self) {
        for (bytes, done) in self.asked.borrow_mut().drain(..) {
            done(crate::verified_hashes(&bytes).map_err(|error| error.to_string()));
        }
    }
}

/// A saved document of many changes, well over the size verified away.
fn large() -> Vec<u8> {
    let mut doc = AutoCommit::new();
    let list = doc
        .put_object(automerge::ROOT, "items", automerge::ObjType::List)
        .unwrap();
    for at in 0..400usize {
        let item = doc
            .insert_object(&list, at, automerge::ObjType::Text)
            .unwrap();
        // Text that does not compress away: each item's own digits.
        let text = (0..60u64).fold(String::new(), |mut text, word| {
            let digits = (at as u64)
                .wrapping_mul(7919)
                .wrapping_add(word)
                .wrapping_mul(2_654_435_761);
            let _ = write!(text, "{digits:x} ");
            text
        });
        doc.splice_text(&item, 0, 0, &text).unwrap();
        doc.commit();
    }
    let bytes = doc.save();
    assert!(bytes.len() >= away::MIN_BYTES, "{} bytes", bytes.len());
    bytes
}

fn apply(doc: &mut AutoCommit, bytes: &[u8]) -> Result<Applied, Rejected> {
    in_page::apply(doc, bytes, Dependencies::RequireComplete)
}

#[test]
fn a_large_document_read_deferring_loads_once_verified_away() {
    let held = Held::installed();
    let bytes = large();
    let mut reference = AutoCommit::load(&bytes).unwrap();
    let mut doc = AutoCommit::new();
    let before = away::verified();
    assert_eq!(
        away::deferring(|| apply(&mut doc, &bytes)),
        Err(Rejected::Verifying)
    );
    assert!(doc.get_heads().is_empty(), "nothing is applied meanwhile");
    assert!(away::verifying());
    assert_eq!(
        away::deferring(|| apply(&mut doc, &bytes)),
        Err(Rejected::Verifying),
        "a second read waits on the same verification"
    );
    assert_eq!(held.asked.borrow().len(), 1, "verified once");
    held.answer();
    assert!(!away::verifying());
    assert_ne!(away::verified(), before, "the reader learns to ask again");
    assert_eq!(
        away::deferring(|| apply(&mut doc, &bytes)),
        Ok(Applied::Changed(true))
    );
    assert_eq!(
        doc.document().change_hashes(),
        reference.document().change_hashes()
    );
    assert_eq!(
        doc.get_changes(&[])
            .iter()
            .map(automerge::Change::raw_bytes)
            .collect::<Vec<_>>(),
        reference
            .get_changes(&[])
            .iter()
            .map(automerge::Change::raw_bytes)
            .collect::<Vec<_>>(),
        "the same changes under the same hashes"
    );
    // A load of the same bytes elsewhere on this thread takes them too.
    let mut loaded = in_page::load(&bytes).unwrap();
    assert_eq!(loaded.get_heads(), reference.get_heads());
    assert!(held.asked.borrow().is_empty());
}

#[test]
fn outside_deferring_or_small_a_document_loads_in_place() {
    let held = Held::installed();
    let bytes = large();
    let mut doc = AutoCommit::new();
    assert_eq!(apply(&mut doc, &bytes), Ok(Applied::Changed(true)));
    let mut small = AutoCommit::new();
    small.put(automerge::ROOT, "a", 1).unwrap();
    small.commit();
    let mut doc = AutoCommit::new();
    assert_eq!(
        away::deferring(|| apply(&mut doc, &small.save())),
        Ok(Applied::Changed(true))
    );
    assert!(held.asked.borrow().is_empty(), "nothing was sent away");
}

#[test]
fn a_failed_verification_leaves_the_load_to_this_thread() {
    let held = Held::installed();
    let bytes = large();
    let mut doc = AutoCommit::new();
    assert_eq!(
        away::deferring(|| apply(&mut doc, &bytes)),
        Err(Rejected::Verifying)
    );
    for (_, done) in held.asked.borrow_mut().drain(..) {
        done(Err("the worker did not start".into()));
    }
    assert_eq!(
        away::deferring(|| apply(&mut doc, &bytes)),
        Ok(Applied::Changed(true))
    );
    // Bad bytes are still refused here, verified or not.
    let mut torn = bytes;
    let middle = torn.len() / 2;
    torn[middle] ^= 0xff;
    let mut doc = AutoCommit::new();
    let refused = away::deferring(|| apply(&mut doc, &torn));
    held.answer();
    let refused = match refused {
        Err(Rejected::Verifying) => away::deferring(|| apply(&mut doc, &torn)),
        other => other,
    };
    assert!(
        refused.as_ref().is_err_and(Rejected::refuses_input),
        "{refused:?}"
    );
    assert!(doc.get_heads().is_empty());
}

#[test]
fn a_document_loaded_a_step_at_a_time_is_the_one_the_read_takes() {
    let held = Held::installed();
    tilo_automerge::shared::clear();
    let bytes = large();
    let mut doc = AutoCommit::new();
    assert_eq!(
        away::deferring(|| apply(&mut doc, &bytes)),
        Err(Rejected::Verifying)
    );
    let hashes = crate::verified_hashes(&bytes).unwrap();
    let mut loading = away::Loading::new(bytes.clone(), hashes).expect("one document");
    let mut steps = 0;
    while !loading.step(500) {
        steps += 1;
    }
    loading.finish();
    assert!(steps > 2, "{steps} steps");
    assert_eq!(
        tilo_automerge::shared::kept(),
        1,
        "the loaded document is kept"
    );
    held.answer();
    assert_eq!(
        away::deferring(|| apply(&mut doc, &bytes)),
        Ok(Applied::Changed(true))
    );
    let mut reference = AutoCommit::load(&bytes).unwrap();
    assert_eq!(doc.get_heads(), reference.get_heads());
    assert_eq!(doc.save(), reference.save());
    tilo_automerge::shared::clear();
}
