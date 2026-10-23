//! Signed relay input, admitted off the frame loop in the order it arrived.
//!
//! Admitting a native relay input commits it through the room's owner,
//! which publishes the state file: a data and a directory sync, seconds on a
//! loaded disk. The frame loop's tick ([`ShellNetwork::frame_tick`]) hands
//! the admission to a worker of its own and drains no later event until it
//! is back. Its loop-side remainder (driving the owner's publication,
//! handing an admitted app write to the broker) then runs before any later
//! event, so inbound events are handled in the order they arrived, one
//! admission at a time. Inputs of one session queued back to back (a room's
//! replay) are one admission: each is authorized alone, in order, and the
//! state file is published once for them all.
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
    /// The event that ended the last batch of inputs drained: the next one
    /// to handle, ahead of the queue.
    carried: Mutex<Option<super::NetEvent>>,
    /// Runs on the worker before each data sync of an admission, for a test
    /// that holds the worker inside one. Read when the worker starts.
    #[cfg(test)]
    pub(super) on_worker_sync: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
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
    /// Rooms whose oldest held inputs were let go to keep the bound: once
    /// a surface is named their hosts replay what they retain, so nothing
    /// let go is lost.
    let_go: std::collections::BTreeSet<super::RoomId>,
    /// [`ui::app_surface::registered`] when `let_go` was last replayed.
    replayed_at: u64,
}

/// The most inputs held at once. A replay holds one per record change it
/// carries before the app starts; past this the oldest goes, and the room's
/// next checkpoint refresh delivers it again.
const MAX_HELD: usize = 4096;

impl State {
    /// Handle `event` next, ahead of the queue: it came after a batch.
    pub(super) fn carry(&self, event: super::NetEvent) {
        let previous = self.carried.lock_recover().replace(event);
        debug_assert!(previous.is_none(), "one event is carried at a time");
    }

    /// The event carried over from the last drain, if any.
    pub(super) fn carried(&self) -> Option<super::NetEvent> {
        self.carried.lock_recover().take()
    }

    pub(super) fn carries(&self) -> bool {
        self.carried.lock_recover().is_some()
    }

    /// Keep `input` until a surface is named.
    pub(super) fn hold(&self, input: native_ingress::Input) {
        let mut held = self.held.lock_recover();
        if held.waiting.len() >= MAX_HELD
            && let Some(oldest) = held.waiting.pop_front()
        {
            tracing::warn!(
                room = %oldest.room(),
                "a held relay input was let go: too many waiting for app surfaces; the room is replayed once one is named"
            );
            held.let_go.insert(oldest.room());
        }
        held.waiting.push_back(input);
    }

    /// The rooms to replay because inputs held for a surface were let go,
    /// once a surface was named since: what they let go comes back.
    pub(super) fn take_replays(&self) -> Vec<super::RoomId> {
        let mut held = self.held.lock_recover();
        let named = ui::app_surface::registered();
        if held.let_go.is_empty() || named == held.replayed_at {
            return Vec::new();
        }
        held.replayed_at = named;
        std::mem::take(&mut held.let_go).into_iter().collect()
    }

    /// The next held inputs to admit again, once a surface was named since
    /// they were held: the oldest, and those after it of the same session,
    /// up to [`MAX_BATCH`].
    #[allow(
        clippy::significant_drop_tightening,
        reason = "taking the batch and refilling `again` is one step under the lock"
    )]
    pub(super) fn next_again(&self) -> Vec<native_ingress::Input> {
        let mut held = self.held.lock_recover();
        if held.again.is_empty() && !held.waiting.is_empty() {
            let named = ui::app_surface::registered();
            if named != held.seen {
                held.seen = named;
                let waiting = std::mem::take(&mut held.waiting);
                held.again = waiting;
            }
        }
        let mut batch: Vec<native_ingress::Input> = Vec::new();
        while batch.len() < MAX_BATCH
            && let Some(next) = held.again.front()
            && batch.first().is_none_or(|first| first.same_session(next))
        {
            batch.extend(held.again.pop_front());
        }
        batch
    }
}

