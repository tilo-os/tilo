//! LIVE_V1 lane C on the Cloud host: forward a verified state item before its
//! durable Store commit (parity with `qos-session-peer`'s `early_forward`).
//!
//! The Cloud data routes run each admission synchronously inside one held
//! Store turn on the calling thread, within [`scoped`]. `admit_delivery_item`
//! [`arm`]s an item only after `StatePeer::receive_from_report` accepted it as
//! new (signature, author certificate, role, capability, canonical form),
//! capacity was checked and its durable publications were built; the Store's
//! `save` then [`fire`]s it after `prepare_commit` validation and encoding,
//! immediately before `publish_bytes` (the atomic write and barriers). The
//! sink only enqueues; the live forwarder publishes on another task, in
//! parallel with the barrier. A failed check never reaches [`fire`]; a failed
//! barrier leaves a forwarded, non-durable item that receivers reconcile
//! against durable state (browser `peer_live` speculation overlay).
use std::{cell::RefCell, sync::Arc};

/// `(author device, exact canonical item)`; must never block.
pub(crate) type EarlySink = Arc<dyn Fn([u8; 32], &[u8]) + Send + Sync>;

/// Largest canonical item worth arming: a forwarded frame must fit one
/// `OpsFrame` (`MAX_STREAM_FRAME_BYTES`) including record and digest, so
/// larger items are never copied here and reach members by durable replay.
pub(crate) const MAX_FORWARD_BYTES: usize =
    qos_relay::delivery_v3::live_wire::MAX_STREAM_FRAME_BYTES;

thread_local! {
    static SINK: RefCell<Option<EarlySink>> = const { RefCell::new(None) };
    static ARMED: RefCell<Vec<([u8; 32], Vec<u8>)>> = const { RefCell::new(Vec::new()) };
}

struct Restore(Option<EarlySink>);
impl Drop for Restore {
    fn drop(&mut self) {
        SINK.with(|sink| *sink.borrow_mut() = self.0.take());
        ARMED.with(|armed| armed.borrow_mut().clear());
    }
}

/// Run one synchronous owner admission with `sink` installed for this thread.
pub(crate) fn scoped<T>(sink: Option<&EarlySink>, operation: impl FnOnce() -> T) -> T {
    let _restore = Restore(SINK.with(|slot| slot.replace(sink.cloned())));
    operation()
}

/// Clears an armed item when its commit attempt ends (success or failure).
pub(crate) struct Disarm(());
impl Drop for Disarm {
    fn drop(&mut self) {
        ARMED.with(|armed| armed.borrow_mut().clear());
    }
}

/// Arm a newly accepted item for the next [`fire`]. Without an installed sink,
/// or for an item too large to forward, nothing is copied.
pub(crate) fn arm(device: [u8; 32], canonical: &[u8]) -> Disarm {
    arm_group(device, &[canonical])
}

/// Arm every newly accepted item of one multi-item commit group, in request
/// order; the group's single [`fire`] forwards each of them before its barrier.
pub(crate) fn arm_group(device: [u8; 32], items: &[&[u8]]) -> Disarm {
    let installed = SINK.with(|sink| sink.borrow().is_some());
    if installed {
        ARMED.with(|armed| {
            let mut armed = armed.borrow_mut();
            armed.clear();
            armed.extend(
                items
                    .iter()
                    .filter(|canonical| {
                        !canonical.is_empty() && canonical.len() <= MAX_FORWARD_BYTES
                    })
                    .map(|canonical| (device, canonical.to_vec())),
            );
        });
    }
    Disarm(())
}

