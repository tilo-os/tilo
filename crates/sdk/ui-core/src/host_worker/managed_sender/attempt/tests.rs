use super::*;
use crate::host_worker::managed_sender::{
    NativeSenderOutboxV1,
    tests::{pending_payload, pending_row},
};
fn route() -> RouteSpec {
    RouteSpec::record(
        "caller-app",
        uuid::Uuid::from_bytes([7; 16]),
        "caller-record",
    )
}
#[test]
fn managed_sender_attempt_single_claim_result_and_last_clone_keep_backing() {
    // No CurrentPrivateState or physical receipt exists.
    let bytes = pending_payload(&[pending_row(8, Some(""))]);
    let view = NativeSenderOutboxV1::decode(&bytes).unwrap();
    let row = view.find(&[8; 16]).unwrap().unwrap();
    let needed = AttemptOwner::queued_bytes(&row, "{}").unwrap();
    let (owner, attempt) = AttemptOwner::new(
        &row,
        [9; 16],
        route().cluster_id,
        Operation::Invoke,
        "{}",
        Instant::now(),
    )
    .unwrap();
    assert_eq!(attempt.meter_bytes(), needed);
    let duplicate = attempt.clone();
    assert!(!owner.quiescent());
    let Some(Ok(RequestView::Invoke {
        invocation,
        binding,
        revision,
        argument,
        ..
    })) = attempt.claim(&route())
    else {
        panic!("first exact request missing")
    };
    assert_eq!(invocation, &[8; 16]);
    assert_eq!(binding.host, [3; 16]);
    assert_eq!(binding.target_binding, [6; 32]);
    assert_eq!(revision, "revision-original");
    assert_eq!(argument, Some(""));
    assert!(duplicate.claim(&route()).is_none());
    let result = attempt
        .finish(Ok(PublishedInvocationObservation {
            host: [3; 16],
            invocation: [8; 16],
            result: None,
        }))
        .unwrap();
    assert!(owner.matches(&result));
    let last = result.clone();
    drop(result);
    assert!(!owner.quiescent());
    drop(duplicate);
    drop(last);
    assert!(owner.quiescent());
}
#[test]
fn managed_sender_attempt_wrong_route_retirement_and_oversized_confirmation_refuse() {
    let bytes = pending_payload(&[pending_row(8, None)]);
    let row = NativeSenderOutboxV1::decode(&bytes)
        .unwrap()
        .find(&[8; 16])
        .unwrap()
        .unwrap();
    assert!(
        AttemptOwner::new(
            &row,
            [9; 16],
            route().cluster_id,
            Operation::Observe,
            &"x".repeat(4097),
            Instant::now(),
        )
        .is_err()
    );
    let (owner, attempt) = AttemptOwner::new(
        &row,
        [9; 16],
        route().cluster_id,
        Operation::Observe,
        "",
        Instant::now(),
    )
    .unwrap();
    let mut foreign = route();
    foreign.app_id = "another".into();
    assert!(matches!(
        attempt.claim(&foreign),
        Some(Err(AttemptFailure::Unavailable))
    ));
    let result = attempt.finish(Err(AttemptFailure::Unavailable)).unwrap();
    assert!(owner.matches(&result));
    drop(result);
    drop(owner);
    let (owner, attempt) = AttemptOwner::new(
        &row,
        [9; 16],
        route().cluster_id,
        Operation::Cancel,
        "",
        Instant::now(),
    )
    .unwrap();
    drop(owner);
    assert!(matches!(
        attempt.claim(&route()),
        Some(Err(AttemptFailure::Unavailable))
    ));
}

#[test]
fn managed_sender_attempt_all_backings_are_released_after_last_result_drop() {
    use crate::semantic_document_read::retirement::measure_test_allocations;
    let bytes = pending_payload(&[pending_row(8, Some("original"))]);
    let row = NativeSenderOutboxV1::decode(&bytes)
        .unwrap()
        .find(&[8; 16])
        .unwrap()
        .unwrap();
    let selected_route = route();
    let ((), allocations) = measure_test_allocations(|| {
        let (owner, attempt) = AttemptOwner::new(
            &row,
            [9; 16],
            selected_route.cluster_id,
            Operation::Invoke,
            "{}",
            Instant::now(),
        )
        .unwrap();
        assert!(matches!(attempt.claim(&selected_route), Some(Ok(_))));
        let response = PublishedInvocationObservation {
            host: [3; 16],
            invocation: [8; 16],
            result: Some(runtime_collab::PublishedInvocationResult {
                admission_json: None,
                terminal_json: None,
                id: 4,
                update_json:
                    r#"{"state":"accepted","invocation":"08080808-0808-0808-0808-080808080808"}"#
                        .into(),
                uncertain: false,
                cancellation_requested: false,
            }),
        };
        let result = attempt.finish(Ok(response)).unwrap();
        let last = result.clone();
        drop(owner);
        drop(result);
        drop(last);
    });
    assert!(allocations.allocations > 0);
    assert_eq!(allocations.live, 0);
    let oversized = "x".repeat(4097);
    let ((), allocations) = measure_test_allocations(|| {
        assert!(
            AttemptOwner::new(
                &row,
                [9; 16],
                selected_route.cluster_id,
                Operation::Invoke,
                &oversized,
                Instant::now(),
            )
            .is_err()
        );
    });
    assert_eq!(
        allocations.allocations, 0,
        "refuse before copying any request"
    );
}
