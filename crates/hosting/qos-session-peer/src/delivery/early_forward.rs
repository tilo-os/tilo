//! LIVE_V1 lane C: forward a verified state item before its durable commit.
//!
//! The installed data routes run each admission synchronously on the owner
//! thread inside [`scoped`]. Native admission [`arm`]s an item only after
//! `StatePeer::receive_from_report` accepted it as new (signature, author
//! certificate, role, capability and canonical form all checked) and the
//! candidate aggregate was built; `commit_delivery` then [`fire`]s the armed
//! item after every remaining non-durability check (aggregate validation,
//! replay validation, byte retention, encoding, policy fence) and immediately
//! before the store write and barrier. The sink only enqueues (the live
//! forwarder publishes on another task), so publication runs in parallel with
//! the barrier. An item that fails any check never reaches [`fire`]; an item
//! whose barrier then fails was forwarded but is not durable, and receivers
//! reconcile it against durable state (Cloud spec
//! `docs/superpowers/specs/2026-09-23-live-v1-design.md`, "Lane C", and the
//! browser overlay `qos-browser-runtime/src/peer_live/speculation.rs`).
use std::{cell::RefCell, sync::Arc};

/// `(author device, exact canonical item)`; must never block.
pub(crate) type EarlySink = Arc<dyn Fn([u8; 32], &[u8]) + Send + Sync>;

/// Largest canonical item worth arming: a forwarded frame must fit one
/// `OpsFrame` (`MAX_STREAM_FRAME_BYTES`) including record and digest, so
/// larger items are never copied here and reach members by durable replay.
pub(crate) const MAX_FORWARD_BYTES: usize =
    tilo_relay::delivery_v3::live_wire::MAX_STREAM_FRAME_BYTES;

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
}
