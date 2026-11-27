#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use automerge::transaction::{CommitOptions, Transactable};
use automerge::{AutoCommit, ROOT, ReadDoc};
use serde::{Deserialize, Serialize};
use tilo_actions::{
    Action, ActionContext, ActionError, ActionItem, ActionKey, ActionRegistry, Described, Did, Doc,
    EncodedAction, LocalTransport, Verified, dispatch_with_context, verify_change,
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

#[derive(Debug, Serialize, Deserialize)]
struct Paste(String);

impl Action for Paste {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        doc.put(ROOT, "pasted", self.0.as_str())?;
        Ok(())
    }
}

/// An action larger than a change documents whole records its large parts as content:
/// the change still names the action, a device with the content replays
/// it, and one without holds the change until the content arrives.
#[test]
fn a_large_action_is_recorded_with_its_parts_as_content() {
    let window = tilo_wake::WakeScope::new();
    let store = tilo_content::ContentStore::memory().sealing();
    tilo_actions::register(&window.handle(), store.clone());
    let mut origin = base();
    let mut transport = LocalTransport::new();
    let paste = Paste("p".repeat(tilo_actions::context::WHOLE_ACTION_BUDGET + 1));
    {
        let _in_the_window = window.enter();
        dispatch_with_context(
            &mut origin,
            &paste,
            EncodedAction::encode("test.paste", &paste).unwrap(),
            &mut transport,
            ActionContext::for_resource("r", "k", "l"),
        )
        .unwrap();
    }
    let hash = origin.get_heads()[0];
    let envelope = &transport.replicated[0];
    let recorded = envelope
        .context
        .as_ref()
        .and_then(|context| context.action.clone())
        .expect("the change records its action");
    let parts = recorded.content_parts();
    assert_eq!(parts.len(), 1);
    assert!(
        recorded
            .resolve_content(&|reference| store.read(reference))
            .unwrap()
            .is_ok(),
        "the window's store keeps it"
    );
    let content = envelope.recorded_content().unwrap();
    assert_eq!(content.len(), 1);
    let first = parts[0].clone();

    // Verified on no window's thread, where a part comes from the record's own
    // content store: without it the change is held, with it reproduced.
    let (held, reproduced) = std::thread::spawn(move || {
        let without = ActionRegistry::with(|r| r.register::<Paste>("test.paste"));
        let held = verify_change(&mut origin, hash, &without).unwrap();
        let mut with = ActionRegistry::with(|r| r.register::<Paste>("test.paste"));
        let kept = tilo_content::ContentStore::memory();
        for (reference, bytes) in content {
            kept.put_as(&reference, &bytes).unwrap();
        }
        with.connect_content(kept);
        let reproduced = verify_change(&mut origin, hash, &with).unwrap();
        (held, reproduced)
    })
    .join()
    .unwrap();
    assert_eq!(held, Verified::ContentMissing(first));
    assert_eq!(reproduced, Verified::Reproduced);
}

/// An action a change cannot document whole, with nowhere to keep its parts, is
/// refused, with the reason, before it commits anything: no change is
/// written and nothing is recorded that names a part nothing holds.
#[test]
fn a_large_action_with_nowhere_to_keep_its_parts_is_refused() {
    let mut origin = base();
    let before = origin.get_heads();
    let mut transport = LocalTransport::new();
    let paste = Paste("p".repeat(tilo_actions::context::WHOLE_ACTION_BUDGET + 1));
    let refused = dispatch_with_context(
        &mut origin,
        &paste,
        EncodedAction::encode("test.paste", &paste).unwrap(),
        &mut transport,
        ActionContext::for_resource("r", "k", "l"),
    )
    .expect_err("a part nothing keeps would be lost");
    let ActionError::Refused(reason) = refused else {
        panic!("expected a refusal, got {refused:?}");
    };
    assert!(reason.contains("no content store"), "{reason}");
    assert_eq!(origin.get_heads(), before, "nothing was recorded");
    assert!(transport.replicated.is_empty(), "nothing reached the peers");
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
fn typed(doc: &mut AutoCommit, transport: &mut LocalTransport, writer: &[u8], text: &str) {
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
            Some(writer),
            context,
        )
        .unwrap();
    }
}

