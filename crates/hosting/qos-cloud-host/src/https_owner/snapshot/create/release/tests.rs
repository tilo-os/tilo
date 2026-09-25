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

/// Lane T: pins of abandoned transfers (a failed download, a restart) are
/// released when the same device starts a new one, instead of filling the
/// `MAX_PINS` table until expiry and refusing every device's snapshots.
#[tokio::test]
async fn cloud_snapshot_orphan_pins_of_a_device_are_released_but_live_ones_kept() {
    let f = snapshot_fixture().await;
    register(&f);
    let kept = live(&f);
    let orphan = live(&f);
    let kept_id = kept.draft.prepared.selection.pin;
    let orphan_id = orphan.draft.prepared.selection.pin;
    // The transfer is abandoned without a release (its LiveSnapshot is gone).
    drop(orphan);
    run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        assert!(turn.delivery_ledger().pins.contains_key(&orphan_id));
        Ok(())
    })
    .unwrap();
    let released = run(&f, DeliveryOperation::SnapshotRelease, |turn| {
        turn.release_orphan_snapshot_pins(f.device, &[kept_id])
    })
    .unwrap();
    assert_eq!(released, vec![orphan_id]);
    run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        let pins = &turn.delivery_ledger().pins;
        assert!(!pins.contains_key(&orphan_id));
        assert!(pins.contains_key(&kept_id));
        Ok(())
    })
    .unwrap();
    // Nothing left to release; another device's call spares the live pin
    // and, with the table far from full, other devices' pins.
    let again = run(&f, DeliveryOperation::SnapshotRelease, |turn| {
        turn.release_orphan_snapshot_pins(f.device, &[kept_id])
    })
    .unwrap();
    assert!(again.is_empty());
    let other = run(&f, DeliveryOperation::SnapshotRelease, |turn| {
        turn.release_orphan_snapshot_pins([0xAB; 32], &[])
    })
    .unwrap();
    assert!(other.is_empty());
    // Only the matching operation may release.
    assert!(
        run(&f, DeliveryOperation::SnapshotSelect, |turn| {
            turn.release_orphan_snapshot_pins(f.device, &[])
        })
        .is_err()
    );
}

/// A nearly full pin table is relieved of the oldest pins no live transfer
/// serves, whichever device holds them, so one device's abandoned pins can
/// never refuse every other device's snapshot.
#[tokio::test]
async fn cloud_snapshot_full_pin_table_releases_oldest_orphans_of_any_device() {
    use qos_relay::delivery_v3::commit::MAX_PINS;
    let f = snapshot_fixture().await;
    register(&f);
    let kept = live(&f);
    let kept_id = kept.draft.prepared.selection.pin;
    for _ in 1..MAX_PINS - 1 {
        drop(live(&f));
    }
    let count = |f| {
        run(f, DeliveryOperation::SnapshotSelect, |turn| {
            Ok(turn.delivery_ledger().pins.len())
        })
        .unwrap()
    };
    assert_eq!(count(&f), MAX_PINS - 1);
    let released = run(&f, DeliveryOperation::SnapshotRelease, |turn| {
        turn.release_orphan_snapshot_pins([0xAB; 32], &[kept_id])
    })
    .unwrap();
    assert_eq!(count(&f), MAX_PINS / 2);
    assert_eq!(released.len(), MAX_PINS - 1 - MAX_PINS / 2);
    assert!(!released.contains(&kept_id));
    run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        assert!(turn.delivery_ledger().pins.contains_key(&kept_id));
        Ok(())
    })
    .unwrap();
}

/// A sidecar whose ledger pin is gone (a delivery rotation drops every pin)
/// is unlinked by the sweep; a sidecar still pinned or leased is kept.
#[tokio::test]
async fn cloud_snapshot_sweep_unlinks_sidecars_without_a_ledger_pin() {
    let f = snapshot_fixture().await;
    register(&f);
    let kept = live(&f);
    let kept_id = kept.draft.prepared.selection.pin;
    let gone = live(&f);
    let gone_id = gone.draft.prepared.selection.pin;
    drop(gone);
    let path = |id: [u8; 16]| {
        f.root
            .path()
            .join("snapshots-v1")
            .join(format!("{}.pair", hex::encode(id)))
    };
    assert!(path(gone_id).exists() && path(kept_id).exists());
    // Pinned sidecars survive the sweep.
    let removed = run(&f, DeliveryOperation::Maintenance, |turn| {
        Ok(crate::https_owner::snapshot::remove_unpinned_sidecars(
            turn,
            &f.host_work(),
        ))
    })
    .unwrap();
    assert_eq!(removed, 0);
    // The ledger loses the pin without a sidecar cleanup (as a rotation does).
    run(&f, DeliveryOperation::SnapshotRelease, |turn| {
        turn.release_orphan_snapshot_pins(f.device, &[kept_id])
    })
    .unwrap();
    assert!(path(gone_id).exists());
    let removed = run(&f, DeliveryOperation::Maintenance, |turn| {
        Ok(crate::https_owner::snapshot::remove_unpinned_sidecars(
            turn,
            &f.host_work(),
        ))
    })
    .unwrap();
    assert_eq!(removed, 1);
    assert!(!path(gone_id).exists());
    assert!(path(kept_id).exists());
}
