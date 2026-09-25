use super::super::super::tests as fixtures;
use super::*;
use crate::committed_authority::CommittedAuthority;
struct Fixture {
    _temp: tempfile::TempDir,
    owner: SessionOwner,
    current: CommittedAuthority,
    session: auth::VerifiedSession,
    pool: HostBytes,
    _ingress: crate::https::Ingress,
}
impl Fixture {
    fn new() -> Self {
        let (temp, mut owner) = fixtures::hosted_owner();
        let (ingress, _rx) = crate::https::ingress_channel();
        let pool = ingress.host_bytes();
        owner.attach_data_resources(&pool).unwrap();
        let current = CommittedAuthority::new(&owner);
        let session = fixtures::authenticated_session(&owner, &current);
        let mut turn = owner
            .prepare_data_turn(&pool, OwnerDataOperation::RegisterConsumer)
            .unwrap();
        current
            .with_owner_view(&mut owner, |view, owner| {
                let source = owner.data_source(&mut turn, view, &session, fixtures::now_ns())?;
                owner.data_register_consumer(&mut turn, &source)
            })
            .unwrap()
            .unwrap();
        Self {
            _temp: temp,
            owner,
            current,
            session,
            pool,
            _ingress: ingress,
        }
    }
    fn select(&mut self, op: SnapshotOperation) -> (SnapshotSelection, OwnedPublicationPin) {
        let mut turn = self
            .owner
            .prepare_data_turn(&self.pool, OwnerDataOperation::SnapshotPrepare)
            .unwrap();
        self.current
            .with_owner_view(&mut self.owner, |view, owner| {
                let source =
                    owner.data_source(&mut turn, view, &self.session, fixtures::now_ns())?;
                owner.data_select_snapshot(&mut turn, &source, &op, &self.pool)
            })
            .unwrap()
            .unwrap()
    }
    fn revalidate(
        &self,
        selection: &SnapshotSelection,
        pin: &OwnedPublicationPin,
    ) -> DataResult<()> {
        let mut turn = self
            .owner
            .prepare_data_turn(&self.pool, OwnerDataOperation::SnapshotPrepare)?;
        let current = self.current.read().unwrap();
        let source =
            self.owner
                .data_source(&mut turn, current.view(), &self.session, fixtures::now_ns())?;
        self.owner.data_revalidate_snapshot(
            &mut turn,
            &source,
            selection,
            pin.custody(),
            &self.pool,
        )
    }
    fn edit(&mut self, name: &str) -> CommitRef {
        let mut author = self.owner.peer.clone();
        let bytes = fixtures::upload(&self.owner, &mut author, name);
        let mut turn = self
            .owner
            .prepare_data_turn(
                &self.pool,
                OwnerDataOperation::State {
                    input_len: bytes.len(),
                },
            )
            .unwrap();
        let result = self
            .current
            .with_owner_view(&mut self.owner, |view, owner| {
                let source =
                    owner.data_source(&mut turn, view, &self.session, fixtures::now_ns())?;
                owner.data_admit(
                    &mut turn,
                    &source,
                    CompleteStateItem {
                        delivery_id: [name.as_bytes()[0]; 16],
                        canonical: &bytes,
                    },
                    SystemTime::now(),
                )
            })
            .unwrap()
            .unwrap();
        match result {
            AdmissionOutcome::Durable(record) => record.committed,
            _ => panic!("actual native admission required"),
        }
    }
}
#[test]
fn snapshot_selection_actual_baseline_has_indexed_cut_and_all_original_backings() {
    let mut f = Fixture::new();
    let (selection, pin) = f.select(SnapshotOperation::Baseline);
    assert_eq!(selection.kind(), SnapshotKind::Baseline);
    assert!(selection.predecessor().is_none());
    assert_ne!(selection.pin(), [0; 16]);
    let (sequence, entry) = f
        .owner
        .delivery_ledger()
        .unwrap()
        .replay
        .last_key_value()
        .unwrap();
    assert_eq!(selection.scope().committed, reference(*sequence, entry));
    assert!(
        selection.scope().committed.sequence < f.owner.committed_generation(),
        "consumer metadata generation is not snapshot watermark"
    );
    let outgoing = f.owner.outgoing();
    assert_eq!(outgoing.len(), pin.len());
    for (i, p) in outgoing.iter().enumerate() {
        assert_eq!(p.bytes.as_ptr(), pin.get(i).unwrap().bytes.as_ptr());
    }
    f.revalidate(&selection, &pin).unwrap();
}
#[test]
fn snapshot_selection_concurrent_suffix_preserves_original_cut_and_exact_replay_after() {
    let mut f = Fixture::new();
    let (selection, pin) = f.select(SnapshotOperation::Baseline);
    let original = selection.scope().committed.clone();
    let cursor = {
        let mut turn = f
            .owner
            .prepare_data_turn(&f.pool, OwnerDataOperation::Replay)
            .unwrap();
        let current = f.current.read().unwrap();
        let source = f
            .owner
            .data_source(&mut turn, current.view(), &f.session, fixtures::now_ns())
            .unwrap();
        f.owner
            .data_issue_cursor(&mut turn, &source, original.clone())
            .unwrap()
    };
    let after = f.edit("suffix");
    assert!(after.sequence > original.sequence);
    f.revalidate(&selection, &pin).unwrap();
    let (replay, roots) = f.select(SnapshotOperation::ReplayAfter { cursor });
    assert_eq!(replay.scope().committed, after);
    assert_eq!(replay.predecessor(), Some(&original));
    assert_eq!(
        roots.len(),
        f.owner.delivery_ledger().unwrap().replay[&after.sequence]
            .publications
            .len()
    );
    f.revalidate(&replay, &roots).unwrap();
    assert_eq!(selection.scope().committed, original);
}
#[test]
fn snapshot_selection_rejects_pairing_other_original_roots_and_replaced_baseline() {
    let mut f = Fixture::new();
    let (selection, pin) = f.select(SnapshotOperation::Baseline);
    f.edit("changed");
    let (_, new_pin) = f.select(SnapshotOperation::Baseline);
    assert!(f.revalidate(&selection, &new_pin).is_err());
    let mut scope = f
        .owner
        .prepare_owner_scope(&f.pool, crate::owner::resources::OwnerOperation::Checkpoint)
        .unwrap();
    f.owner
        .with_owner_scope(&mut scope, &f.current, |owner| owner.refresh_checkpoint())
        .unwrap()
        .unwrap();
    assert!(
        f.revalidate(&selection, &pin).is_err(),
        "changed actual baseline cannot be silently reconstructed from payload availability"
    );
}

