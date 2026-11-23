#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT};
use serde::{Deserialize, Serialize};

use super::{CONTINUATIONS, MAX_CONTINUATIONS, STEPS_BACK, dispatch_by_with_context, entry_of};
use crate::action::{Action, ActionError, ActionId, Doc, EncodedAction};
use crate::context::{ActionContext, CONTINUATION_PREFIX};
use crate::executor::LocalTransport;

#[derive(Debug, Serialize, Deserialize)]
struct Type(String);

impl Action for Type {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        doc.put(ROOT, "typed", self.0.as_str())?;
        Ok(())
    }

    /// Every keystroke reads the same, so each continues the one before.
    fn describe(&self, _doc: &Doc) -> Option<crate::Described> {
        Some(crate::Described::new(crate::Did::Edit(crate::FieldEdit {
            ty: "note".into(),
            field: "body".into(),
            path: vec![crate::PathStep::Field("body".into())],
            op: crate::EditOp::Splice,
            old: None,
            new: None,
            args: std::collections::BTreeMap::new(),
        })))
    }
}

/// Type `keys` keystrokes of one gesture; the message of each change.
fn type_keys(doc: &mut AutoCommit, gesture: ActionId, keys: usize) -> Vec<String> {
    let mut transport = LocalTransport::new();
    (0..keys)
        .map(|key| {
            let typed = Type(key.to_string());
            let context = ActionContext {
                gesture: Some(gesture),
                ..ActionContext::for_resource("note", "notes.note", "Plan")
            };
            dispatch_by_with_context(
                doc,
                &typed,
                EncodedAction::encode("test.type", &typed).unwrap(),
                &mut transport,
                Some(b"writer"),
                context,
            )
            .unwrap();
            let head = doc.get_heads()[0];
            doc.get_change_by_hash(&head)
                .and_then(|change| change.message().map(str::to_owned))
                .unwrap_or_default()
        })
        .collect()
}

/// How many changes back the next keystroke of a gesture `keys` long reads.
fn steps_for_next_key(keys: usize) -> usize {
    let mut doc = AutoCommit::new();
    let gesture = ActionId::new();
    type_keys(&mut doc, gesture, keys);
    STEPS_BACK.with(|steps| steps.set(0));
    type_keys(&mut doc, gesture, 1);
    STEPS_BACK.with(std::cell::Cell::get)
}

#[test]
fn a_keystroke_reads_its_gestures_entry_one_change_back_however_long_the_gesture() {
    let short = steps_for_next_key(40);
    let long = steps_for_next_key(400);
    assert!(short > 0, "the keystroke continued its gesture");
    assert_eq!(
        short, long,
        "a keystroke's entry costs the same however long its gesture is"
    );
    assert!(short <= 2, "one change back: {short}");
}

#[test]
fn a_gesture_longer_than_the_walk_starts_its_entry_again_as_without_what_is_kept() {
    let mut doc = AutoCommit::new();
    let messages = type_keys(&mut doc, ActionId::new(), MAX_CONTINUATIONS + 3);
    let continues = |at: usize| messages[at].starts_with(CONTINUATION_PREFIX);
    assert!(!continues(0));
    assert!(continues(1) && continues(MAX_CONTINUATIONS + 1));
    // The change after the deepest continuation an entry is followed back
    // over writes its entry whole, as a device reading the gesture from the
    // start would.
    assert!(!continues(MAX_CONTINUATIONS + 2));
    // Read again with nothing kept, newest first so nothing read is kept
    // for the next, each change names the entry it named.
    let hashes: Vec<_> = doc
        .get_changes(&[])
        .iter()
        .map(automerge::Change::hash)
        .collect();
    let picked = [
        MAX_CONTINUATIONS + 2,
        MAX_CONTINUATIONS + 1,
        MAX_CONTINUATIONS,
        1,
    ];
    let kept: Vec<_> = picked
        .iter()
        .map(|&at| entry_of(doc.committed_document(), hashes[at]))
        .collect();
    let fresh: Vec<_> = picked
        .iter()
        .map(|&at| {
            CONTINUATIONS.with_borrow_mut(std::collections::VecDeque::clear);
            entry_of(doc.committed_document(), hashes[at])
        })
        .collect();
    assert_eq!(kept, fresh);
    assert!(kept[1].is_none() && kept[2].is_some());
}
