use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn spec(calls: &Arc<AtomicUsize>) -> ActionSpec {
    let calls = calls.clone();
    ActionSpec::deferred_operation(
        "create",
        "Create",
        OperationKind::Effect,
        "test.create",
        move || {
            EncodedAction::encode_operation("test.create", &calls.fetch_add(1, Ordering::SeqCst))
                .unwrap()
        },
    )
}
fn resource(spec: ActionSpec) -> Resource {
    Resource::new("test", "test", "Test").with_action(spec)
}

#[test]
fn deferred_construction_discovery_protocol_and_control_never_build() {
    let calls = Arc::new(AtomicUsize::new(0));
    let r = resource(spec(&calls));
    for _ in 0..3 {
        assert_eq!(
            crate::door::survey(std::slice::from_ref(&r))[0].input,
            crate::InputSpec::None
        );
        let handlers = std::collections::HashSet::from(["test.create".to_owned()]);
        assert!(
            crate::protocol::validate_surface(
                std::slice::from_ref(&r),
                &crate::ActionRegistry::new(),
                &handlers
            )
            .is_empty()
        );
        let unit = ActionBinding::unit(&r, &r.actions[0]).unwrap();
        let json = ActionBinding::static_json(&r, &r.actions[0], serde_json::Map::new()).unwrap();
        assert!(unit.invoke(()).unwrap().arguments.is_empty());
        assert!(json.invoke(()).unwrap().arguments.is_empty());
    }
    assert!(r.actions[0].fixed_invocation_encoding().is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
#[test]
fn deferred_both_actual_encoder_lanes_build_once_per_invocation() {
    let calls = Arc::new(AtomicUsize::new(0));
    let resources = [resource(spec(&calls))];
    let address = crate::ActionAddress::new("test", "create");
    let mut values = Vec::new();
    for _ in 0..2 {
        values.push(crate::door::encode(&resources, &address, None).unwrap());
        values
            .push(crate::door::encode_json(&resources, &address, &serde_json::Map::new()).unwrap());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    for (i, encoded) in values.iter().enumerate() {
        assert_eq!(
            serde_json::from_slice::<usize>(&encoded.payload).unwrap(),
            i
        );
    }
}
#[test]
fn deferred_nonempty_arguments_refuse_before_builder() {
    let calls = Arc::new(AtomicUsize::new(0));
    let resources = [resource(spec(&calls))];
    let address = crate::ActionAddress::new("test", "create");
    assert!(crate::door::encode(&resources, &address, Some("")).is_err());
    let args = serde_json::from_value(serde_json::json!({"x":1})).unwrap();
    assert!(crate::door::encode_json(&resources, &address, &args).is_err());
    assert!(ActionBinding::static_json(&resources[0], &resources[0].actions[0], args).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
#[test]
fn deferred_wrong_builder_type_refuses_both_lanes() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let wrong = ActionSpec::deferred_operation(
        "create",
        "Create",
        OperationKind::Effect,
        "test.create",
        move || {
            count.fetch_add(1, Ordering::SeqCst);
            EncodedAction::encode_operation("test.wrong", &()).unwrap()
        },
    );
    let resources = [resource(wrong)];
    let address = crate::ActionAddress::new("test", "create");
    assert!(crate::door::encode(&resources, &address, None).is_err());
    assert!(crate::door::encode_json(&resources, &address, &serde_json::Map::new()).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
#[test]
fn deferred_mutated_public_declaration_refuses_before_either_builder() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut changed = spec(&calls);
    let Lane::Deferred { type_id } = &mut changed.type_mut().lane else {
        panic!("deferred")
    };
    *type_id = "test.replaced".into();
    let resources = [resource(changed)];
    let address = crate::ActionAddress::new("test", "create");
    assert!(crate::door::encode(&resources, &address, None).is_err());
    assert!(crate::door::encode_json(&resources, &address, &serde_json::Map::new()).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn deferred_changed_operation_type_refuses_without_minting() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut changed = spec(&calls);
    changed.type_mut().operation_type = Some("test.changed-handler".into());
    let resources = [resource(changed)];
    let address = crate::ActionAddress::new("test", "create");
    assert!(crate::door::encode(&resources, &address, None).is_err());
    assert!(crate::door::encode_json(&resources, &address, &serde_json::Map::new()).is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct DeferredLocusAction {
    server: bool,
}
impl crate::Action for DeferredLocusAction {
    fn apply(&self, doc: &mut crate::Doc) -> Result<(), crate::ActionError> {
        use automerge::transaction::Transactable;
        doc.put(automerge::ROOT, "effect", true)?;
        Ok(())
    }
    fn locus(&self) -> ExecutionLocus {
        if self.server {
            ExecutionLocus::ServerPreferred
        } else {
            ExecutionLocus::ClientLocal
        }
    }
}
#[derive(Default)]
struct AcceptingDeferredTransport {
    submitted: usize,
}
impl crate::ActionTransport for AcceptingDeferredTransport {
    fn replicate(&mut self, _: &crate::ActionEnvelope) {}
    fn submit_to_host(&mut self, _: &crate::ActionEnvelope) -> crate::HostDecision {
        self.submitted = self.submitted.saturating_add(1);
        crate::HostDecision::AcceptedByHost
    }
}
fn invoke_deferred_document(
    declared_server: bool,
    actual_server: bool,
    json: bool,
) -> (
    Result<crate::Dispatched, crate::DoorError>,
    usize,
    bool,
    usize,
) {
    use automerge::ReadDoc;
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let spec = ActionSpec::deferred(
        "create",
        "Create",
        if declared_server {
            ExecutionLocus::ServerPreferred
        } else {
            ExecutionLocus::ClientLocal
        },
        "test.document",
        move || {
            count.fetch_add(1, Ordering::SeqCst);
            EncodedAction::encode(
                "test.document",
                &DeferredLocusAction {
                    server: actual_server,
                },
            )
            .unwrap()
        },
    );
    let resources = [resource(spec)];
    let address = crate::ActionAddress::new("test", "create");
    let mut registry = crate::ActionRegistry::new();
    registry.register::<DeferredLocusAction>("test.document");
    let mut doc = automerge::AutoCommit::new();
    let mut transport = AcceptingDeferredTransport::default();
    let result = if json {
        crate::door::invoke_json(
            &mut doc,
            &resources,
            &address,
            &serde_json::Map::new(),
            crate::door::InvocationProvenance::caller(b"owner"),
            &registry,
            &mut transport,
        )
    } else {
        crate::door::invoke(
            &mut doc,
            &resources,
            &address,
            None,
            crate::door::InvocationProvenance::caller(b"owner"),
            &registry,
            &mut transport,
        )
    };
    (
        result,
        calls.load(Ordering::SeqCst),
        doc.get(automerge::ROOT, "effect").unwrap().is_some(),
        transport.submitted,
    )
}
#[test]
fn deferred_encoded_only_transport_cannot_mint_remote_creation() {
    for json in [false, true] {
        let (result, calls, effect, submitted) = invoke_deferred_document(true, true, json);
        assert!(
            result.is_err(),
            "remote creation requires the address-based owner door"
        );
        assert_eq!(calls, 0);
        assert!(!effect);
        assert_eq!(submitted, 0);
    }
}
#[test]
fn deferred_core_owner_checks_actual_decoded_locus_before_effect() {
    for json in [false, true] {
        let (result, calls, effect, submitted) = invoke_deferred_document(false, true, json);
        assert!(result.is_err());
        assert_eq!(calls, 1);
        assert!(!effect);
        assert_eq!(submitted, 0);
        let (result, calls, effect, submitted) = invoke_deferred_document(false, false, json);
        assert!(result.is_ok());
        assert_eq!(calls, 1);
        assert!(effect);
        assert_eq!(submitted, 0);
    }
}
