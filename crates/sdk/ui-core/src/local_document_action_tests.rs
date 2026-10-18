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
        custody: None,
        invocation: uuid::Uuid::new_v4(),
        capture_id: uuid::Uuid::new_v4(),
        revision: private.into(),
        published: Some(PublishedInvocation {
            resource: private.into(),
            action: "set".into(),
            arguments: serde_json::Map::from_iter([("text".into(), private.into())]),
        }),
        encoded: EncodedAction::encode("test.private", &Paste(private.into())).unwrap(),
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
    assert_ne!(captured[0].capture_id, captured[1].capture_id);
    for capture in &captured {
        assert_eq!(capture.invocation, capture.bound_invocation().unwrap());
        let retry = capture.clone();
        assert_eq!(retry.capture_id, capture.capture_id);
        assert_eq!(retry.bound_invocation().unwrap(), capture.invocation);
        let mut without_mutations = capture.clone();
        without_mutations.mutation_hashes.clear();
        assert_ne!(
            without_mutations.bound_invocation().unwrap(),
            capture.invocation
        );
    }
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
    host.try_dispatch(&mut doc, "noop", &Noop).unwrap();
    let captured = queue.drain();
    assert_eq!(captured.len(), 2);
    assert_ne!(captured[0].capture_id, captured[1].capture_id);
    assert_ne!(captured[0].invocation, captured[1].invocation);
    for capture in captured {
        assert!(capture.mutation_hashes.is_empty());
        assert!(capture.published.is_some());
        assert_eq!(capture.invocation, capture.bound_invocation().unwrap());
    }
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
fn missing_published_invocation_refuses_observation_not_legacy_edits() {
    let queue = Queue::new();
    let _scope = queue.enter("snapshot".into());
    let mut doc = AutoCommit::new();
    let before = doc.get_heads();
    let mut host = crate::action_host::ActionHost::new();
    host.try_dispatch(&mut doc, "put", &Put(1)).unwrap();
    assert_ne!(doc.get_heads(), before);
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
fn reservation_counts_transport_bytes_with_bounded_hash_allowance() {
    for arguments in [
        serde_json::Map::new(),
        serde_json::Map::from_iter([("text".into(), "quotes: \" \\ \n café 🦀".into())]),
    ] {
        let queue = Queue::new();
        let _scope = queue.enter("snapshot".into());
        let _invocation = with_invocation(PublishedInvocation {
            resource: "body".into(),
            action: "set".into(),
            arguments: arguments.clone(),
        });
        let mut doc = AutoCommit::new();
        let mut host = crate::action_host::ActionHost::new();
        // Include authenticated provenance in both capture identity and budget.
        let custody = runtime_collab::committed_document::ConnectionSnapshot {
            root: [1; 32],
            device: [2; 32],
            agent: Some("agent".into()),
            app_id: "app".into(),
            room: "room".into(),
            document: "document".into(),
        };
        ACTIVE.with_borrow_mut(|active| {
            active.as_mut().unwrap().custody = Some(custody.clone());
        });
        // Include an existing head and context (including a duplicated group).
        doc.put(ROOT, "existing", true).unwrap();
        let context = ActionContext::default()
            .with_purpose("quoted \" purpose\n")
            .with_group("gesture", "Quoted \" group", "edit");
        host.try_dispatch_with_context(&mut doc, "put", &Put(1), context)
            .unwrap();
        let bytes = queue.0.borrow().bytes;
        let actions = queue.drain_leased();
        let action = &actions[0].0;
        assert_eq!(action.custody, Some(custody));
        assert_eq!(
            action.clone().into_report().unwrap().custody,
            action.custody
        );
        let mut report = runtime_collab::LocalDocumentActionReport {
            custody: action.custody.clone(),
            version: 1,
            invocation: action.invocation,
            capture_id: action.capture_id,
            revision: action.revision.clone(),
            resource: "body".into(),
            action: "set".into(),
            argument: (!arguments.is_empty()).then(|| {
                format!(
                    "{}{}",
                    tilo_actions::ARGUMENTS_JSON_PREFIX,
                    serde_json::Value::Object(arguments)
                )
            }),
            encoded: action.encoded.clone(),
            context: action.context.clone(),
            group: action.context.as_ref().and_then(|cx| cx.group.clone()),
            original_heads: Vec::new(),
            mutation_hashes: Vec::new(),
        };
        let base = report.encode().unwrap().len();
        assert_eq!(bytes, base + (130 - 1) + (256 * 130 - 1));
        report.original_heads.clone_from(&action.original_heads);
        report.mutation_hashes.clone_from(&action.mutation_hashes);
        assert!(report.encode().unwrap().len() <= bytes);
    }
}

#[test]
fn reservation_accepts_exact_wire_limit_and_refuses_one_extra_byte() {
    let queue = Queue::new();
    let _scope = queue.enter("snapshot".into());
    let _invocation = published();
    let mut host = crate::action_host::ActionHost::new();
    host.try_dispatch_with_context(
        &mut AutoCommit::new(),
        "put",
        &Put(1),
        ActionContext::default().with_purpose("x"),
    )
    .unwrap();
    let baseline = queue.0.borrow().bytes;
    drop(queue.drain());
    for extra in [0, 1] {
        let mut doc = AutoCommit::new();
        let result = host.try_dispatch_with_context(
            &mut doc,
            "put",
            &Put(1),
            ActionContext::default().with_purpose("x".repeat(64 * 1024 - baseline + 1 + extra)),
        );
        if extra == 0 {
            result.unwrap();
            assert_eq!(queue.0.borrow().bytes, 64 * 1024);
            assert_eq!(queue.drain().len(), 1);
        } else {
            assert!(result.is_err());
            assert!(doc.get_heads().is_empty());
            assert!(queue.drain().is_empty());
            assert!(!queue.take_refusals().is_empty());
        }
    }
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
