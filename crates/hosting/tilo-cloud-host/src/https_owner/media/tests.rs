use super::*;
use crate::Store;
use std::sync::atomic::Ordering;
use tilo_workspace_host::ingress::MediaService;
use tilo_workspace_host::owner_turn::{HostOwner, Observed, OwnerTurn};

#[test]
fn cloud_media_source_uses_one_actual_store_turn_and_only_real_contention_is_busy() {
    let (dir, mut store, _) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(tilo_delivery_protocol::clock::now_ns())
        .unwrap();
    let source = CloudStoreSource::bind(store).unwrap();
    let before = source.0.turns.load(Ordering::Relaxed);
    let Observed::Current((workspace, continuity)) = HostOwner::with_current(&source, |turn| {
        assert!(matches!(
            crate::delivery::access::try_open(dir.path(), "agent-admission"),
            Err(crate::delivery::access::AccessError::Busy)
        ));
        (turn.workspace(), turn.continuity())
    }) else {
        panic!("actual current Store required")
    };
    assert_ne!(workspace, 0);
    assert_eq!(source.0.turns.load(Ordering::Relaxed) - before, 1);
    let _service =
        MediaService::from_source(source.clone(), "https://localhost:9443/moq/voice").unwrap();
    let held = Store::open(dir.path(), "agent-admission").unwrap();
    assert!(matches!(
        HostOwner::with_current(&source, |_| ()),
        Observed::Busy
    ));
    drop(held);
    assert!(
        matches!(HostOwner::with_current(&source,|turn|turn.continuity()),Observed::Current(current) if current==continuity)
    );
    source.retire();
    assert!(matches!(
        HostOwner::with_current(&source, |_| ()),
        Observed::Denied
    ));
}