#[derive(Default)]
enum Slot {
    #[default]
    Idle,
    /// Handed to the worker, which has not taken it yet.
    Pending(Box<(Vec<native_ingress::Input>, HostIdentity)>),
    Admitting,
    Admitted(Box<Admitted>),
}

/// The most relay inputs of one session admitted in one commit. A room's
/// replay arrives as a burst of them; each is still authorized alone, and
/// the owner's state file is published once for all of them.
pub(super) const MAX_BATCH: usize = 256;

/// An admission of a batch, for the loop to complete.
pub(super) struct Admitted {
    /// Each input, in the order it came, with what its admission decided.
    pub(super) inputs: Vec<(
        native_ingress::Input,
        Result<tilo_peer_state::AdmissionReport, String>,
    )>,
    /// Whether the admission committed a new owner generation.
    pub(super) changed: bool,
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

/// Admit `inputs`, all of one relay session, as this device in one commit,
/// and whether the owner's generation moved, read right after: what the
/// loop-side remainder needs.
pub(super) fn admit(inputs: Vec<native_ingress::Input>, identity: &HostIdentity) -> Admitted {
    let Some(first) = inputs.first() else {
        return Admitted {
            inputs: Vec::new(),
            changed: false,
        };
    };
    let before = first.generation().ok();
    let reports = native_ingress::Input::admit_all(&inputs, identity);
    let changed = before != first.generation().ok();
    let inputs = match reports {
        Ok(reports) => inputs.into_iter().zip(reports).collect(),
        Err(error) => inputs
            .into_iter()
            .map(|input| (input, Err(error.clone())))
            .collect(),
    };
    Admitted { inputs, changed }
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
) -> Option<Box<(Vec<native_ingress::Input>, HostIdentity)>> {
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

    /// `first`, an app write, and the writes of its session queued right
    /// after it, up to [`MAX_BATCH`]: one admission. The event that ends the
    /// batch is carried over, to be handled next.
    pub(super) fn batch_from(
        &self,
        first: native_ingress::Input,
        events: &mut tokio::sync::mpsc::Receiver<super::NetEvent>,
    ) -> Vec<native_ingress::Input> {
        let mut batch = vec![first];
        while batch.len() < MAX_BATCH {
            let Ok(next) = events.try_recv() else {
                break;
            };
            match next {
                super::NetEvent::NativeInput(next)
                    if batch.first().is_some_and(|first| first.same_session(&next))
                        && self.admits_in_batch(&next) =>
                {
                    batch.push(next);
                }
                other => {
                    self.native_admission.carry(other);
                    break;
                }
            }
        }
        batch
    }

    /// Admit `inputs` on the admission worker, which wakes the loop once
    /// they are admitted. Hands them back when the worker cannot take them;
    /// the caller then admits in place.
    pub(super) fn admit_off_loop(
        &self,
        inputs: Vec<native_ingress::Input>,
        identity: HostIdentity,
    ) -> Option<Vec<native_ingress::Input>> {
        let Some(worker) = self.admission_worker() else {
            return Some(inputs);
        };
        *locked(&self.native_admission.slot) = Slot::Pending(Box::new((inputs, identity)));
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
        #[cfg(test)]
        let on_sync = self.native_admission.on_worker_sync.lock_recover().clone();
        let spawned = std::thread::Builder::new()
            .name("native-admission".into())
            .spawn(move || {
                // Ends with the network, which holds the only sender.
                while signals.recv().is_ok() {
                    let Some(pending) = take_pending(&slot, Slot::Admitting) else {
                        continue;
                    };
                    let (inputs, identity) = *pending;
                    let room = inputs.first().map(native_ingress::Input::room);
                    // A panicking admission must not leave the loop waiting
                    // for it forever.
                    let admitted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        #[cfg(test)]
                        if let Some(on_sync) = on_sync.clone() {
                            use tilo_files::native::atomic::test_support::{
                                Checkpoint, with_observer,
                            };
                            return with_observer(
                                move |event| {
                                    if event.checkpoint == Checkpoint::BeforeDataFlush {
                                        on_sync();
                                    }
                                },
                                || admit(inputs, &identity),
                            );
                        }
                        admit(inputs, &identity)
                    }))
                    .map_or_else(
                        |_| {
                            tracing::error!(?room, "native relay admission panicked");
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