tilo_test_env::scenario! {
    fn a_recorded_change_is_verified_against_its_action;

    phase a_received_change_its_action_produces_is_reproduced;
    {
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

    phase a_change_that_writes_other_than_its_action_is_caught;
    {
        let mut doc = base();
        let context = ActionContext {
            action: Some(EncodedAction::encode("test.counter", &SetCounter(11)).unwrap()),
            ..ActionContext::for_resource("r", "k", "l")
        };
        // Claims to set the counter to 11, writes 12 and another key.
        doc.put(ROOT, "counter", 12_i64).unwrap();
        doc.put(ROOT, "elsewhere", true).unwrap();
        let hash = doc
            .commit_with(CommitOptions::default().with_message(context.entry_in(&doc).unwrap()))
            .unwrap();
        assert!(verify_change(&mut doc, hash, &registry()).is_err());
    }

    phase a_change_with_no_recorded_action_or_an_unknown_app_is_left_to_others;
    {
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

    phase a_writers_later_changes_are_short_continuations_of_its_first_entry;
    {
        let mut registry = ActionRegistry::new();
        registry.register::<Type>("test.type");
        let mut origin = AutoCommit::new();
        let mut peer = origin.fork();
        let mut transport = LocalTransport::new();
        typed(&mut origin, &mut transport, b"writer", "abc");
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
            assert!(continuation.len() < messages[0].len(), "{messages:?}");
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

        // The next word's gesture continues the same entry under its own
        // gesture, after someone else's change, and replays to the same change.
        let before = transport.replicated.len();
        typed(&mut origin, &mut transport, b"other", "x");
        typed(&mut origin, &mut transport, b"writer", "d");
        let last = origin.get_changes(&[]).last().unwrap().hash();
        let message = origin.get_change_by_hash(&last).unwrap().message().unwrap().to_owned();
        assert!(message.starts_with(tilo_actions::CONTINUATION_PREFIX), "{message}");
        let entry = tilo_actions::entry_of(origin.committed_document(), last).unwrap();
        assert_ne!(entry.gesture, first.gesture);
        assert_eq!(entry.gesture, tilo_actions::ActionContext::continued_gesture(&message));
        for envelope in &transport.replicated[before..] {
            tilo_actions::replay(&mut peer, envelope, &registry).unwrap();
        }
        assert_eq!(peer.get_heads(), origin.get_heads());
        assert_eq!(verify_change(&mut origin, last, &registry).unwrap(), Verified::Reproduced);
    }
}

/// How [`Retitle`] names the card it writes: by the card's whole title, as an
/// entry was written before an item's label was kept as one line, or by one
/// line of it.
#[derive(Clone, Copy)]
enum Naming {
    Whole,
    OneLine,
}

thread_local! {
    static NAMING: std::cell::Cell<Naming> = const { std::cell::Cell::new(Naming::Whole) };
}

#[derive(Debug, Serialize, Deserialize)]
struct Retitle(String);

fn title(doc: &Doc) -> Option<String> {
    Some(doc.get(ROOT, "title").ok()??.0.as_str()?.to_owned())
}

impl Action for Retitle {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        doc.put(ROOT, "title", self.0.as_str())?;
        Ok(())
    }

    fn inverse_encoded(&self, doc: &Doc) -> Option<EncodedAction> {
        EncodedAction::encode("test.retitle", &Self(title(doc)?)).ok()
    }

    fn describe(&self, doc: &Doc) -> Option<Described> {
        let title = title(doc)?;
        let label = match NAMING.get() {
            Naming::Whole => title,
            Naming::OneLine => title.chars().take(96).collect(),
        };
        Some(Described {
            what: Did::verb(ActionKey::new("test", "card", "retitle"), []),
            item: Some(ActionItem {
                id: "card".into(),
                label,
            }),
        })
    }
}

/// A change is verified, and its undo found, under the entry it was written
/// with: nothing describes its action again. So a change that names its item
/// by a whole text (every change kept from before a label was one line) and
/// a change that names it by one line are both taken, whichever of the two
/// the verifying code would write itself.
#[test]
fn a_change_verifies_and_undoes_under_the_entry_it_was_written_with() {
    let registry = ActionRegistry::with(|r| r.register::<Retitle>("test.retitle"));
    let long = "word ".repeat(2_000);
    for (written, verified, carried) in [
        (Naming::Whole, Naming::OneLine, long.chars().count()),
        (Naming::OneLine, Naming::Whole, 96),
    ] {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "title", long.as_str()).unwrap();
        doc.commit();
        let mut transport = LocalTransport::new();
        NAMING.set(written);
        dispatch_with_context(
            &mut doc,
            &Retitle("Short".into()),
            EncodedAction::encode("test.retitle", &Retitle("Short".into())).unwrap(),
            &mut transport,
            ActionContext::for_resource("r", "k", "l"),
        )
        .unwrap();
        let hash = doc.get_heads()[0];
        let label = transport.replicated[0]
            .context
            .as_ref()
            .and_then(|context| context.item.as_ref())
            .map(|item| item.label.chars().count());
        assert_eq!(label, Some(carried), "the entry names the card as written");

        NAMING.set(verified);
        // The kept entry reads back as it was written, a whole text too.
        let kept = doc
            .get_changes(&[])
            .last()
            .and_then(|change| change.message())
            .and_then(ActionContext::from_change_message)
            .and_then(|entry| entry.item)
            .map(|item| item.label.chars().count());
        assert_eq!(kept, Some(carried), "the kept entry reads as written");
        assert_eq!(
            verify_change(&mut doc, hash, &registry).unwrap(),
            Verified::Reproduced
        );
        let undo = tilo_actions::executor::verified_compensation(&mut doc, hash, &registry)
            .unwrap()
            .expect("the change's undo is found");
        undo.apply(&mut doc).unwrap();
        assert_eq!(title(&doc).as_deref(), Some(long.as_str()));
    }
}

/// An entry names the item it is about by one line, never by what the item
/// reads: a message is labelled by its text, and a label kept whole put the
/// whole reply so far into every change that grew it.
#[test]
fn an_entry_names_its_item_by_one_line() {
    let on = |id: &str, label: &str| {
        Described::new(Did::verb(ActionKey::default(), []))
            .on(id, label)
            .item
            .unwrap()
    };
    let reply = "word ".repeat(12_000);
    let item = on("m1", &format!("\n  {reply}\nA second line"));
    assert_eq!(item.id, "m1");
    assert_eq!(
        item.label.chars().count(),
        tilo_actions::context::MAX_LABEL_CHARS,
        "a label of {} bytes",
        item.label.len()
    );
    assert!(reply.starts_with(&item.label), "{}", item.label);
    assert_eq!(on("s1", "First").label, "First");
    // An item with no name of its own is called by its id, whole: a reader
    // takes a label that repeats the id for no name.
    let id = "i".repeat(200);
    assert_eq!(on(&id, &id).label, id);
}
