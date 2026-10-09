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
