//! An action whose meaning depends on its author reads who invoked it, and a
//! replaying replica reads the same.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT, ReadDoc};
use serde::{Deserialize, Serialize};
use tilo_actions::{
    Action, ActionContext, ActionError, ActionKey, ActionRegistry, Described, Did, Doc,
    EncodedAction, LocalTransport, Scalar, dispatch_with_context, invoking_context, replay,
};

/// Writes the work group of whoever invoked it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StampGroup;

const TYPE: &str = "test.stamp-group";

impl Action for StampGroup {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let group = invoking_context()
            .and_then(|context| context.group)
            .map_or_else(|| "none".to_owned(), |group| group.id);
        doc.put(ROOT, "group", group)?;
        Ok(())
    }
}

fn group(doc: &AutoCommit) -> Option<String> {
    doc.get(ROOT, "group")
        .unwrap()
        .map(|(value, _)| value.into_string().unwrap())
}

#[test]
fn apply_reads_the_invoking_context_and_replay_reads_it_again() {
    let mut doc = AutoCommit::new();
    let base = doc.fork();
    let context =
        ActionContext::for_resource("r", "k", "l").with_group("run-7", "Run", "agent-run");
    let encoded = EncodedAction::encode(TYPE, &StampGroup).unwrap();
    let mut transport = LocalTransport::new();
    dispatch_with_context(&mut doc, &StampGroup, encoded, &mut transport, context).unwrap();
    assert_eq!(group(&doc).as_deref(), Some("run-7"));
    assert!(
        invoking_context().is_none(),
        "the context ends with the apply"
    );

    let mut registry = ActionRegistry::new();
    registry.register::<StampGroup>(TYPE);
    let mut peer = base;
    replay(&mut peer, &transport.replicated[0], &registry).unwrap();
    assert_eq!(group(&peer).as_deref(), Some("run-7"));
    assert_eq!(peer.get_heads(), doc.get_heads(), "the same change");
}

#[test]
fn caller_context_cannot_install_executable_history_compensation() {
    let mut doc = AutoCommit::new();
    let encoded = EncodedAction::encode(TYPE, &StampGroup).unwrap();
    let context = ActionContext {
        compensation: Some(tilo_actions::context::ActionCompensation {
            undo: encoded.clone(),
            redo: encoded.clone(),
        }),
        ..ActionContext::for_resource("r", "k", "l")
    };
    let mut transport = LocalTransport::new();
    dispatch_with_context(&mut doc, &StampGroup, encoded, &mut transport, context).unwrap();
    assert!(
        transport.replicated[0]
            .context
            .as_ref()
            .unwrap()
            .compensation
            .is_none()
    );
    let message = doc
        .get_changes(&[])
        .last()
        .unwrap()
        .message()
        .unwrap()
        .to_owned();
    assert!(
        ActionContext::from_change_message(&message)
            .unwrap()
            .compensation
            .is_none()
    );
}

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

#[test]
fn canonical_executor_persists_the_actual_before_state_compensation_on_replay() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "counter", 7_i64).unwrap();
    doc.commit();
    let mut peer = doc.fork();
    let action = SetCounter(11);
    let encoded = EncodedAction::encode("test.counter", &action).unwrap();
    let mut transport = LocalTransport::new();
    dispatch_with_context(
        &mut doc,
        &action,
        encoded.clone(),
        &mut transport,
        ActionContext::for_resource("r", "k", "l"),
    )
    .unwrap();
    let compensation = transport.replicated[0]
        .context
        .as_ref()
        .unwrap()
        .compensation
        .as_ref()
        .unwrap();
    assert_eq!(
        compensation.undo,
        EncodedAction::encode("test.counter", &SetCounter(7)).unwrap()
    );
    assert_eq!(compensation.redo, encoded);
    let mut registry = ActionRegistry::new();
    registry.register::<SetCounter>("test.counter");
    replay(&mut peer, &transport.replicated[0], &registry).unwrap();
    assert_eq!(peer.get_heads(), doc.get_heads());
    let context = ActionContext::from_change_message(
        peer.get_changes(&[]).last().unwrap().message().unwrap(),
    )
    .unwrap();
    assert_eq!(context.compensation.as_ref(), Some(compensation));
}

