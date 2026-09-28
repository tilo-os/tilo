use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn encoded(value: usize) -> EncodedAction {
    EncodedAction::encode_operation("test.effective", &value).unwrap()
}

#[test]
fn effective_primitive_fixed_witness_is_actual_private_builder_not_public_input() {
    let original = encoded(1);
    let mut spec =
        ActionSpec::immediate_encoded("run", "Run", ExecutionLocus::ClientLocal, original.clone());
    assert_eq!(spec.fixed_invocation_encoding(), Some(&original));
    let changed = encoded(2);
    spec.input = ActionInput::Immediate(changed.clone());
    assert_eq!(spec.fixed_invocation_encoding(), Some(&original));
    assert_eq!(spec.build_json(&serde_json::Map::new()).unwrap(), original);
    let resource = Resource::new("probe", "probe", "Probe").with_action(spec);
    assert_eq!(
        crate::door::encode(
            &[resource],
            &crate::ActionAddress::new("probe", "run"),
            None
        )
        .unwrap(),
        changed
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
