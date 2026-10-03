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
    Action, ActionContext, ActionError, ActionRegistry, Doc, EncodedAction, LocalTransport,
    dispatch_with_context, invoking_context, replay,
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
        ActionContext::for_resource("Stamp", "r", "k", "l").with_group("run-7", "Run", "agent-run");
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