#[test]
fn snapshot_selection_cannot_cross_reopened_owner_resource_identity() {
    let mut f = Fixture::new();
    let (selection, pin) = f.select(SnapshotOperation::Baseline);
    let root = f.owner.root().to_owned();
    drop(f.current);
    drop(f.owner);
    let mut reopened = SessionOwner::open(&root).unwrap();
    let (other_ingress, _rx) = crate::https::ingress_channel();
    let other_pool = other_ingress.host_bytes();
    reopened.attach_data_resources(&other_pool).unwrap();
    let current = CommittedAuthority::new(&reopened);
    let view = current.read().unwrap();
    let mut turn = reopened
        .prepare_data_turn(&other_pool, OwnerDataOperation::SnapshotPrepare)
        .unwrap();
    let source = reopened
        .data_source(&mut turn, view.view(), &f.session, fixtures::now_ns())
        .unwrap();
    assert!(
        reopened
            .data_revalidate_snapshot(&mut turn, &source, &selection, pin.custody(), &f.pool)
            .is_err(),
        "identical persisted scope is not original owner resource custody"
    );
}

#[test]
fn snapshot_selection_replay_generation_survives_newer_current_checkpoint() {
    let mut f = Fixture::new();
    let (baseline, _) = f.select(SnapshotOperation::Baseline);
    let cursor = {
        let mut turn = f
            .owner
            .prepare_data_turn(&f.pool, OwnerDataOperation::Replay)
            .unwrap();
        let current = f.current.read().unwrap();
        let source = f
            .owner
            .data_source(&mut turn, current.view(), &f.session, fixtures::now_ns())
            .unwrap();
        f.owner
            .data_issue_cursor(&mut turn, &source, baseline.scope().committed.clone())
            .unwrap()
    };
    let selected = f.edit("replay-before-checkpoint");
    let (selection, pin) = f.select(SnapshotOperation::ReplayAfter { cursor });
    let mut scope = f
        .owner
        .prepare_owner_scope(&f.pool, crate::owner::resources::OwnerOperation::Checkpoint)
        .unwrap();
    f.owner
        .with_owner_scope(&mut scope, &f.current, |owner| owner.refresh_checkpoint())
        .unwrap()
        .unwrap();
    assert!(f.owner.publication_baseline().0 > selection.baseline_generation());
    assert_eq!(selection.baseline_generation(), selected.sequence);
    f.revalidate(&selection, &pin).unwrap();
}
#[test]
fn snapshot_selection_retention_between_capture_and_revalidation_is_history_unavailable() {
    let mut f = Fixture::new();
    let (baseline, _) = f.select(SnapshotOperation::Baseline);
    let cursor = {
        let mut turn = f
            .owner
            .prepare_data_turn(&f.pool, OwnerDataOperation::Replay)
            .unwrap();
        let current = f.current.read().unwrap();
        let source = f
            .owner
            .data_source(&mut turn, current.view(), &f.session, fixtures::now_ns())
            .unwrap();
        f.owner
            .data_issue_cursor(&mut turn, &source, baseline.scope().committed.clone())
            .unwrap()
    };
    let mut scope = f
        .owner
        .prepare_owner_scope(&f.pool, crate::owner::resources::OwnerOperation::Checkpoint)
        .unwrap();
    f.owner
        .with_owner_scope(&mut scope, &f.current, |owner| owner.refresh_checkpoint())
        .unwrap()
        .unwrap();
    drop(scope);
    let (selection, pin) = f.select(SnapshotOperation::ReplayAfter { cursor });
    let removed = selection.scope().committed.sequence;
    // A real metadata transaction models retention after the temporary byte pin,
    // before a durable SnapshotPin exists. No admission receipt refers to this
    // checkpoint-only entry; retained bytes by themselves are not an indexed cut.
    let mut turn = f
        .owner
        .prepare_data_turn(&f.pool, OwnerDataOperation::SnapshotRelease)
        .unwrap();
    f.current
        .with_owner_view(&mut f.owner, |view, owner| {
            let source = owner.data_source(&mut turn, view, &f.session, fixtures::now_ns())?;
            owner.with_data_operation(&mut turn, OwnerDataOperation::SnapshotRelease, |owner| {
                owner.matches_delivery_view(source.view)?;
                let mut next = owner.aggregate.clone();
                let PublicationStorage::V5 { delivery, .. } = &mut next.publication else {
                    unreachable!()
                };
                assert!(
                    delivery
                        .admissions
                        .values()
                        .all(|record| record.committed.sequence != removed)
                );
                assert!(delivery.replay.remove(&removed).is_some());
                owner.commit(next, owner.peer.clone())
            })
        })
        .unwrap()
        .unwrap();
    drop(turn);
    let error = f.revalidate(&selection, &pin).unwrap_err();
    assert_eq!(
        error.kind(),
        DataFailure::Reset(ResetReason::HistoryUnavailable)
    );
}
