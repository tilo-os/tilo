use super::*;
use automerge::{ROOT, transaction::Transactable};
use serde::Serialize;
use tilo_actions::{Action, Doc, ExecutionLocus};

#[derive(Debug, Clone, Serialize)]
struct Put(u64);
impl Action for Put {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        doc.put(ROOT, "value", self.0)?;
        Ok(())
    }
    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

#[derive(Debug, Serialize)]
struct Noop;
impl Action for Noop {
    fn apply(&self, _: &mut Doc) -> Result<(), ActionError> {
        Ok(())
    }
    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

fn published() -> InvocationScope {
    with_invocation(PublishedInvocation {
        resource: "body".into(),
        action: "set".into(),
        arguments: serde_json::Map::new(),
    })
}

#[test]
fn legacy_local_action_keeps_behavior_without_fabricating_admission() {
    let queue = Queue::new();
    let _scope = queue.enter("snapshot".into());
    let mut doc = AutoCommit::new();
    let mut host = crate::action_host::ActionHost::new();
    host.try_dispatch(&mut doc, "put", &Put(7)).unwrap();
    assert!(!doc.get_heads().is_empty());
    assert!(queue.drain().is_empty());
    assert!(!queue.take_refusals().is_empty());
}

#[test]
fn capture_debug_does_not_disclose_private_intent() {
    let private = "private note body";
    let capture = CapturedAction {
        invocation: uuid::Uuid::new_v4(),
        revision: private.into(),
        published: Some(PublishedInvocation {
            resource: private.into(),
            action: "set".into(),
            arguments: serde_json::Map::from_iter([("text".into(), private.into())]),
        }),
        encoded: EncodedAction::encode("test.private", &private).unwrap(),
        context: Some(ActionContext::default().with_purpose(private)),
        original_heads: Vec::new(),
        mutation_hashes: Vec::new(),
    };
    assert!(!format!("{capture:?}").contains(private));
    assert!(!format!("{:?}", capture.published).contains(private));
}

#[test]
fn canonical_host_captures_each_execution_not_each_undo_step() {
    let queue = Queue::new();
    let mut doc = AutoCommit::new();
    let mut host = crate::action_host::ActionHost::new();
    // A native/background action outside local input has no user observation.
    host.try_dispatch(&mut doc, "put", &Put(0)).unwrap();
    assert!(queue.drain().is_empty());
    let before = doc.get_heads();
    let _scope = queue.enter("published-before-input".into());
    let _invocation = with_invocation(PublishedInvocation {
        resource: "child".into(),
        action: "set".into(),
        arguments: serde_json::Map::from_iter([("value".into(), 1.into())]),
    });
    host.try_dispatch(&mut doc, "parent.put", &Put(1)).unwrap();
    host.try_dispatch(&mut doc, "parent.put", &Put(2)).unwrap();
    let captured = queue.drain();
    assert_eq!(captured.len(), 2);
    assert_ne!(captured[0].invocation, captured[1].invocation);
    assert_eq!(captured[0].published.as_ref().unwrap().resource, "child");
    assert_eq!(captured[0].encoded.type_id, "parent.put");
    assert_eq!(
        captured[0].encoded.payload,
        serde_json::to_vec(&Put(1)).unwrap()
    );
    let mut expected = before.iter().map(|hash| hash.0).collect::<Vec<_>>();
    expected.sort_unstable();
    assert_eq!(captured[0].original_heads, expected);
    assert_eq!(captured[0].mutation_hashes.len(), 1);
    assert_eq!(captured[1].original_heads, captured[0].mutation_hashes);
    assert!(host.take_committed().is_some());
    assert!(host.take_committed().is_none());
}

#[test]
fn saturation_refuses_before_mutation_and_reply_restores_capacity() {
    let queue = Queue::new();
    let _scope = queue.enter("snapshot".into());
    let _published = published();
    let mut doc = AutoCommit::new();
    let mut host = crate::action_host::ActionHost::new();
    let mut accepted = 0;
    for index in 0..CAPACITY {
        if host
            .try_dispatch(&mut doc, "put", &Put(index as u64))
            .is_err()
        {
            break;
        }
        accepted += 1;
    }
    assert!(accepted > 0);
    let heads = doc.get_heads();
    assert!(host.try_dispatch(&mut doc, "put", &Put(999)).is_err());
    assert_eq!(doc.get_heads(), heads);
    assert!(!queue.take_refusals().is_empty());
    assert!(queue.take_refusals().is_empty());
    let in_flight = queue.drain_leased();
    assert_eq!(in_flight.len(), accepted);
    assert!(host.try_dispatch(&mut doc, "put", &Put(999)).is_err());
    assert_eq!(doc.get_heads(), heads);
    drop(in_flight);
    host.try_dispatch(&mut doc, "put", &Put(1000)).unwrap();
    assert_eq!(queue.drain().len(), 1);
}

#[test]
fn noops_keep_empty_actual_mutations_without_inventing_a_change() {
    let queue = Queue::new();
    let _scope = queue.enter("snapshot".into());
    let _published = published();
    let mut doc = AutoCommit::new();
    let mut host = crate::action_host::ActionHost::new();
    host.try_dispatch(&mut doc, "noop", &Noop).unwrap();
    let captured = queue.drain();
    assert_eq!(captured.len(), 1);
    assert!(captured[0].mutation_hashes.is_empty());
    assert!(captured[0].published.is_some());
}

#[test]
fn original_broker_continuations_and_native_work_are_not_local_user_actions() {
    let queue = Queue::new();
    let _scope = queue.enter("snapshot".into());
    let _published = published();
    let mut doc = AutoCommit::new();
    let mut host = crate::action_host::ActionHost::new();
    let source = tilo_actions::InvocationId(uuid::Uuid::new_v4());
    host.try_dispatch_with_context(
        &mut doc,
        "put",
        &Put(1),
        ActionContext::default().with_source_invocation(Some(source)),
    )
    .unwrap();
    {
        let _native = suspend();
        host.try_dispatch(&mut doc, "put", &Put(2)).unwrap();
    }
    assert!(queue.drain().is_empty());
    host.try_dispatch(&mut doc, "put", &Put(3)).unwrap();
    assert_eq!(queue.drain().len(), 1);
}

#[test]
fn missing_published_invocation_refuses_legacy_dispatch_before_mutation() {
    let queue = Queue::new();
    let _scope = queue.enter("snapshot".into());
    let mut doc = AutoCommit::new();
    let before = doc.get_heads();
    let mut host = crate::action_host::ActionHost::new();
    assert!(host.try_dispatch(&mut doc, "put", &Put(1)).is_err());
    assert_eq!(doc.get_heads(), before);
    assert!(!queue.take_refusals().is_empty());
    assert!(queue.drain().is_empty());
}

#[derive(Debug, Serialize)]
struct Paste(String);
impl Action for Paste {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        doc.put(ROOT, "paste", self.0.as_str())?;
        Ok(())
    }
    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

#[test]
fn escaped_payload_and_published_arguments_refuse_before_mutation() {
    for published_arguments in [false, true] {
        let queue = Queue::new();
        let _scope = queue.enter("snapshot".into());
        let oversized = "\"".repeat(32 * 1024);
        let _invocation = with_invocation(PublishedInvocation {
            resource: "body".into(),
            action: "paste".into(),
            arguments: if published_arguments {
                serde_json::Map::from_iter([("text".into(), oversized.clone().into())])
            } else {
                serde_json::Map::new()
            },
        });
        let mut doc = AutoCommit::new();
        let before = doc.get_heads();
        let mut host = crate::action_host::ActionHost::new();
        let text = if published_arguments {
            "small".into()
        } else {
            oversized
        };
        assert!(host.try_dispatch(&mut doc, "paste", &Paste(text)).is_err());
        assert_eq!(doc.get_heads(), before);
        assert!(!queue.take_refusals().is_empty());
        assert!(queue.drain().is_empty());
    }
}

#[test]
fn context_wire_budget_refuses_before_mutation() {
    let queue = Queue::new();
    let _scope = queue.enter("snapshot".into());
    let _invocation = published();
    let mut doc = AutoCommit::new();
    let mut host = crate::action_host::ActionHost::new();
    let context = ActionContext {
        purpose: Some("\n".repeat(32 * 1024)),
        ..ActionContext::default()
    };
    assert!(
        host.try_dispatch_with_context(&mut doc, "put", &Put(1), context)
            .is_err()
    );
    assert!(doc.get_heads().is_empty());
    assert!(!queue.take_refusals().is_empty());
}

#[test]
fn input_without_any_pinned_directory_refuses_before_mutation() {
    let _scope = enter_input(Vec::new());
    let _invocation = published();
    let mut doc = AutoCommit::new();
    let mut host = crate::action_host::ActionHost::new();
    assert!(host.try_dispatch(&mut doc, "put", &Put(1)).is_err());
    assert!(doc.get_heads().is_empty());
}

#[test]
fn nested_route_scope_restores_exact_snapshot_without_cross_route_drain() {
    let outer = Queue::new();
    let inner = Queue::new();
    let _scope = outer.enter("outer".into());
    let _published = published();
    let mut doc = AutoCommit::new();
    let mut host = crate::action_host::ActionHost::new();
    {
        let _scope = inner.enter("inner".into());
        let _published = published();
        host.try_dispatch(&mut doc, "put", &Put(1)).unwrap();
    }
    host.try_dispatch(&mut doc, "put", &Put(2)).unwrap();
    assert_eq!(inner.drain()[0].revision, "inner");
    assert_eq!(outer.drain()[0].revision, "outer");
}
