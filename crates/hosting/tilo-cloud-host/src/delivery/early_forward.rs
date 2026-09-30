//! Live forwarding on the Cloud host: forward a verified state item before its
//! durable Store commit (`tilo_workspace_host::early_forward`, which this host shares).
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
pub(crate) use tilo_workspace_host::early_forward::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

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
            .activate_delivery_storage(tilo_delivery_protocol::clock::now_ns())
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
        let mut message: tilo_peer_state::NeighbourMsg = postcard::from_bytes(&canonical).unwrap();
        if let tilo_peer_state::NeighbourMsg::AppDocument { cluster_id, .. } = &mut message {
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
        let message: tilo_peer_state::NeighbourMsg = postcard::from_bytes(&canonical).unwrap();
        let mut store = crate::Store::open(dir.path(), "agent-admission").unwrap();
        let (sink, seen) = recorder(dir.path());
        let before = store.state.generation;
        let failed = crate::storage::test_support::with_save_failure(
            dir.path(),
            tilo_files::native::atomic::test_support::Checkpoint::BeforePublish,
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
        let now = tilo_delivery_protocol::clock::now_ns();
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
                .peer_at(tilo_delivery_protocol::clock::now_ns())
                .unwrap()
                .covers(&message)
        );
    }
}