/// Counts what it found: a step named by what it does.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Tally;

impl Action for Tally {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        doc.put(ROOT, "tally", 3_i64)?;
        Ok(())
    }

    fn describe(&self, doc: &Doc) -> Option<Described> {
        // Read before the apply: nothing tallied yet.
        let before = doc.get(ROOT, "tally").unwrap().is_none();
        before.then(|| {
            Described::new(Did::verb(
                ActionKey::new("test", "test.tally", "tally"),
                [("count".to_string(), Scalar::from(3))],
            ))
            .on("t", "Tally")
        })
    }
}

#[test]
fn a_step_records_what_it_did_and_replicas_replay_the_entry() {
    let mut doc = AutoCommit::new();
    let base = doc.fork();
    let context = ActionContext::for_resource("r", "k", "l");
    let encoded = EncodedAction::encode("test.tally", &Tally).unwrap();
    let mut transport = LocalTransport::new();
    dispatch_with_context(&mut doc, &Tally, encoded, &mut transport, context).unwrap();
    let sent = &transport.replicated[0];
    let entry = sent.context.as_ref().expect("an entry");
    assert_eq!(
        entry.what,
        Some(Did::verb(
            ActionKey::new("test", "test.tally", "tally"),
            [("count".to_string(), Scalar::from(3))],
        ))
    );
    assert_eq!(
        entry.item.as_ref().map(|item| item.label.as_str()),
        Some("Tally")
    );
    let mut registry = ActionRegistry::new();
    registry.register::<Tally>("test.tally");
    let mut peer = base;
    replay(&mut peer, sent, &registry).unwrap();
    assert_eq!(
        peer.get_heads(),
        doc.get_heads(),
        "the same change, name and all"
    );
}

#[test]
fn replay_refuses_a_forged_inverse_before_mutating_the_replica() {
    let mut origin = AutoCommit::new();
    origin.put(ROOT, "counter", 7_i64).unwrap();
    origin.commit();
    let mut peer = origin.fork();
    let before = peer.get_heads();
    let mut transport = LocalTransport::new();
    dispatch_with_context(
        &mut origin,
        &SetCounter(11),
        EncodedAction::encode("test.counter", &SetCounter(11)).unwrap(),
        &mut transport,
        ActionContext::for_resource("r", "k", "l"),
    )
    .unwrap();
    transport.replicated[0]
        .context
        .as_mut()
        .unwrap()
        .compensation
        .as_mut()
        .unwrap()
        .undo = EncodedAction::encode("test.counter", &SetCounter(99)).unwrap();
    let mut registry = ActionRegistry::new();
    registry.register::<SetCounter>("test.counter");
    assert!(replay(&mut peer, &transport.replicated[0], &registry).is_err());
    assert_eq!(peer.get_heads(), before);
    assert_eq!(
        peer.get(ROOT, "counter").unwrap().unwrap().0.as_i64(),
        Some(7)
    );
}

#[test]
fn raw_change_metadata_cannot_install_an_unrelated_compensation() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "counter", 7_i64).unwrap();
    doc.commit();
    let encoded = EncodedAction::encode("test.counter", &SetCounter(11)).unwrap();
    let context = ActionContext {
        compensation: Some(tilo_actions::context::ActionCompensation {
            undo: EncodedAction::encode("test.counter", &SetCounter(7)).unwrap(),
            redo: encoded,
        }),
        ..ActionContext::for_resource("r", "k", "l")
    };
    // A well-shaped inverse, but the actual mutation differs from its claim.
    doc.put(ROOT, "counter", 12_i64).unwrap();
    let hash = doc
        .commit_with(
            automerge::transaction::CommitOptions::default()
                .with_message(context.change_message().unwrap()),
        )
        .unwrap();
    let before = doc.get_heads();
    let mut registry = ActionRegistry::new();
    registry.register::<SetCounter>("test.counter");
    assert!(tilo_actions::executor::verified_compensation(&mut doc, hash, &registry).is_err());
    assert_eq!(doc.get_heads(), before);
    assert_eq!(
        doc.get(ROOT, "counter").unwrap().unwrap().0.as_i64(),
        Some(12)
    );
}
