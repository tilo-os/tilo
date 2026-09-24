use super::*;
use crate::Store;
use qos_session_peer::ingress::MediaService;
use std::sync::atomic::Ordering;

#[test]
fn cloud_media_source_uses_one_actual_store_turn_and_only_real_contention_is_busy() {
    let (dir, mut store, _) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    let source = CloudStoreSource::bind(&store).unwrap();
    drop(store);
    let before = source.0.turns.load(Ordering::Relaxed);
    let MediaObservation::Current((workspace, continuity)) =
        MediaAuthoritySource::with_current(&source, |turn| {
            assert!(matches!(
                crate::delivery::access::try_open(dir.path(), "agent-admission"),
                Err(crate::delivery::access::AccessError::Busy)
            ));
            (turn.workspace(), turn.continuity())
        })
    else {
        panic!("actual current Store required")
    };
    assert_ne!(workspace, 0);
    assert_eq!(source.0.turns.load(Ordering::Relaxed) - before, 1);
    let _service =
        MediaService::from_source(source.clone(), "https://localhost:9443/moq/v21/voice").unwrap();
    let held = Store::open(dir.path(), "agent-admission").unwrap();
    assert!(matches!(
        MediaAuthoritySource::with_current(&source, |_| ()),
        MediaObservation::Busy
    ));
    drop(held);
    assert!(
        matches!(MediaAuthoritySource::with_current(&source,|turn|turn.continuity()),MediaObservation::Current(current) if current==continuity)
    );
    std::fs::write(dir.path().join("state.json"), b"{broken").unwrap();
    assert!(matches!(
        MediaAuthoritySource::with_current(&source, |_| ()),
        MediaObservation::Denied
    ));
}
