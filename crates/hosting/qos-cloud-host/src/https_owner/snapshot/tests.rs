use super::super::{
    Current, DeliveryOperation,
    data::tests::{Fixture, authenticate_with_capabilities, fixture, register},
};
use super::*;
use qos_relay::delivery_v3::cursor::CursorToken;

pub(super) async fn snapshot_fixture() -> Fixture {
    let mut f = fixture().await;
    let origin = f.session.binding().origin.clone();
    f.session = match f
        .source
        .with_current(|turn| authenticate_with_capabilities(turn, &f.host, &origin, 9))
    {
        Current::Current(session) => session,
        _ => panic!("actual current snapshot handshake required"),
    };
    f
}
pub(super) fn run<R>(
    f: &Fixture,
    operation: DeliveryOperation,
    action: impl FnOnce(&mut CloudStoreTurn) -> Result<R>,
) -> Result<R> {
    match f
        .source
        .with_delivery_turn_unqualified_test(&f.pool, operation, action)
    {
        Current::Current(value) => value,
        Current::Busy => panic!("real Store fixture unexpectedly busy"),
        Current::Denied => panic!("real Store fixture unexpectedly denied"),
    }
}
pub(super) fn baseline(f: &Fixture) -> PreparedSnapshot {
    run(f, DeliveryOperation::SnapshotSelect, |turn| {
        select(&f.source, turn, &f.session, &SnapshotOperation::Baseline)
    })
    .unwrap()
    .prepare()
    .unwrap()
}
fn cursor(f: &Fixture, cut: CommitRef) -> CursorToken {
    run(f, DeliveryOperation::Cursor, |turn| {
        turn.issue_delivery_cursor(&f.session, cut)
    })
    .unwrap()
}
pub(super) fn edit(f: &Fixture, id: &str) {
    let mut store = crate::Store::open(f.root.path(), "agent-admission").unwrap();
    store
        .action(
            f.device,
            &crate::ActionRequest {
                version: 1,
                room: store.state.room.clone(),
                invocation_id: id.into(),
                operation: crate::NativeOperation::WriterInsert {
                    text: "actual snapshot suffix".into(),
                },
            },
            qos_relay::clock::now_ns(),
        )
        .unwrap();
}
#[tokio::test]
async fn cloud_snapshot_selects_actual_indexed_baseline_without_durable_pin_or_ack() {
    let f = snapshot_fixture().await;
    let consumer = register(&f);
    let selected = run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        let selected = select(&f.source, turn, &f.session, &SnapshotOperation::Baseline)?;
        let roots = turn.snapshot_roots()?;
        for (i, reference) in roots.baseline.iter().chain(roots.tail).enumerate() {
            assert_eq!(
                selected.custody.get(i).unwrap().bytes.as_ptr(),
                turn.publication_pool()
                    .resolve(&reference.payload)
                    .unwrap()
                    .as_ptr()
            );
        }
        Ok(selected)
    })
    .unwrap();
    let prepared = selected.prepare().unwrap();
    run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        prepared.revalidate(turn, &f.session)?;
        let ledger = turn.delivery_ledger();
        assert_eq!(prepared.selection.scope.consumer, consumer);
        assert!(
            ledger.pins.is_empty(),
            "temporary byte custody cannot create a durable pin"
        );
        assert!(ledger.consumers[&f.device].acknowledged.is_none());
        let (sequence, entry) = ledger.replay.last_key_value().unwrap();
        assert_eq!(
            prepared.selection.scope.committed,
            CommitRef {
                sequence: *sequence,
                digest: entry.digest
            }
        );
        assert!(
            turn.generation() > *sequence,
            "consumer metadata is not a new native watermark"
        );
        let roots = turn.snapshot_roots()?;
        assert_eq!(
            prepared.selection.baseline_generation,
            roots.baseline_generation
        );
        for (i, reference) in roots.baseline.iter().chain(roots.tail).enumerate() {
            assert_eq!(prepared.stream.custody().reference(i), Some(reference));
            assert_eq!(
                prepared.stream.custody().get(i).unwrap().bytes,
                turn.publication_pool().resolve(&reference.payload).unwrap()
            );
        }
        Ok(())
    })
    .unwrap();
}
#[tokio::test]
async fn cloud_snapshot_concurrent_suffix_preserves_cut_and_selects_exact_next_replay() {
    let f = snapshot_fixture().await;
    register(&f);
    let initial = baseline(&f);
    let cut = initial.selection.scope.committed.clone();
    let cursor = cursor(&f, cut.clone());
    edit(&f, "snapshot-suffix");
    run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        initial.revalidate(turn, &f.session)
    })
    .unwrap();
    let replay = run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        select(
            &f.source,
            turn,
            &f.session,
            &SnapshotOperation::ReplayAfter { cursor },
        )
    })
    .unwrap()
    .prepare()
    .unwrap();
    assert_eq!(replay.selection.predecessor, Some(cut));
    run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        replay.revalidate(turn, &f.session)?;
        let entry = &turn.delivery_ledger().replay[&replay.selection.scope.committed.sequence];
        assert_eq!(replay.selection.baseline_generation, entry.owner_generation);
        assert_eq!(replay.stream.item_count(), entry.publications.len());
        Ok(())
    })
    .unwrap();
}
#[tokio::test]
async fn cloud_snapshot_private_selection_cannot_cross_equal_rebound_source() {
    let f = snapshot_fixture().await;
    register(&f);
    let selected = baseline(&f);
    let store = crate::Store::open(f.root.path(), "agent-admission").unwrap();
    let rebound = CloudStoreSource::bind(&store).unwrap();
    drop(store);
    match rebound.with_delivery_turn_unqualified_test(
        &f.pool,
        DeliveryOperation::SnapshotSelect,
        |turn| selected.revalidate(turn, &f.session),
    ) {
        Current::Current(Err(CloudDeliveryError::Denied)) => (),
        _ => panic!("persisted equality cannot replace original source lifetime"),
    }
}

#[tokio::test]
async fn cloud_snapshot_state_only_session_cannot_authorize_snapshot_route() {
    let f = fixture().await;
    let result = run(&f, DeliveryOperation::SnapshotSelect, |turn| {
        select(&f.source, turn, &f.session, &SnapshotOperation::Baseline)
    });
    assert!(matches!(result, Err(CloudDeliveryError::Denied)));
}
