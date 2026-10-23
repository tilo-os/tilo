//! Optional exact-component wake intake. This module does not schedule, service,
//! freeze, or skip any component. A wake reason is not proof of model purity.
use crate::application::AppComponent;
use crate::bound_dispatch::{BoundDispatchError, CallbackBinding};
use crate::wake::{WakeEvent, WakeHandle};
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::{Rc, Weak as LocalWeak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use tilo_lock::MutexRecover as _;
use uuid::Uuid;

/// Non-authoritative progress from an actual snapshot service. Errors retain
/// failed/unvisited work; completed owners have returned all ordinary doors.
#[derive(Debug)]
pub struct OwnerModelServiceReport {
    pub attempted: bool,
    pub completed: usize,
    pub result: Result<(), BoundDispatchError>,
}

struct Token {
    id: Uuid,
    open: AtomicBool,
    lifetime: WakeHandle,
}
impl Token {
    fn accepts(&self, event: &WakeEvent) -> bool {
        self.open.load(Ordering::Acquire) && self.lifetime.accepts(event)
    }
}
struct Packet {
    token: Weak<Token>,
    event: WakeEvent,
    requires_sync: bool,
}
#[derive(Default)]
struct PendingState {
    entries: HashMap<Uuid, Packet>,
    order: Vec<Uuid>,
}
/// The queue, and what a parked host waits on for it to fill.
#[derive(Default)]
struct Pending(Mutex<PendingState>, Condvar);
impl Pending {
    fn push(&self, token: &Arc<Token>, event: WakeEvent, requires_sync: bool) -> bool {
        let mut pending = self.0.lock_recover();
        // Close may race a Send sink. Recheck while holding only the queue loan.
        if !token.accepts(&event) {
            return false;
        }
        let weak = Arc::downgrade(token);
        match pending.entries.entry(token.id) {
            std::collections::hash_map::Entry::Occupied(mut slot) => {
                if !Weak::ptr_eq(&slot.get().token, &weak) {
                    return false;
                }
                slot.get_mut().requires_sync |= requires_sync;
            }
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(Packet {
                    token: weak,
                    event,
                    requires_sync,
                });
                pending.order.push(token.id);
            }
        }
        drop(pending);
        self.1.notify_all();
        true
    }
}
struct Entry {
    token: Weak<Token>,
    binding: CallbackBinding,
}
type Index = RefCell<HashMap<Uuid, Entry>>;

/// UI-affine registration index with a Send-safe, weak pending intake.
/// Hosts register actual mounted components. Queue presence is only scheduling
/// metadata; each service re-admits the current physical owner and canonical record.
#[derive(Clone, Default)]
pub struct OwnerWakeQueue {
    pending: Arc<Pending>,
    index: Rc<Index>,
}

/// An opaque index locator. Equality requires the exact registration allocation
/// as well as the UUID bucket. Neither the locator nor a wake grants authority.
#[derive(Clone)]
pub struct OwnerWakeKey {
    id: Uuid,
    token: Weak<Token>,
}
impl PartialEq for OwnerWakeKey {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && Weak::ptr_eq(&self.token, &other.token)
    }
}
impl Eq for OwnerWakeKey {}
impl Hash for OwnerWakeKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

/// Weak Send sink for the component's ORIGINAL mount callback. It owns no root,
/// registry, document, store, callback binding, or component lifetime.
#[derive(Clone)]
pub struct OwnerWakeSink {
    token: Weak<Token>,
    pending: Weak<Pending>,
}
impl OwnerWakeSink {
    /// Queue a signal only if its original scope and registration remain open.
    /// `true` means queued; it is not a checked document or callback admission.
    pub fn enqueue(&self, event: WakeEvent) -> bool {
        let _profile = tilo_resource::profiling::Phase::start(if event.requires_sync() {
            "ui.owner_wake.enqueue_sync"
        } else {
            "ui.owner_wake.enqueue_events"
        });
        let (Some(token), Some(pending)) = (self.token.upgrade(), self.pending.upgrade()) else {
            return false;
        };
        let requires_sync = event.requires_sync();
        pending.push(&token, event, requires_sync)
    }
}

