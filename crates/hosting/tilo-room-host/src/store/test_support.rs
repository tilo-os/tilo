//! The store entry points tests outside this crate drive it through: the
//! shell replay adapter's selection tests (`tilo-relay-shell`) retain, stage,
//! capture and replay through the production store exactly as the relay's
//! own owner does. Built only for tests and the `test-support` feature.
use super::{
    WorkspaceStore,
    bounded::{Step, WorkLimit},
};
use crate::transport::Envelope;

pub use super::replay::{PublicationCursor, ReplayBudget, ReplayId, ReplayLimits, ReplaySnapshot};
pub use super::retained::{CheckpointStage, RetainBuild};
pub use crate::persistence::output_budget;

fn finish<T>(mut step: impl FnMut() -> Step<T>) -> T {
    loop {
        if let Step::Done(value) = step() {
            return value;
        }
    }
}

/// Prepare `snapshot` and read every frame it replays.
pub fn drain_replay(
    store: &WorkspaceStore,
    snapshot: &mut ReplaySnapshot,
    budget: &ReplayBudget,
) -> Vec<Envelope> {
    while matches!(
        store
            .prepare_replay_step(snapshot, WorkLimit::default())
            .unwrap(),
        Step::More
    ) {}
    let mut result = Vec::new();
    loop {
        let mut batch = store
            .read_replay_batch(snapshot, ReplayLimits::default(), budget)
            .unwrap();
        while let Some(frame) = batch.front() {
            result.push(frame.clone());
            batch.pop_written();
        }
        if batch.eof {
            return result;
        }
    }
}

/// Capture a replay of `store` for a fresh device, `limit` per quantum.
pub fn capture(store: &WorkspaceStore, limit: WorkLimit) -> ReplaySnapshot {
    let mut build = store
        .start_replay_capture(ReplayId(rand::random()), [2; 32])
        .unwrap();
    finish(|| store.capture_replay_step(&mut build, limit).unwrap())
}

/// Every frame a fresh device's replay of `store` delivers, in order.
pub fn replay_frames(store: &WorkspaceStore) -> Vec<Envelope> {
    let mut snapshot = capture(
        store,
        WorkLimit {
            entries: 1,
            ..Default::default()
        },
    );
    let result = drain_replay(store, &mut snapshot, &ReplayBudget::new(256 * 1024 * 1024));
    let mut discard = snapshot.into_discard().unwrap();
    while matches!(discard.step(WorkLimit::default()).unwrap(), Step::More) {}
    result
}

/// `bytes` (a retained frame) with unknown trailing bytes on its outer frame
/// (`outer`) or on its co-edit payload.
pub fn wrapper_suffix(bytes: &[u8], outer: bool) -> Vec<u8> {
    let mut bytes = bytes.to_vec();
    if outer {
        bytes.extend_from_slice(b"fixture extension");
        let length = u32::try_from(bytes.len().checked_sub(4).unwrap()).unwrap();
        bytes[..4].copy_from_slice(&length.to_le_bytes());
    } else {
        let mut envelope = crate::transport::decode(&bytes[4..]).unwrap();
        let Envelope::Delta { delta, .. } = &mut envelope else {
            panic!()
        };
        delta.extend_from_slice(b"fixture extension");
        bytes = crate::transport::encode(&envelope).unwrap();
    }
    bytes
}
