//! The idempotency log every provider shares: an invocation is keyed by its caller-chosen id, so
//! a retry after a lost reply finds the original invocation instead of running the action twice.

use std::collections::{HashMap, VecDeque};

use tilo_actions::outcome::InvocationId;
use tilo_actions::remote::{ActionCallError, Caller, InvokeRequest, asked};

/// Default number of invocations an [`InvocationLog`] remembers.
pub const DEFAULT_LOG_CAPACITY: usize = 1024;

struct LogEntry<T> {
    caller: Caller,
    request: InvokeRequest,
    /// `None` while a start begun with [`InvocationLog::reserve`] is in
    /// flight.
    handle: Option<T>,
}

/// The idempotency log behind
/// [`ActionProvider::invoke`](tilo_actions::remote::ActionProvider::invoke).
///
/// Maps each admitted [`InvocationId`] to the caller and request that
/// admitted it and to the provider's own handle for the running job. A retry
/// with the same caller and request returns the existing handle without
/// starting anything; any other reuse of the id is a conflict. Only admitted
/// invocations are remembered — a refused one ran nothing, so retrying it
/// may be admitted later. At `capacity`, the oldest entry whose admission
/// completed is forgotten. In-flight reservations are never evicted.
pub struct InvocationLog<T> {
    entries: HashMap<InvocationId, LogEntry<T>>,
    order: VecDeque<InvocationId>,
    capacity: usize,
}

impl<T> Default for InvocationLog<T> {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_LOG_CAPACITY)
    }
}

/// What [`InvocationLog::reserve`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reservation {
    /// The id is newly claimed; the caller must now start the job.
    Reserved,
    /// The same caller and request were already admitted.
    Admitted,
}

/// Whether [`InvocationLog::admit`] started a job or found one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    Started,
    Retried,
}

impl<T> InvocationLog<T> {
    /// A log remembering at most `capacity` invocations (at least one).
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// Admit `request` for `caller`, calling `start` only if the id is new.
    ///
    /// # Errors
    ///
    /// Returns [`ActionCallError::InvocationConflict`] when the id was
    /// admitted for another caller or request, [`ActionCallError::Unavailable`]
    /// when all slots are reserved, or whatever `start` returns.
    pub fn admit(
        &mut self,
        caller: &Caller,
        request: &InvokeRequest,
        start: impl FnOnce() -> Result<T, ActionCallError>,
    ) -> Result<(&T, Admission), ActionCallError> {
        let id = request.invocation;
        let admission = match self.reserve(caller, request)? {
            Reservation::Admitted => Admission::Retried,
            Reservation::Reserved => match start() {
                Ok(handle) => {
                    self.complete(&id, handle);
                    Admission::Started
                }
                Err(error) => {
                    self.abandon(&id);
                    return Err(error);
                }
            },
        };
        self.entries
            .get(&id)
            .and_then(|entry| entry.handle.as_ref())
            .map(|handle| (handle, admission))
            .ok_or(ActionCallError::UnknownInvocation)
    }

