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
use std::sync::{Arc, Mutex, Weak};
use uuid::Uuid;

/// Non-authoritative progress from an actual snapshot service. Errors retain
/// failed/unvisited work; completed owners have returned all ordinary doors.
#[derive(Debug)]
pub struct OwnerModelServiceReport {
    pub attempted: bool,
    pub completed: usize,
    pub result: Result<(), BoundDispatchError>,
}

/// Limits checked between returned original owner-service attempts. A callback
/// already running cannot be preempted. Queue/lookup cost is part of the turn.
#[derive(Clone, Copy, Debug)]
pub struct OwnerTurnBudget {
    pub max_steps: usize,
    pub deadline: Option<crate::time::Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerTurnCutoff {
    Idle,
    SnapshotComplete,
    StepLimit,
    Deadline,
    Blocked,
}

/// Scheduling accounting only; no registration, document or map authority.
#[derive(Debug)]
pub struct OwnerTurnReport {
    /// Retained-owner dispatch attempts, including refused admissions/lookup.
    pub attempted: usize,
    pub completed: usize,
    /// Pending owners after failed/unvisited prefix custody has been restored.
    pub remaining: usize,
    pub cutoff: OwnerTurnCutoff,
    /// Entire measured turn, including intake, lookup and retry restoration.
    pub elapsed: crate::time::Duration,
    /// Time past the supplied deadline on return (zero without a deadline).
    pub overrun: crate::time::Duration,
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
    entries: HashMap<Uuid, PendingNode>,
    head: Option<Uuid>,
    tail: Option<Uuid>,
    #[cfg(test)]
    touches: usize,
}
// Safe indexed FIFO: unlinking a late duplicate never scans unrelated owners.
struct PendingNode {
    packet: Packet,
    prev: Option<Uuid>,
    next: Option<Uuid>,
}
impl PendingState {
    fn node_mut(&mut self, id: &Uuid) -> Option<&mut PendingNode> {
        #[cfg(test)]
        {
            self.touches += 1;
        }
        self.entries.get_mut(id)
    }
    fn remove(&mut self, id: &Uuid) -> Option<Packet> {
        #[cfg(test)]
        {
            self.touches += 1;
        }
        let node = self.entries.remove(id)?;
        if let Some(prev) = node.prev {
            if let Some(previous) = self.node_mut(&prev) {
                previous.next = node.next;
            }
        } else {
            self.head = node.next;
        }
        if let Some(next) = node.next {
            if let Some(following) = self.node_mut(&next) {
                following.prev = node.prev;
            }
        } else {
            self.tail = node.prev;
        }
        Some(node.packet)
    }
    fn push_back(&mut self, id: Uuid, packet: Packet) {
        if let Some(tail) = self.tail {
            if let Some(last) = self.node_mut(&tail) {
                last.next = Some(id);
            }
        } else {
            self.head = Some(id);
        }
        self.entries.insert(
            id,
            PendingNode {
                packet,
                prev: self.tail,
                next: None,
            },
        );
        self.tail = Some(id);
    }
    fn push_front(&mut self, id: Uuid, packet: Packet) {
        if let Some(head) = self.head {
            if let Some(first) = self.node_mut(&head) {
                first.prev = Some(id);
            }
        } else {
            self.tail = Some(id);
        }
        self.entries.insert(
            id,
            PendingNode {
                packet,
                prev: None,
                next: self.head,
            },
        );
        self.head = Some(id);
    }
}
#[derive(Default)]
struct Pending(Mutex<PendingState>);
impl Pending {
    fn push(&self, token: &Arc<Token>, event: WakeEvent, requires_sync: bool) -> bool {
        let mut pending = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Close may race a Send sink. Recheck while holding only the queue loan.
        if !token.accepts(&event) {
            return false;
        }
        let weak = Arc::downgrade(token);
        if let Some(slot) = pending.node_mut(&token.id) {
            if !Weak::ptr_eq(&slot.packet.token, &weak) {
                return false;
            }
            slot.packet.requires_sync |= requires_sync;
        } else {
            pending.push_back(
                token.id,
                Packet {
                    token: weak,
                    event,
                    requires_sync,
                },
            );
        }
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
    pub(crate) fn check_registration(&self) -> Result<(), BoundDispatchError> {
        let token = self
            .key
            .token
            .upgrade()
            .ok_or(BoundDispatchError::ClosedLifetime)?;
        if !token.accepts(&self.event) {
            return Err(BoundDispatchError::ClosedLifetime);
        }
        Ok(())
    }
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
        self.check_registration()?;
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
            let mut entries = pending
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if entries.entries.get(&self.token.id).is_some_and(|entry| {
                Weak::ptr_eq(&entry.packet.token, &Arc::downgrade(&self.token))
            }) {
                entries.remove(&self.token.id);
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
        self.take_prefix(usize::MAX)
    }
    // Both bounded batches and public snapshots use the same visible FIFO.
    // At most `limit` nodes are visited, even if some registrations retired.
    fn take_prefix(&self, limit: usize) -> Vec<OwnerWakeRequest> {
        let mut pending = self
            .pending
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = self.index.borrow();
        let count = limit.min(pending.entries.len());
        let mut requests = Vec::with_capacity(count);
        for _ in 0..count {
            let Some(id) = pending.head else {
                break;
            };
            let Some(packet) = pending.remove(&id) else {
                break;
            };
            let Some(entry) = index.get(&id) else {
                continue;
            };
            if !Weak::ptr_eq(&entry.token, &packet.token) {
                continue;
            }
            let Some(token) = packet.token.upgrade() else {
                continue;
            };
            if !token.accepts(&packet.event) {
                continue;
            }
            requests.push(OwnerWakeRequest {
                key: OwnerWakeKey {
                    id,
                    token: packet.token,
                },
                event: packet.event,
                requires_sync: packet.requires_sync,
                binding: entry.binding.clone(),
            });
        }
        requests
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
        self.pending_count() != 0
    }
    /// Pending scheduling keys, not an admitted owner census.
    pub fn pending_count(&self) -> usize {
        self.pending
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .len()
    }
    /// Registered metadata count; never a mounted or freshly admitted census.
    pub fn registered_count(&self) -> usize {
        self.index.borrow().len()
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
    fn restore_failed_prefix(&self, requests: impl DoubleEndedIterator<Item = OwnerWakeRequest>) {
        let index = self.index.borrow();
        let mut pending = self
            .pending
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Reverse insertion restores the original prefix ahead of the untouched
        // tail and all late wakes. Each duplicate is unlinked in constant time.
        for request in requests.rev() {
            let Some(token) = request.key.token.upgrade() else {
                continue;
            };
            if !index
                .get(&request.key.id)
                .is_some_and(|entry| Weak::ptr_eq(&entry.token, &request.key.token))
                || !token.accepts(&request.event)
            {
                continue;
            }
            if pending
                .entries
                .get(&token.id)
                .is_some_and(|node| !Weak::ptr_eq(&node.packet.token, &request.key.token))
            {
                continue;
            }
            let mut packet = pending.remove(&token.id).unwrap_or(Packet {
                token: request.key.token,
                event: request.event,
                requires_sync: false,
            });
            packet.requires_sync |= request.requires_sync;
            pending.push_front(token.id, packet);
        }
    }
    pub(crate) fn take_batch(&self, limit: usize) -> OwnerWakeBatch {
        OwnerWakeBatch {
            queue: self.clone(),
            requests: self.take_prefix(limit).into(),
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::OwnerWakeQueue;
    pub(crate) fn take_touches(queue: &OwnerWakeQueue) -> usize {
        let mut state = queue.pending.0.lock().unwrap();
        std::mem::take(&mut state.touches)
    }
}
