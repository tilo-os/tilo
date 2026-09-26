use super::*;
fn parse(method: &str, target: &str, proof: bool) -> Result<Parsed, Rejection> {
    let mut request = Request::builder()
        .method(method)
        .uri(target)
        .header("host", "localhost:9443")
        .header("origin", "https://localhost:9443");
    if proof {
        request = request.header(PROOF_HEADER, "00");
    }
    let (parts, _) = request.body(()).unwrap().into_parts();
    parse_head(&parts, &Origin::parse("https://localhost:9443").unwrap())
}
#[test]
fn onboarding_routes_are_exact_bounded_bulk_with_embedded_possession_proof() {
    for target in [
        "/peer/v3/join/inspect",
        "/peer/v3/join/challenge",
        "/peer/v3/join/complete",
        "/peer/v3/join/membership-challenge",
        "/peer/v3/join/membership-complete",
        "/peer/v3/onboarding/chunks",
    ] {
        let parsed = parse("POST", target, false).expect("exact signed onboarding route");
        assert_eq!(parsed.route.body_limit(), 256 * 1024);
        assert!(!parsed.route.control());
        assert!(parse("POST", &format!("{target}?x=1"), false).is_err());
        assert!(parse("GET", target, false).is_err());
        assert!(parse("POST", target, true).is_err());
    }
}
#[test]
fn detach_route_requires_exact_lowercase_session_id() {
    assert!(
        parse(
            "DELETE",
            "/peer/v3/sessions/000102030405060708090a0b0c0d0e0f",
            true
        )
        .is_ok()
    );
    for id in [
        "a",
        "000102030405060708090A0B0C0D0E0F",
        "000102030405060708090a0b0c0d0e0f00",
        "000102030405060708090a0b0c0d0e0g",
    ] {
        assert!(
            parse("DELETE", &format!("/peer/v3/sessions/{id}"), true).is_err(),
            "{id}"
        );
    }
}

#[test]
fn single_ingress_inventory_is_reserved_before_channel_publication() {
    let (ingress, receiver) = try_ingress_channel().expect("real fallible inventory construction");
    let available = receiver.available_bytes();
    assert!(
        available < GLOBAL_BYTES,
        "inventory metadata remains charged"
    );
    assert!(available >= crate::host_bytes::CONTROL_RESERVE_BYTES);
    assert_eq!(ingress.host_bytes().available_bytes(), available);
    let pool = ingress.host_bytes();
    drop(ingress);
    assert_eq!(pool.available_bytes(), available);
    drop(receiver);
    assert_eq!(pool.available_bytes(), GLOBAL_BYTES);
}

#[test]
fn cloned_ingress_and_receiver_share_one_large_decode_inventory() {
    let (ingress, receiver) = try_ingress_channel().unwrap();
    let work = ingress.host_work();
    assert!(work.same_pool(&ingress.host_bytes()));
    let lease = work.try_large_decode().unwrap();
    assert!(receiver.host_work().try_large_decode().is_err());
    assert!(ingress.clone().host_work().try_large_decode().is_err());
    drop(lease);
    assert!(receiver.host_work().try_large_decode().is_ok());
}

#[test]
fn parent_installation_private_routes_are_exact_bounded_bulk_and_proof_required() {
    for target in [
        "/peer/v3/commands/catalogue",
        "/peer/v3/commands/invoke",
        "/peer/v3/commands/status",
        "/peer/v3/commands/cancel",
        "/peer/v3/reads",
    ] {
        let parsed = parse("POST", target, true).expect("exact closed private route");
        assert_eq!(parsed.route.body_limit(), 256 * 1024);
        assert!(!parsed.route.control());
        assert!(parse("POST", target, false).is_err());
        assert!(parse("POST", &format!("{target}?x=1"), true).is_err());
        assert!(parse("GET", target, true).is_err());
    }
    let parsed = parse("GET", "/peer/v3/private/events?mode=poll", true).unwrap();
    assert_eq!(parsed.route.body_limit(), 0);
    assert!(!parsed.route.control());
    for target in [
        "/peer/v3/private/events",
        "/peer/v3/private/events?cursor=00&mode=poll",
        "/peer/v3/private/events?mode=poll&x=1",
    ] {
        assert!(parse("GET", target, true).is_err());
    }
}