/// Called by the owner immediately before the durable store write: hand each
/// armed item (if any) to the sink exactly once, in order.
pub(crate) fn fire() {
    let armed = ARMED.with(|armed| std::mem::take(&mut *armed.borrow_mut()));
    if armed.is_empty() {
        return;
    }
    let sink = SINK.with(|sink| sink.borrow().clone());
    if let Some(sink) = sink {
        for (device, canonical) in armed {
            sink(device, &canonical);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn only_an_armed_item_inside_a_scope_fires_once_and_scopes_restore() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        let sink: EarlySink =
            Arc::new(move |device, bytes| record.lock().unwrap().push((device, bytes.to_vec())));
        // No scope: arming copies nothing and fire is inert.
        drop(arm([1; 32], b"a"));
        fire();
        scoped(Some(&sink), || {
            fire(); // nothing armed
            {
                let _disarm = arm([2; 32], b"b");
                fire();
                fire(); // exactly once
            }
            // A failed attempt (disarmed without fire) forwards nothing.
            drop(arm([3; 32], b"c"));
            fire();
            // Oversized items are never armed.
            drop(arm([4; 32], &vec![0; MAX_FORWARD_BYTES + 1]));
            fire();
            // Empty items are never armed.
            let _disarm = arm([5; 32], b"");
            fire();
        });
        drop(arm([6; 32], b"d"));
        fire();
        assert_eq!(*seen.lock().unwrap(), vec![([2; 32], b"b".to_vec())]);
    }

    #[test]
    fn a_group_forwards_each_verified_item_once_in_order() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = seen.clone();
        let sink: EarlySink =
            Arc::new(move |device, bytes| record.lock().unwrap().push((device, bytes.to_vec())));
        scoped(Some(&sink), || {
            {
                let big = vec![0; MAX_FORWARD_BYTES + 1];
                let _disarm = arm_group([7; 32], &[b"x", b"", &big, b"y"]);
                fire();
                fire();
            }
            // A failed group commit forwards nothing.
            drop(arm_group([8; 32], &[b"z"]));
            fire();
        });
        assert_eq!(
            *seen.lock().unwrap(),
            vec![([7; 32], b"x".to_vec()), ([7; 32], b"y".to_vec())]
        );
    }

    /// Cloud Store: an admitted item reaches the sink while `state.json` still
    /// holds the previous generation, i.e. before the atomic publication.
    fn disk_generation(root: &std::path::Path) -> u64 {
        let state: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("state.json")).unwrap()).unwrap();
        state["generation"].as_u64().unwrap()
    }
    type Seen = Arc<Mutex<Vec<([u8; 32], Vec<u8>, u64)>>>;
    fn recorder(root: &std::path::Path) -> (EarlySink, Seen) {
        let seen: Seen = Arc::default();
        let record = seen.clone();
        let root = root.to_owned();
        let sink: EarlySink = Arc::new(move |device, bytes| {
            record
                .lock()
                .unwrap()
                .push((device, bytes.to_vec(), disk_generation(&root)))
        });
        (sink, seen)
    }
    fn prepared() -> (tempfile::TempDir, [u8; 32]) {
        let (dir, mut store, device) = crate::agent_owner_tests::enrolled();
        store
            .activate_delivery_storage(qos_relay::clock::now_ns())
            .unwrap();
        (dir, device)
    }

    #[test]
    fn cloud_forwards_a_verified_item_before_the_store_barrier() {
        let (dir, device) = prepared();
        let canonical =
            crate::delivery::admission::tests::browser_delta(dir.path(), "agent-admission", "live");
        let mut store = crate::Store::open(dir.path(), "agent-admission").unwrap();
        let (sink, seen) = recorder(dir.path());
        let before = store.state.generation;
        assert_eq!(disk_generation(dir.path()), before);
        let outcome = scoped(Some(&sink), || {
            store.admit_delivery_item(device, [7; 16], &canonical, 1024 * 1024)
        });
        assert!(matches!(
            outcome,
            Ok(super::super::admission::StateOutcome::Durable(_))
        ));
        assert_eq!(store.state.generation, before + 1);
        assert_eq!(disk_generation(dir.path()), before + 1);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!((seen[0].0, &seen[0].1), (device, &canonical));
        assert_eq!(
            seen[0].2, before,
            "forwarded before the durable publication"
        );
        drop(seen);
        // The same item again (lost reply) is a receipt, never forwarded again.
        let (sink, seen) = recorder(dir.path());
        scoped(Some(&sink), || {
            store.admit_delivery_item(device, [7; 16], &canonical, 1024 * 1024)
        })
        .unwrap();
        assert!(seen.lock().unwrap().is_empty());
    }

    #[test]
    fn cloud_never_forwards_items_that_fail_admission() {
        let (dir, device) = prepared();
        let canonical =
            crate::delivery::admission::tests::browser_delta(dir.path(), "agent-admission", "x");
        let mut message: qos_peer_state::NeighbourMsg = postcard::from_bytes(&canonical).unwrap();
        if let qos_peer_state::NeighbourMsg::AppDocument { cluster_id, .. } = &mut message {
            *cluster_id = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into();
        }
        let forged = postcard::to_stdvec(&message).unwrap();
        let mut store = crate::Store::open(dir.path(), "agent-admission").unwrap();
        let (sink, seen) = recorder(dir.path());
        let generation = store.state.generation;
        scoped(Some(&sink), || {
            assert!(
                store
                    .admit_delivery_item(device, [8; 16], &forged, 1024 * 1024)
                    .is_err()
            );
            assert!(
                store
                    .admit_delivery_item(device, [9; 16], b"not canonical", 1024 * 1024)
                    .is_err()
            );
            // Another device cannot submit this author's item as its own.
            let _ = store.admit_delivery_item([3; 32], [10; 16], &canonical, 1024 * 1024);
        });
        assert!(
            seen.lock().unwrap().is_empty(),
            "no refused item is forwarded"
        );
        assert_eq!(store.state.generation, generation);
    }

    #[test]
    fn cloud_forwarded_item_whose_barrier_fails_is_not_durable_until_retried() {
        let (dir, device) = prepared();
        let canonical = crate::delivery::admission::tests::browser_delta(
            dir.path(),
            "agent-admission",
            "fails",
        );
        let message: qos_peer_state::NeighbourMsg = postcard::from_bytes(&canonical).unwrap();
        let mut store = crate::Store::open(dir.path(), "agent-admission").unwrap();
        let (sink, seen) = recorder(dir.path());
        let before = store.state.generation;
        let failed = crate::storage::test_support::with_save_failure(
            dir.path(),
            atomic_file::test_support::Checkpoint::BeforePublish,
            || {
                scoped(Some(&sink), || {
                    store.admit_delivery_item(device, [7; 16], &canonical, 1024 * 1024)
                })
            },
        );
        assert!(failed.is_err());
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "forwarded before the failed barrier"
        );
        assert_eq!(disk_generation(dir.path()), before, "nothing durable");
        drop(store);
        // Reopen: durable state does not cover the forwarded change.
        let mut store = crate::Store::open(dir.path(), "agent-admission").unwrap();
        let now = qos_relay::clock::now_ns();
        assert!(!store.peer_at(now).unwrap().covers(&message));
        // The author's retry commits the same item; durable state now covers it.
        let (sink, seen) = recorder(dir.path());
        let outcome = scoped(Some(&sink), || {
            store.admit_delivery_item(device, [7; 16], &canonical, 1024 * 1024)
        });
        assert!(matches!(
            outcome,
            Ok(super::super::admission::StateOutcome::Durable(_))
        ));
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(
            store
                .peer_at(qos_relay::clock::now_ns())
                .unwrap()
                .covers(&message)
        );
    }
}