    /// First half of an admission whose start completes later (for example
    /// across an asynchronous resolution): claim the id for `caller` and
    /// `request`, then [`complete`](Self::complete) or
    /// [`abandon`](Self::abandon) it.
    ///
    /// # Errors
    ///
    /// Returns [`ActionCallError::InvocationConflict`] when the id belongs to
    /// another caller or request, [`ActionCallError::Unanswered`] while an
    /// earlier start of the same invocation is still in flight, and
    /// [`ActionCallError::Unavailable`] while every slot is held by an
    /// in-flight reservation.
    pub fn reserve(
        &mut self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Reservation, ActionCallError> {
        let id = request.invocation;
        if let Some(entry) = self.entries.get_mut(&id) {
            if !caller.continues(&entry.caller) || !entry.request.same_invocation(request) {
                return Err(ActionCallError::InvocationConflict);
            }
            // Asked again on the connection that replaced its own: it is this one's from now on.
            entry.caller.clone_from(caller);
            // Its first start has not ended: a workspace action that runs whole as it starts
            // (a server taking in a workspace's gigabytes) is still going. Nothing is known of
            // what it did yet, so the caller asks the same invocation again, as for one whose
            // answer was lost.
            return if entry.handle.is_some() {
                Ok(Reservation::Admitted)
            } else {
                Err(ActionCallError::Unanswered)
            };
        }
        while self.order.len() >= self.capacity {
            let oldest_completed = self.order.iter().position(|candidate| {
                self.entries
                    .get(candidate)
                    .is_some_and(|entry| entry.handle.is_some())
            });
            let Some(index) = oldest_completed else {
                return Err(ActionCallError::Unavailable {
                    reason: "all invocation slots are still being admitted; retry".into(),
                });
            };
            if let Some(oldest) = self.order.remove(index) {
                self.entries.remove(&oldest);
            }
        }
        self.order.push_back(id);
        self.entries.insert(
            id,
            LogEntry {
                caller: caller.clone(),
                request: InvokeRequest {
                    wait_ms: None,
                    appearance: None,
                    ..request.clone()
                },
                handle: None,
            },
        );
        Ok(Reservation::Reserved)
    }

    /// Admit `request` again under its id where its person approved the asked record it waits
    /// in: the entry `caller` admitted for the same target and arguments, whose handle `waits`
    /// says is still asked, is reserved again carrying their answer, and the caller completes or
    /// abandons it as after [`Self::reserve`]. `waits` is handed `None` while the entry's first
    /// start is still in flight. False where nothing of that waits, which a reserve then refuses
    /// as another invocation.
    pub fn readmit(
        &mut self,
        caller: &Caller,
        request: &InvokeRequest,
        waits: impl FnOnce(Option<&T>) -> bool,
    ) -> bool {
        let Some(entry) = self.entries.get_mut(&request.invocation) else {
            return false;
        };
        let again = request.approval().is_some()
            && caller.continues(&entry.caller)
            && asked::same_call(&entry.request, request)
            && waits(entry.handle.as_ref());
        if again {
            entry.request = InvokeRequest {
                wait_ms: None,
                appearance: None,
                ..request.clone()
            };
            entry.handle = None;
        }
        again
    }

    /// Record the provider's handle for a reserved invocation.
    pub fn complete(&mut self, id: &InvocationId, handle: T) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.handle = Some(handle);
        }
    }

    /// Release a reservation whose start failed: nothing ran, so the id may
    /// be admitted later.
    pub fn abandon(&mut self, id: &InvocationId) {
        if self
            .entries
            .get(id)
            .is_some_and(|entry| entry.handle.is_none())
        {
            self.entries.remove(id);
            self.order.retain(|candidate| candidate != id);
        }
    }

    /// The request that admitted invocation `id`, for the provider that keeps this log: what an
    /// app it runs the invocation in carries as the proof of a call it makes for it.
    #[must_use]
    pub fn request_of(&self, id: &InvocationId) -> Option<&InvokeRequest> {
        self.entries.get(id).map(|entry| &entry.request)
    }

    /// Invocation `id` whoever admitted it, with the caller that did, once its start is done:
    /// for a provider that decides by its own rule who else may read it.
    #[must_use]
    pub fn admitted_by(&self, id: &InvocationId) -> Option<(&Caller, &T)> {
        let entry = self.entries.get(id)?;
        Some((&entry.caller, entry.handle.as_ref()?))
    }

    /// The handle for an invocation `caller` admitted, or the caller it continues
    /// ([`Caller::continues`]).
    #[must_use]
    pub fn get(&self, caller: &Caller, id: &InvocationId) -> Option<&T> {
        self.entries
            .get(id)
            .filter(|entry| caller.follows(&entry.caller))
            .and_then(|entry| entry.handle.as_ref())
    }

    /// The invocation `caller` admitted under `id`, as its handle: `Some(None)` while its start
    /// is still in flight, `None` where `caller` admitted nothing under `id`.
    #[must_use]
    pub fn admitted(&self, caller: &Caller, id: &InvocationId) -> Option<Option<&T>> {
        self.entries
            .get(id)
            .filter(|entry| caller.follows(&entry.caller))
            .map(|entry| entry.handle.as_ref())
    }

    /// Mutable access to the handle for an invocation `caller` admitted.
    pub fn get_mut(&mut self, caller: &Caller, id: &InvocationId) -> Option<&mut T> {
        self.entries
            .get_mut(id)
            .filter(|entry| caller.follows(&entry.caller))
            .and_then(|entry| entry.handle.as_mut())
    }
}