fn queued(
    ingress: &Ingress,
    control: bool,
) -> (
    BufferedRequest,
    (watch::Sender<bool>, oneshot::Receiver<Reply>),
) {
    let target = if control {
        "/peer/v3/challenge"
    } else {
        "/peer/v3/state"
    };
    let parsed = parse("POST", target, !control).unwrap();
    let lease = ingress
        .reserve(
            parsed.route.body_limit(),
            if control { Lane::Control } else { Lane::Data },
        )
        .unwrap();
    let (cancel, cancelled) = watch::channel(false);
    let (response, _received) = oneshot::channel();
    (
        BufferedRequest {
            method: Method::POST,
            target: target.into(),
            body: Bytes::new(),
            proof: parsed.proof,
            route: parsed.route,
            deadline: Instant::now() + DEADLINE,
            cancelled,
            response,
            lease,
        },
        (cancel, _received),
    )
}
#[tokio::test]
async fn ingress_fairness_bounds_priority_batches_before_ready_bulk() {
    for asynchronous in [false, true] {
        let (ingress, mut receiver) = try_ingress_channel().unwrap();
        let (bulk, _bulk_cancel) = queued(&ingress, false);
        ingress.data.try_send(bulk).unwrap();
        let mut cancellations = Vec::new();
        for index in 0..=8 {
            let (control, cancel) = queued(&ingress, true);
            cancellations.push(cancel);
            ingress.control.try_send(control).unwrap();
            let next = if asynchronous {
                receiver.recv().await.unwrap()
            } else {
                receiver.try_recv().unwrap()
            };
            assert_eq!(
                next.route(),
                if index == 8 {
                    Route::State
                } else {
                    Route::Challenge
                },
                "async={asynchronous}, index={index}"
            );
        }
    }
}

#[test]
fn snapshot_routes_are_exact_bounded_bulk_and_proof_required() {
    let parsed = parse("POST", "/peer/v3/snapshots", true).expect("snapshot request route");
    assert_eq!(parsed.route, Route::Snapshot);
    assert_eq!(parsed.route.body_limit(), AUTH_BYTES);
    assert!(!parsed.route.control());
    assert!(parse("POST", "/peer/v3/snapshots", false).is_err());
    assert!(parse("POST", "/peer/v3/snapshots?x=1", true).is_err());
    assert!(parse("GET", "/peer/v3/snapshots", true).is_err());
    let pin = "000102030405060708090a0b0c0d0e0f";
    for index in ["0", "7", "1104"] {
        let target = format!("/peer/v3/snapshots/{pin}/chunks/{index}");
        let parsed = parse("GET", &target, true).expect("snapshot chunk route");
        assert_eq!(parsed.route, Route::SnapshotChunk);
        assert_eq!(parsed.route.body_limit(), 0);
        assert!(!parsed.route.control());
        assert!(parse("GET", &target, false).is_err());
        assert!(parse("POST", &target, true).is_err());
    }
    assert_eq!(
        snapshot_chunk_target(&format!("/peer/v3/snapshots/{pin}/chunks/3")),
        Some(([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15], 3))
    );
    for index in ["", "01", "1105", "-1", "a", "12345"] {
        assert!(
            parse(
                "GET",
                &format!("/peer/v3/snapshots/{pin}/chunks/{index}"),
                true
            )
            .is_err(),
            "{index}"
        );
    }
    for pin in [
        "00",
        "000102030405060708090A0B0C0D0E0F",
        "000102030405060708090a0b0c0d0e0g",
    ] {
        assert!(parse("GET", &format!("/peer/v3/snapshots/{pin}/chunks/0"), true).is_err());
    }
    assert!(
        parse(
            "GET",
            &format!("/peer/v3/snapshots/{pin}/chunks/0?x=1"),
            true
        )
        .is_err()
    );
}
