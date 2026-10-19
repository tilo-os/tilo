//! Signed relay input, admitted off the frame loop in the order it arrived.
//!
//! Admitting a native relay input commits it through the room's owner,
//! which publishes the state file: a data and a directory sync, seconds on a
//! loaded disk. The frame loop's tick ([`ShellNetwork::frame_tick`]) hands
//! the admission to a worker of its own and drains no later event until it
//! is back. Its loop-side remainder (driving the owner's publication,
//! handing an admitted app write to the broker) then runs before any later
//! event, so inbound events are handled in the order they arrived, one
//! admission at a time.
use super::ShellNetwork;
use crate::network::actor::{HostIdentity, native_ingress};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, mpsc};
use tilo_lock::MutexRecover as _;

#[derive(Default)]
pub(super) struct State {
    slot: Arc<Mutex<Slot>>,
    /// Signals the worker, started with the first admission, that the slot
    /// holds one to admit.
    worker: OnceLock<mpsc::Sender<()>>,
    /// Inputs held until the app surface they are decided against is here.
    held: Mutex<Held>,
}

/// Inputs neither admitted nor refused because the app surface they are
/// decided against was not here yet ([`tilo_peer_state::HELD_FOR_SURFACE`]):
/// a room replays its records to a member as it joins, often before the app
/// that opens them has started and named its surface. Dropping them lost
/// the records' history for good, and every later change to them then
/// failed in the app ("change's deps should already be in the document").
/// They are admitted again, in the order they arrived, once a surface is
/// named.
#[derive(Default)]
struct Held {
    waiting: std::collections::VecDeque<native_ingress::Input>,
    /// Taken from `waiting` to be admitted again, oldest first.
    again: std::collections::VecDeque<native_ingress::Input>,
    /// [`ui::app_surface::registered`] when `waiting` was last taken.
    seen: u64,
}

/// The most inputs held at once. A replay holds one per record change it
/// carries before the app starts; past this the oldest goes, and the room's
/// next checkpoint refresh delivers it again.
const MAX_HELD: usize = 4096;

impl State {
    /// Keep `input` until a surface is named.
    pub(super) fn hold(&self, input: native_ingress::Input) {
        let mut held = self.held.lock_recover();
        if held.waiting.len() >= MAX_HELD {
            held.waiting.pop_front();
            tracing::warn!("a held relay input was dropped: too many waiting for app surfaces");
        }
        held.waiting.push_back(input);
    }

    /// The next held input to admit again, once a surface was named since
    /// they were held.
    pub(super) fn next_again(&self) -> Option<native_ingress::Input> {
        let mut held = self.held.lock_recover();
        if held.again.is_empty() && !held.waiting.is_empty() {
            let named = ui::app_surface::registered();
            if named != held.seen {
                held.seen = named;
                let waiting = std::mem::take(&mut held.waiting);
                held.again = waiting;
            }
        }
        held.again.pop_front()
    }
}

#[derive(Default)]
enum Slot {
    #[default]
    Idle,
    /// Handed to the worker, which has not taken it yet.
    Pending(Box<(native_ingress::Input, HostIdentity)>),
    Admitting,
    Admitted(Box<Admitted>),
}

/// An admission, for the loop to complete.
pub(super) struct Admitted {
    pub(super) input: native_ingress::Input,
    /// Whether the admission committed a new owner generation.
    pub(super) changed: bool,
    pub(super) report: Result<tilo_peer_state::AdmissionReport, String>,
}

/// Where the admission handed off the loop stands.
pub(super) enum Lane {
    /// None is in flight.
    Idle,
    /// One is still being admitted: no later event may be handled yet.
    Busy,
    /// One finished: complete it before any later event.
    Finished(Box<Admitted>),
}

/// Admit `input` as this device, and whether the owner's generation moved,
/// read right after: what the loop-side remainder needs.
pub(super) fn admit(input: native_ingress::Input, identity: &HostIdentity) -> Admitted {
    let before = input.generation().ok();
    let report = input.admit(identity);
    let changed = before != input.generation().ok();
    Admitted {
        input,
        changed,
        report,
    }
}

fn locked(slot: &Mutex<Slot>) -> MutexGuard<'_, Slot> {
    // Nothing panics while holding it; a poisoned slot is still consistent.
    slot.lock_recover()
}

/// The admission waiting in `slot` for the worker, leaving `then` in its
/// place; `None`, and the slot as it was, when none is waiting.
fn take_pending(
    slot: &Mutex<Slot>,
    then: Slot,
) -> Option<Box<(native_ingress::Input, HostIdentity)>> {
    let mut slot = locked(slot);
    if !matches!(*slot, Slot::Pending(_)) {
        return None;
    }
    let pending = std::mem::replace(&mut *slot, then);
    drop(slot);
    match pending {
        Slot::Pending(pending) => Some(pending),
        Slot::Idle | Slot::Admitting | Slot::Admitted(_) => None,
    }
}

impl ShellNetwork {
    /// The admission handed off the loop, taken once it finished.
    pub(super) fn take_native_admission(&self) -> Lane {
        let mut slot = locked(&self.native_admission.slot);
        match std::mem::take(&mut *slot) {
            Slot::Idle => Lane::Idle,
            Slot::Admitted(admitted) => Lane::Finished(admitted),
            busy @ (Slot::Pending(_) | Slot::Admitting) => {
                *slot = busy;
                Lane::Busy
            }
        }
    }

    /// Admit `input` on the admission worker, which wakes the loop once it
    /// is admitted. Hands `input` back when the worker cannot take it; the
    /// caller then admits in place.
    pub(super) fn admit_off_loop(
        &self,
        input: native_ingress::Input,
        identity: HostIdentity,
    ) -> Option<native_ingress::Input> {
        let Some(worker) = self.admission_worker() else {
            return Some(input);
        };
        *locked(&self.native_admission.slot) = Slot::Pending(Box::new((input, identity)));
        if worker.send(()).is_ok() {
            return None;
        }
        take_pending(&self.native_admission.slot, Slot::Idle).map(|pending| pending.0)
    }

    fn admission_worker(&self) -> Option<&mpsc::Sender<()>> {
        if let Some(worker) = self.native_admission.worker.get() {
            return Some(worker);
        }
        let (signal, signals) = mpsc::channel::<()>();
        let slot = Arc::clone(&self.native_admission.slot);
        let wake = Arc::clone(&self.event_wake);
        let spawned = std::thread::Builder::new()
            .name("native-admission".into())
            .spawn(move || {
                // Ends with the network, which holds the only sender.
                while signals.recv().is_ok() {
                    let Some(pending) = take_pending(&slot, Slot::Admitting) else {
                        continue;
                    };
                    let (input, identity) = *pending;
                    let room = input.room();
                    // A panicking admission must not leave the loop waiting
                    // for it forever.
                    let admitted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        admit(input, &identity)
                    }))
                    .map_or_else(
                        |_| {
                            tracing::error!(%room, "native relay admission panicked");
                            Slot::Idle
                        },
                        |admitted| Slot::Admitted(Box::new(admitted)),
                    );
                    *locked(&slot) = admitted;
                    if let Some(wake) = wake.get() {
                        wake();
                    }
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "cannot start the native admission worker");
            return None;
        }
        // Only the frame loop admits off the loop, so nothing races this.
        let _ = self.native_admission.worker.set(signal);
        self.native_admission.worker.get()
    }
}
