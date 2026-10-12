//! A device that has the app replays every change it receives and checks
//! that the change is exactly what its recorded action produces.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::transaction::{CommitOptions, Transactable};
use automerge::{AutoCommit, ROOT, ReadDoc};
use serde::{Deserialize, Serialize};
use tilo_actions::{
    Action, ActionContext, ActionError, ActionRegistry, Doc, EncodedAction, LocalTransport,
    Verified, dispatch_with_context, verify_change,
};

#[derive(Debug, Serialize, Deserialize)]
struct SetCounter(i64);

impl Action for SetCounter {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        doc.put(ROOT, "counter", self.0)?;
        Ok(())
    }

    fn inverse_encoded(&self, doc: &Doc) -> Option<EncodedAction> {
        let before = doc.get(ROOT, "counter").ok()??.0.as_i64()?;
        EncodedAction::encode("test.counter", &Self(before)).ok()
    }
}

fn registry() -> ActionRegistry {
    let mut registry = ActionRegistry::new();
    registry.register::<SetCounter>("test.counter");
    registry
}

fn base() -> AutoCommit {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "counter", 7_i64).unwrap();
    doc.commit();
    doc
}

#[test]
fn a_received_change_its_action_produces_is_reproduced() {
    let mut origin = base();
    let mut peer = origin.fork();
    let mut transport = LocalTransport::new();
    dispatch_with_context(
        &mut origin,
        &SetCounter(11),
        EncodedAction::encode("test.counter", &SetCounter(11)).unwrap(),
        &mut transport,
        ActionContext::for_resource("r", "k", "l"),
    )
    .unwrap();
    let hash = origin.get_heads()[0];
    tilo_actions::replay(&mut peer, &transport.replicated[0], &registry()).unwrap();
    let before = peer.get_heads();
    assert_eq!(
        verify_change(&mut peer, hash, &registry()).unwrap(),
        Verified::Reproduced
    );
    assert_eq!(peer.get_heads(), before, "verifying changes nothing");
}

#[test]
fn a_change_that_writes_other_than_its_action_is_caught() {
    let mut doc = base();
    let context = ActionContext {
        action: Some(EncodedAction::encode("test.counter", &SetCounter(11)).unwrap()),
        ..ActionContext::for_resource("r", "k", "l")
    };
    // Claims to set the counter to 11, writes 12 and another key.
    doc.put(ROOT, "counter", 12_i64).unwrap();
    doc.put(ROOT, "elsewhere", true).unwrap();
    let hash = doc
        .commit_with(CommitOptions::default().with_message(context.change_message().unwrap()))
        .unwrap();
    assert!(verify_change(&mut doc, hash, &registry()).is_err());
}

#[test]
fn a_change_with_no_recorded_action_or_an_unknown_app_is_left_to_others() {
    let mut doc = base();
    let genesis = doc.get_heads()[0];
    assert_eq!(
        verify_change(&mut doc, genesis, &registry()).unwrap(),
        Verified::NothingRecorded
    );

    let mut transport = LocalTransport::new();
    dispatch_with_context(
        &mut doc,
        &SetCounter(3),
        EncodedAction::encode("test.counter", &SetCounter(3)).unwrap(),
        &mut transport,
        ActionContext::for_resource("r", "k", "l"),
    )
    .unwrap();
    let hash = doc.get_heads()[0];
    assert_eq!(
        verify_change(&mut doc, hash, &ActionRegistry::new()).unwrap(),
        Verified::UnknownAction("test.counter".into())
    );
}

#[derive(Debug, Serialize, Deserialize)]
struct Paste(String);

impl Action for Paste {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        doc.put(ROOT, "pasted", self.0.as_str())?;
        Ok(())
    }
}

/// An action over the history budget records its large parts as content:
/// the change still names the action, a device with the content replays
/// it, and one without holds the change until the content arrives.
#[test]
fn a_large_action_is_recorded_with_its_parts_as_content() {
    let mut origin = base();
    let mut transport = LocalTransport::new();
    let paste = Paste("p".repeat(tilo_actions::context::HISTORY_BUDGET + 1));
    dispatch_with_context(
        &mut origin,
        &paste,
        EncodedAction::encode("test.paste", &paste).unwrap(),
        &mut transport,
        ActionContext::for_resource("r", "k", "l"),
    )
    .unwrap();
    let hash = origin.get_heads()[0];
    let envelope = &transport.replicated[0];
    let recorded = envelope
        .context
        .as_ref()
        .and_then(|context| context.action.clone())
        .expect("the change records its action");
    assert_eq!(recorded.content_parts().len(), 1);
    let content = envelope.recorded_content().unwrap();
    assert_eq!(content.len(), 1);

    let mut without = ActionRegistry::new();
    without.register::<Paste>("test.paste");
    assert_eq!(
        verify_change(&mut origin, hash, &without).unwrap(),
        Verified::ContentMissing(recorded.content_parts()[0].clone())
    );
    let mut with = ActionRegistry::new();
    with.register::<Paste>("test.paste");
    let kept: std::collections::BTreeMap<String, Vec<u8>> = content.into_iter().collect();
    with.set_content(move |hash| kept.get(hash).cloned());
    assert_eq!(
        verify_change(&mut origin, hash, &with).unwrap(),
        Verified::Reproduced
    );
}

