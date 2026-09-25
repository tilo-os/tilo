use super::super::{
    CloudDeliveryError, Current, DeliveryOperation,
    data::tests::{Fixture, fixture, register},
};
use qos_relay::delivery_v3::state::{AdmissionCode, AdmissionOutcome, CommitScope};

/// Canonical browser-authored delta from the enrolled device key.
pub(in crate::https_owner) fn browser_delta(root: &std::path::Path, text: &str) -> Vec<u8> {
    crate::delivery::admission::tests::browser_delta(root, "agent-admission", text)
}

pub(in crate::https_owner) fn document_text(root: &std::path::Path) -> String {
    crate::delivery::admission::tests::document_text(root, "agent-admission")
}

fn admit(
    f: &Fixture,
    delivery: [u8; 16],
    canonical: &[u8],
) -> Result<AdmissionOutcome, CloudDeliveryError> {
    match f.source.with_state_turn(&f.pool, canonical.len(), |turn| {
        turn.admit_state_item(&f.session, delivery, canonical)
    }) {
        Current::Current(result) => result.map(|admission| admission.outcome().clone()),
        _ => panic!("actual Store turn required"),
    }
}

#[tokio::test]
async fn cloud_state_complete_item_commits_receipt_replay_and_document_once() {
    let f = fixture().await;
    register(&f);
    let canonical = browser_delta(f.root.path(), "hello");
    let first = admit(&f, [41; 16], &canonical).unwrap();
    let AdmissionOutcome::Accepted {
        scope: CommitScope::NativeCommitted,
        committed,
        operations,
    } = &first
    else {
        panic!("native commit required: {first:?}")
    };
    // The identical retry returns the original durable record, not a new commit.
    let generation = crate::Store::open(f.root.path(), "agent-admission")
        .unwrap()
        .state
        .generation;
    assert_eq!(admit(&f, [41; 16], &canonical).unwrap(), first);
    let store = crate::Store::open(f.root.path(), "agent-admission").unwrap();
    assert_eq!(store.state.generation, generation, "retry commits nothing");
    let ledger = store.state.delivery.as_ref().unwrap().ledger();
    let entry = &ledger.replay[&committed.sequence];
    assert_eq!(entry.digest, committed.digest);
    assert!(
        operations
            .iter()
            .all(|op| entry.admitted_operations.contains(op))
    );
    drop(store);
    assert!(document_text(f.root.path()).contains("hello"));
    // A changed body under the same identity is a permanent conflict.
    let other = browser_delta(f.root.path(), "other");
    assert!(matches!(
        admit(&f, [41; 16], &other).unwrap(),
        AdmissionOutcome::Rejected {
            code: AdmissionCode::ConflictingId
        }
    ));
    // The lost-reply lookup answers from the durable record only.
    let receipt =
        match f
            .source
            .with_delivery_turn(&f.pool, DeliveryOperation::ReceiptLookup, |turn| {
                turn.state_receipt(&f.session, [41; 16], *blake3::hash(&canonical).as_bytes())
            }) {
            Current::Current(result) => result.unwrap().unwrap(),
            _ => panic!("actual Store turn required"),
        };
    assert_eq!(receipt.outcome(), &first);
}

#[tokio::test]
async fn cloud_state_reservation_and_canonical_bounds_refuse_before_mutation() {
    let f = fixture().await;
    let canonical = browser_delta(f.root.path(), "bounded");
    let before = std::fs::read(f.root.path().join("state.json")).unwrap();
    // A turn reserved for fewer bytes cannot admit a larger item.
    assert!(matches!(
        f.source
            .with_state_turn(&f.pool, canonical.len() - 1, |turn| {
                turn.admit_state_item(&f.session, [42; 16], &canonical)
            }),
        Current::Current(Err(CloudDeliveryError::InvalidInput))
    ));
    // Non-canonical trailing bytes are rejected by exact re-encoding.
    let mut padded = canonical.clone();
    padded.push(0);
    assert!(matches!(
        admit(&f, [43; 16], &padded).unwrap(),
        AdmissionOutcome::Rejected {
            code: AdmissionCode::InvalidFrame
        }
    ));
    assert_eq!(
        std::fs::read(f.root.path().join("state.json")).unwrap(),
        before
    );
}