/// One taken signal with the original checked binding retained privately.
/// This type deliberately exposes no root, registry, document, or callback door.
pub struct OwnerWakeRequest {
    key: OwnerWakeKey,
    event: WakeEvent,
    requires_sync: bool,
    binding: CallbackBinding,
}
impl OwnerWakeRequest {
    pub(crate) fn binding(&self) -> &CallbackBinding {
        &self.binding
    }
    pub fn key(&self) -> &OwnerWakeKey {
        &self.key
    }
    /// Aggregate OR, independent of the representative event's original reason.
    pub const fn requires_sync(&self) -> bool {
        self.requires_sync
    }
    /// Check current exact component/root/registry custody afresh. This is not a
    /// document loan, and cannot authorize any later opaque model callback.
    /// # Errors
    /// Refuses a retired registration or a different/missing component owner.
    pub fn check_current(&self, component: &mut AppComponent) -> Result<(), BoundDispatchError> {
        let token = self
            .key
            .token
            .upgrade()
            .ok_or(BoundDispatchError::ClosedLifetime)?;
        if !token.accepts(&self.event) {
            return Err(BoundDispatchError::ClosedLifetime);
        }
        component.with_live_component(&self.binding, |_, _, _| ())
    }
}

/// Registration ownership belongs to the actual AppComponent, not its label,
/// checkout, container, or a retained root address. Close precedes callbacks.
pub(crate) struct OwnerWakeRegistration {
    token: Arc<Token>,
    pending: Weak<Pending>,
    index: LocalWeak<Index>,
}
impl Drop for OwnerWakeRegistration {
    fn drop(&mut self) {
        self.token.open.store(false, Ordering::Release);
        if let Some(index) = self.index.upgrade() {
            let mut entries = index.borrow_mut();
            if entries
                .get(&self.token.id)
                .is_some_and(|entry| Weak::ptr_eq(&entry.token, &Arc::downgrade(&self.token)))
            {
                entries.remove(&self.token.id);
            }
        }
        if let Some(pending) = self.pending.upgrade() {
            let mut entries = pending.0.lock_recover();
            if entries
                .entries
                .get(&self.token.id)
                .is_some_and(|entry| Weak::ptr_eq(&entry.token, &Arc::downgrade(&self.token)))
            {
                entries.entries.remove(&self.token.id);
                entries.order.retain(|id| *id != self.token.id);
            }
        }
    }
}
impl OwnerWakeQueue {
    pub(crate) fn register(
        &self,
        binding: CallbackBinding,
    ) -> Result<(OwnerWakeRegistration, OwnerWakeSink), BoundDispatchError> {
        let _entry = binding.enter()?;
        let token = Arc::new(Token {
            id: binding.lifetime_id,
            open: AtomicBool::new(true),
            lifetime: binding.lifetime.clone(),
        });
        let mut index = self.index.borrow_mut();
        // Reject instead of overwriting even a same-UUID entry. Allocation and
        // original binding jointly identify this registration.
        if index.contains_key(&token.id) {
            return Err(BoundDispatchError::ClosedLifetime);
        }
        index.insert(
            token.id,
            Entry {
                token: Arc::downgrade(&token),
                binding,
            },
        );
        drop(index);
        let sink = OwnerWakeSink {
            token: Arc::downgrade(&token),
            pending: Arc::downgrade(&self.pending),
        };
        Ok((
            OwnerWakeRegistration {
                token,
                pending: Arc::downgrade(&self.pending),
                index: Rc::downgrade(&self.index),
            },
            sink,
        ))
    }
    /// Take the whole pending collection BEFORE any consumer callback. Return
    /// owners in first-dirty arrival order; coalescing retains their position.
    /// Late wakes enter a new collection. Public requeue merges at a late
    /// owner's existing position or appends after already pending owners.
    /// Batch Drop instead restores failed and unvisited requests before late owners.
    /// Work is proportional to queued owners, not registrations.
    pub fn take_pending(&self) -> Vec<OwnerWakeRequest> {
        let mut packets = {
            let mut pending = self.pending.0.lock_recover();
            std::mem::take(&mut *pending)
        };
        let index = self.index.borrow();
        std::mem::take(&mut packets.order)
            .into_iter()
            .filter_map(|id| {
                let packet = packets.entries.remove(&id)?;
                let entry = index.get(&id)?;
                if !Weak::ptr_eq(&entry.token, &packet.token) {
                    return None;
                }
                let token = packet.token.upgrade()?;
                if !token.accepts(&packet.event) {
                    return None;
                }
                Some(OwnerWakeRequest {
                    key: OwnerWakeKey {
                        id,
                        token: packet.token,
                    },
                    event: packet.event,
                    requires_sync: packet.requires_sync,
                    binding: entry.binding.clone(),
                })
            })
            .collect()
    }
    /// Return an unserviced signal without clearing work raised after the take.
    /// A failed fresh admission never becomes a successful receipt.
    pub fn requeue(&self, request: OwnerWakeRequest) -> bool {
        let Some(token) = request.key.token.upgrade() else {
            return false;
        };
        {
            let index = self.index.borrow();
            if !index
                .get(&request.key.id)
                .is_some_and(|entry| Weak::ptr_eq(&entry.token, &request.key.token))
            {
                return false;
            }
        }
        self.pending
            .push(&token, request.event, request.requires_sync)
    }
    /// Queue presence only, not current-owner custody or model purity.
    pub fn has_pending(&self) -> bool {
        !self.pending.0.lock_recover().entries.is_empty()
    }
    /// Park the calling thread, as an idle host's event loop does, until a
    /// wake is queued or `deadline` passes. Whether one is queued.
    pub fn wait_pending_until(&self, deadline: crate::time::Instant) -> bool {
        let mut pending = self.pending.0.lock_recover();
        loop {
            if !pending.entries.is_empty() {
                return true;
            }
            let left = deadline.saturating_duration_since(crate::time::now());
            if left.is_zero() {
                return false;
            }
            pending = self
                .pending
                .1
                .wait_timeout(pending, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }
}

/// A snapshot owns retry custody until each admitted callback completes.
pub(crate) struct OwnerWakeBatch {
    queue: OwnerWakeQueue,
    requests: std::collections::VecDeque<OwnerWakeRequest>,
}
impl OwnerWakeBatch {
    pub(crate) fn front(&self) -> Option<&OwnerWakeRequest> {
        self.requests.front()
    }
    pub(crate) fn complete_front(&mut self) {
        self.requests.pop_front();
    }
}
impl Drop for OwnerWakeBatch {
    fn drop(&mut self) {
        self.queue.restore_failed_prefix(self.requests.drain(..));
    }
}
impl OwnerWakeQueue {
    fn restore_failed_prefix(&self, requests: impl Iterator<Item = OwnerWakeRequest>) {
        let retained = {
            let index = self.index.borrow();
            requests
                .filter_map(|request| {
                    let token = request.key.token.upgrade()?;
                    index
                        .get(&request.key.id)
                        .filter(|entry| Weak::ptr_eq(&entry.token, &request.key.token))?;
                    Some((token, request))
                })
                .collect::<Vec<_>>()
        };
        let mut pending = self.pending.0.lock_recover();
        let mut prefix = Vec::new();
        for (token, request) in retained {
            if !token.accepts(&request.event) {
                continue;
            }
            match pending.entries.entry(token.id) {
                std::collections::hash_map::Entry::Occupied(mut slot) => {
                    if !Weak::ptr_eq(&slot.get().token, &request.key.token) {
                        continue;
                    }
                    slot.get_mut().requires_sync |= request.requires_sync;
                }
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(Packet {
                        token: request.key.token,
                        event: request.event,
                        requires_sync: request.requires_sync,
                    });
                }
            }
            prefix.push(token.id);
        }
        let restored = prefix
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        prefix.extend(
            pending
                .order
                .iter()
                .copied()
                .filter(|id| !restored.contains(id)),
        );
        pending.order = prefix;
    }
    pub(crate) fn take_batch(&self) -> OwnerWakeBatch {
        OwnerWakeBatch {
            queue: self.clone(),
            requests: self.take_pending().into(),
        }
    }
}
