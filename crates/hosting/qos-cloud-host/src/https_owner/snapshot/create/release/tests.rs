use super::super::super::tests::{run, snapshot_fixture};
use super::super::chunk::tests::live;
use super::*;
use crate::https_owner::data::tests::register;
#[tokio::test]
async fn cloud_snapshot_release_owner_barrier_precedes_physical_cleanup_and_no_ack() {
    let f = snapshot_fixture().await;
    register(&f);
    let live = live(&f);
    let id = live.draft.prepared.selection.pin;
    let path = f
        .root
        .path()
        .join("snapshots-v1")
        .join(format!("{}.pair", hex::encode(id)));
    let released = run(&f, DeliveryOperation::SnapshotRelease, |turn| {
        live.release(turn, &f.session)
    })
    .unwrap();
    assert!(path.exists());
    run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        assert!(!turn.delivery_ledger().pins.contains_key(&id));
        assert!(
            turn.delivery_ledger().consumers[&f.device]
                .acknowledged
                .is_none()
        );
        Ok(())
    })
    .unwrap();
    released.cleanup().unwrap();
    assert!(!path.exists());
}