/// Types one character at the end of the text `body`: a keystroke.
#[derive(Debug, Serialize, Deserialize)]
struct Type(String);

impl Action for Type {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let body = match doc.get(ROOT, "body")? {
            Some((automerge::Value::Object(automerge::ObjType::Text), id)) => id,
            _ => doc.put_object(ROOT, "body", automerge::ObjType::Text)?,
        };
        let end = doc.length(&body);
        doc.splice_text(&body, end, 0, &self.0)?;
        Ok(())
    }

    /// Every keystroke reads the same: one character typed into the body.
    fn describe(&self, _doc: &Doc) -> Option<tilo_actions::Described> {
        Some(tilo_actions::Described::new(tilo_actions::Did::Edit(
            tilo_actions::FieldEdit {
                ty: "note".into(),
                field: "body".into(),
                path: vec![tilo_actions::PathStep::Field("body".into())],
                op: tilo_actions::EditOp::Splice,
                old: None,
                new: None,
                args: [(
                    "typed".to_owned(),
                    tilo_actions::Scalar::from_json(&1.into()).unwrap(),
                )]
                .into(),
            },
        )))
    }
}

/// Three keystrokes of one gesture by one writer, each dispatched as an
/// action with the gesture's entry.
fn typed(doc: &mut AutoCommit, transport: &mut LocalTransport, text: &str) {
    let gesture = tilo_actions::ActionId::new();
    for ch in text.chars() {
        let context = ActionContext {
            gesture: Some(gesture),
            ..ActionContext::for_resource("note", "notes.note", "Plan")
        };
        tilo_actions::dispatch_by_with_context(
            doc,
            &Type(ch.to_string()),
            EncodedAction::encode("test.type", &Type(ch.to_string())).unwrap(),
            transport,
            Some(b"writer"),
            context,
        )
        .unwrap();
    }
}

#[test]
fn a_gestures_later_changes_are_short_continuations_of_its_first_entry() {
    let mut registry = ActionRegistry::new();
    registry.register::<Type>("test.type");
    let mut origin = AutoCommit::new();
    let mut peer = origin.fork();
    let mut transport = LocalTransport::new();
    typed(&mut origin, &mut transport, "abc");
    let changes = origin.get_changes(&[]);
    let messages: Vec<&str> = changes
        .iter()
        .filter_map(|change| change.message())
        .collect();
    assert!(
        messages[0].starts_with(tilo_actions::CHANGE_MESSAGE_PREFIX),
        "{messages:?}"
    );
    for continuation in &messages[1..] {
        assert!(
            continuation.starts_with(tilo_actions::CONTINUATION_PREFIX),
            "{messages:?}"
        );
        assert!(continuation.len() * 2 < messages[0].len(), "{messages:?}");
    }

    // Every change reads as the gesture's entry with its own action.
    let first = ActionContext::from_change_message(messages[0]).unwrap();
    let mut listed = tilo_actions::EntryReader::default();
    for (change, ch) in changes.iter().zip(["a", "b", "c"]) {
        let entry = tilo_actions::entry_of(origin.committed_document(), change.hash()).unwrap();
        assert_eq!(entry.resource, first.resource);
        assert_eq!(entry.gesture, first.gesture);
        assert_eq!(
            entry.action,
            Some(EncodedAction::encode("test.type", &Type(ch.to_owned())).unwrap())
        );
        let read = listed
            .read(&change.actor_id().clone(), change.message())
            .unwrap();
        assert_eq!(read.action, entry.action);
    }

    // A device replays each keystroke, continuation included, to the same
    // change, and verifies it against its recorded action.
    for envelope in &transport.replicated {
        tilo_actions::replay(&mut peer, envelope, &registry).unwrap();
    }
    assert_eq!(peer.get_heads(), origin.get_heads());
    for change in &changes {
        assert_eq!(
            verify_change(&mut origin, change.hash(), &registry).unwrap(),
            Verified::Reproduced
        );
    }

    // A new gesture starts with its whole entry again.
    typed(&mut origin, &mut transport, "d");
    let last = origin
        .get_changes(&[])
        .last()
        .unwrap()
        .message()
        .unwrap()
        .to_owned();
    assert!(
        last.starts_with(tilo_actions::CHANGE_MESSAGE_PREFIX),
        "{last}"
    );
}
