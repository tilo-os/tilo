use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn encoded(value: usize) -> EncodedAction {
    EncodedAction::encode_operation("test.effective", &value).unwrap()
}

#[test]
fn effective_primitive_fixed_witness_is_what_every_lane_fires() {
    let original = encoded(1);
    let spec =
        ActionSpec::immediate_encoded("run", "Run", ExecutionLocus::ClientLocal, original.clone());
    assert_eq!(spec.fixed_invocation_encoding(), Some(&original));
    assert_eq!(spec.encode_immediate().as_deref(), Some(&original));
    assert_eq!(spec.build_json(&serde_json::Map::new()).unwrap(), original);
    let resource = Resource::new("probe", "probe", "Probe").with_action(spec);
    assert_eq!(
        crate::door::encode(
            &[resource],
            &crate::ActionAddress::new("probe", "run"),
            None
        )
        .unwrap(),
        original
    );
}

#[test]
fn effective_primitive_eager_publication_shares_bytes_and_type() {
    let original = EncodedAction::encode_operation("test.eager", &"雪".repeat(24_000)).unwrap();
    let spec =
        ActionSpec::immediate_encoded("run", "Run", ExecutionLocus::ClientLocal, original.clone());
    let public = spec
        .encode_immediate()
        .expect("an eager action remains eager");
    let InvocationBuilder::Fixed(private) = &spec.build else {
        panic!("an eager action retains its private fixed witness");
    };
    assert!(Arc::ptr_eq(&public, private), "one immutable allocation");
    let cloned = spec.clone();
    assert!(
        Arc::ptr_eq(spec.action_type(), cloned.action_type()),
        "publication clones share the type"
    );
    assert!(
        Arc::ptr_eq(&public, &cloned.encode_immediate().unwrap()),
        "publication clones share payload bytes"
    );
    assert_eq!(cloned.fixed_invocation_encoding(), Some(&original));
    let resource = Resource::new("probe", "probe", "Probe").with_action(cloned);
    assert_eq!(
        crate::door::encode(
            &[resource],
            &crate::ActionAddress::new("probe", "run"),
            None
        )
        .unwrap(),
        original
    );
}

#[test]
fn effective_primitive_typed_zero_remains_dynamic_and_witness_does_not_execute() {
    let calls = Arc::new(AtomicUsize::new(0));
    let worker = calls.clone();
    let spec = ActionSpec::typed_operation(
        "run",
        "Run",
        "",
        OperationKind::Effect,
        "test.effective",
        vec![],
        move |_| encoded(worker.fetch_add(1, Ordering::SeqCst)),
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "existing palette projection behavior is preserved"
    );
    assert!(spec.fixed_invocation_encoding().is_none());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "inspection must never execute an opaque builder"
    );
    let resource = Resource::new("probe", "probe", "Probe").with_action(spec.clone());
    assert_eq!(
        crate::door::encode(
            &[resource],
            &crate::ActionAddress::new("probe", "run"),
            None
        )
        .unwrap(),
        encoded(0)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        spec.build_json(&serde_json::Map::new()).unwrap(),
        encoded(1)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn effective_primitive_installed_unknown_is_not_absent_evidence() {
    let spec = ActionSpec::immediate_encoded("run", "Run", ExecutionLocus::ClientLocal, encoded(1));
    assert!(!spec.has_continuation_identity());
    let spec = spec.with_continuation_identity(|_| None);
    assert!(spec.has_continuation_identity());
    assert!(
        spec.continuation_identity(ContinuationQuery::Draft)
            .is_none()
    );
}

fn canonical(evidence: &ContinuationEvidence) -> Vec<u8> {
    let mut output = Vec::new();
    evidence.write_canonical_identity(&mut output).unwrap();
    output
}

#[test]
fn effective_primitive_canonical_stream_preserves_variant_and_tuple_boundaries() {
    struct Refuse;
    impl std::io::Write for Refuse {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("sink refused"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let target = ContinuationKey::new("domain", [b"ab".as_slice(), b"c".as_slice()]);
    let handler = ContinuationKey::new("handler", [b"v1".as_slice()]);
    let exact = ContinuationEvidence::Exact {
        target: target.clone(),
        handler: handler.clone(),
    };
    let draft = ContinuationEvidence::Draft {
        resolution_domain: target,
        handler: handler.clone(),
    };
    let split = ContinuationEvidence::Exact {
        target: ContinuationKey::new("domain", [b"a".as_slice(), b"bc".as_slice()]),
        handler,
    };
    assert_eq!(canonical(&exact), canonical(&exact.clone()));
    assert_ne!(canonical(&exact), canonical(&draft));
    assert_ne!(canonical(&exact), canonical(&split));
    assert_eq!(
        exact
            .write_canonical_identity(&mut Refuse)
            .unwrap_err()
            .to_string(),
        "sink refused"
    );
}

// A typed action whose parameters all mean something when left out fires at once for a person,
// built per target with nothing bound, while a caller that passes an argument still binds it.
#[test]
fn an_all_optional_typed_action_fires_at_once_and_still_binds_arguments() {
    let built = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&built);
    let spec = ActionSpec::typed_on(
        "check",
        "Check",
        "Check",
        ExecutionLocus::ClientLocal,
        vec![ActionParam::optional_owned(
            "done",
            ParamType::Boolean,
            "done: Bool, when left out: not item.done",
        )],
        move |target, args| {
            counted.fetch_add(1, Ordering::SeqCst);
            let on = target.get::<String>().cloned().unwrap_or_default();
            let done = args.to_json().get("done").cloned();
            EncodedAction::encode_operation("test.check", &(on, done)).unwrap()
        },
    );
    assert!(
        matches!(spec.input(), ActionInput::Prompted { .. }),
        "a typed action asks for its first parameter"
    );
    let spec = spec.with_immediate_per_target();
    assert!(matches!(spec.input(), ActionInput::Immediate(None)));
    assert_eq!(
        built.load(Ordering::SeqCst),
        0,
        "declaring it builds nothing"
    );
    let bound = spec.action_type().bind(Target::new("item-1".to_owned()));
    let fired = bound.encode_immediate().expect("fires at once");
    assert_eq!(
        *fired,
        EncodedAction::encode_operation("test.check", &("item-1".to_owned(), None::<bool>))
            .unwrap()
    );
    let mut args = serde_json::Map::new();
    args.insert("done".into(), serde_json::Value::Bool(false));
    assert_eq!(
        bound.build_json(&args).unwrap(),
        EncodedAction::encode_operation("test.check", &("item-1".to_owned(), Some(false))).unwrap(),
        "an explicit argument is bound"
    );
    let required = ActionSpec::typed_on(
        "rename",
        "Rename",
        "Rename",
        ExecutionLocus::ClientLocal,
        vec![ActionParam::required_owned(
            "title",
            ParamType::Text,
            "title: Text",
        )],
        |_, _| encoded(0),
    )
    .with_immediate_per_target();
    assert!(
        matches!(required.input(), ActionInput::Prompted { .. }),
        "a required parameter is still asked for"
    );
}
