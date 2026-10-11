//! Immutable shovel custody. Constructors are private to the sender.
//! A queued attempt and its result are not durable sender receipts.
use super::{OutboxFailure, codec::RowView};
use crate::collaboration::RouteSpec;
use crate::time::Instant;
use runtime_collab::{PublishedInvocationBinding, PublishedInvocationObservation};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Operation {
    Invoke,
    Observe,
    Cancel,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttemptFailure {
    Deadline,
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Ticket {
    pub instance: [u8; 16],
    pub host: [u8; 16],
    pub invocation: [u8; 16],
}

struct Inner {
    route: RouteSpec,
    binding: PublishedInvocationBinding,
    revision: String,
    resource: String,
    action: String,
    argument: Option<String>,
    confirmation: String,
    ticket: Ticket,
    operation: Operation,
    deadline: Instant,
    claimed: AtomicBool,
    completed: AtomicBool,
    retired: AtomicBool,
    #[cfg(feature = "broker-transport")]
    queued_bytes: usize,
}
/// Opaque request custody. Clone shares the original backing.
pub struct ManagedAttempt {
    inner: Option<Arc<Inner>>,
}
/// The owner keeps this private fence until all queued/in-flight/result copies end.
pub(super) struct AttemptOwner {
    attempt: ManagedAttempt,
}
struct ResultInner {
    result: Result<PublishedInvocationObservation, AttemptFailure>,
    attempt: ManagedAttempt,
}
/// Opaque response custody. No public constructor or durable-success meaning.
pub struct ManagedAttemptResult {
    inner: Option<Arc<ResultInner>>,
}

impl std::fmt::Debug for ManagedAttempt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedAttempt").finish_non_exhaustive()
    }
}
impl std::fmt::Debug for ManagedAttemptResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedAttemptResult")
            .finish_non_exhaustive()
    }
}
impl Clone for ManagedAttempt {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}
impl Clone for ManagedAttemptResult {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}
impl Drop for ManagedAttempt {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            drop(Arc::into_inner(inner));
        }
    }
}
impl Drop for ManagedAttemptResult {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            drop(Arc::into_inner(inner));
        }
    }
}
impl Drop for AttemptOwner {
    fn drop(&mut self) {
        self.attempt.shared().retired.store(true, Ordering::Release);
    }
}

pub(crate) enum RequestView<'a> {
    Invoke {
        invocation: &'a [u8; 16],
        binding: &'a PublishedInvocationBinding,
        revision: &'a str,
        resource: &'a str,
        action: &'a str,
        argument: Option<&'a str>,
        confirmation_json: &'a str,
    },
    Observe {
        host: &'a [u8; 16],
        invocation: &'a [u8; 16],
    },
    Cancel {
        host: &'a [u8; 16],
        invocation: &'a [u8; 16],
    },
}
// Pinned ArcInner: two atomic counters followed by the payload, repr(C,align(2)).
fn arc_bytes<T>() -> Result<usize, OutboxFailure> {
    std::alloc::Layout::new::<[std::sync::atomic::AtomicUsize; 2]>()
        .extend(std::alloc::Layout::new::<T>())
        .map(|(layout, _)| layout.pad_to_align().size())
        .map_err(|_| OutboxFailure::Capacity)
}
fn copy(text: &str) -> Result<String, OutboxFailure> {
    let mut value = String::new();
    value
        .try_reserve_exact(text.len())
        .map_err(|_| OutboxFailure::Capacity)?;
    value.push_str(text);
    Ok(value)
}
impl AttemptOwner {
    pub(super) fn ticket(&self) -> Ticket {
        self.attempt.shared().ticket
    }
    /// Bytes an attempt and its bounded result retain while queued, for the
    /// broker's queue meter. Also refuses an invalid row or confirmation.
    pub(super) fn queued_bytes(
        row: &RowView<'_>,
        confirmation: &str,
    ) -> Result<usize, OutboxFailure> {
        row.encoded_len()?;
        if confirmation.len() > 4096 || confirmation.contains('\0') {
            return Err(OutboxFailure::Capacity);
        }
        let mut bytes = arc_bytes::<Inner>()?
            .checked_add(arc_bytes::<ResultInner>()?)
            .ok_or(OutboxFailure::Capacity)?;
        for text in [
            row.caller.app,
            row.caller.record,
            row.target.app,
            row.target.record,
            row.revision,
            row.resource,
            row.action,
            row.argument.unwrap_or(""),
            confirmation,
        ] {
            bytes = bytes
                .checked_add(text.len())
                .ok_or(OutboxFailure::Capacity)?;
        }
        // The bounded response String arrives in the single ResultInner.
        bytes
            .checked_add(super::codec::MAX_OBSERVATION)
            .ok_or(OutboxFailure::Capacity)
    }
    pub(super) fn new(
        row: &RowView<'_>,
        instance: [u8; 16],
        room: uuid::Uuid,
        operation: Operation,
        confirmation: &str,
        deadline: Instant,
    ) -> Result<(Self, ManagedAttempt), OutboxFailure> {
        #[cfg(feature = "broker-transport")]
        let queued_bytes = Self::queued_bytes(row, confirmation)?;
        #[cfg(not(feature = "broker-transport"))]
        Self::queued_bytes(row, confirmation)?;
        if instance == [0; 16] || room.is_nil() {
            return Err(OutboxFailure::Invalid);
        }
        let inner = Inner {
            route: RouteSpec {
                app_id: copy(row.caller.app)?,
                cluster_id: room,
                record: if row.caller.record.is_empty() {
                    None
                } else {
                    Some(copy(row.caller.record)?)
                },
            },
            binding: PublishedInvocationBinding {
                host: row.target.host,
                peer_id: row.target.peer,
                app_id: copy(row.target.app)?,
                record_id: copy(row.target.record)?,
                root: row.target.root,
                device: row.target.device,
                target_binding: row.target.binding,
            },
            revision: copy(row.revision)?,
            resource: copy(row.resource)?,
            action: copy(row.action)?,
            argument: row.argument.map(copy).transpose()?,
            confirmation: copy(confirmation)?,
            ticket: Ticket {
                instance,
                host: row.target.host,
                invocation: row.invocation,
            },
            operation,
            deadline,
            claimed: AtomicBool::new(false),
            completed: AtomicBool::new(false),
            retired: AtomicBool::new(false),
            #[cfg(feature = "broker-transport")]
            queued_bytes,
        };
        let attempt = ManagedAttempt {
            inner: Some(Arc::new(inner)),
        };
        Ok((
            Self {
                attempt: attempt.clone(),
            },
            attempt,
        ))
    }
    pub(super) fn quiescent(&self) -> bool {
        Arc::strong_count(self.attempt.shared()) == 1
    }
    pub(super) fn matches(&self, result: &ManagedAttemptResult) -> bool {
        Arc::ptr_eq(self.attempt.shared(), result.shared().attempt.shared())
    }
}
impl ManagedAttempt {
    #[expect(
        clippy::expect_used,
        reason = "only Drop takes the shared attempt out of its slot"
    )]
    fn shared(&self) -> &Arc<Inner> {
        self.inner.as_ref().expect("live managed attempt")
    }
    #[cfg(feature = "broker-transport")]
    pub(crate) fn meter_bytes(&self) -> usize {
        self.shared().queued_bytes
    }
    pub(crate) fn deadline(&self) -> Instant {
        self.shared().deadline
    }
    pub(crate) fn claim(
        &self,
        route: &RouteSpec,
    ) -> Option<Result<RequestView<'_>, AttemptFailure>> {
        let inner = self.shared();
        if inner
            .claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }
        if inner.retired.load(Ordering::Acquire) || &inner.route != route {
            return Some(Err(AttemptFailure::Unavailable));
        }
        Some(Ok(match inner.operation {
            Operation::Invoke => RequestView::Invoke {
                invocation: &inner.ticket.invocation,
                binding: &inner.binding,
                revision: &inner.revision,
                resource: &inner.resource,
                action: &inner.action,
                argument: inner.argument.as_deref(),
                confirmation_json: &inner.confirmation,
            },
            Operation::Observe => RequestView::Observe {
                host: &inner.ticket.host,
                invocation: &inner.ticket.invocation,
            },
            Operation::Cancel => RequestView::Cancel {
                host: &inner.ticket.host,
                invocation: &inner.ticket.invocation,
            },
        }))
    }
    pub(crate) fn finish(
        self,
        result: Result<PublishedInvocationObservation, AttemptFailure>,
    ) -> Option<ManagedAttemptResult> {
        let inner = self.shared();
        if !inner.claimed.load(Ordering::Acquire)
            || inner
                .completed
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return None;
        }
        let result = match result {
            Ok(value)
                if value.host != inner.ticket.host
                    || value.invocation != inner.ticket.invocation
                    || value.result.as_ref().is_some_and(|r| {
                        r.id == 0 || r.update_json.capacity() > super::codec::MAX_OBSERVATION
                    }) =>
            {
                Err(AttemptFailure::Unavailable)
            }
            other => other,
        };
        Some(ManagedAttemptResult {
            inner: Some(Arc::new(ResultInner {
                result,
                attempt: self,
            })),
        })
    }
}
/// Host-local request view, for a sender whose receiver runs in the same
/// process (a Cloud host and its own broker). It is the same original row the
/// SDK transport would send; it confers no authority.
pub enum LocalRequest<'a> {
    Invoke {
        invocation: &'a [u8; 16],
        binding: &'a PublishedInvocationBinding,
        revision: &'a str,
        resource: &'a str,
        action: &'a str,
        argument: Option<&'a str>,
        confirmation_json: &'a str,
    },
    Observe {
        host: &'a [u8; 16],
        invocation: &'a [u8; 16],
    },
    Cancel {
        host: &'a [u8; 16],
        invocation: &'a [u8; 16],
    },
}
/// Host-local transport outcome; neither proves that execution did not occur.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalFailure {
    Deadline,
    Unavailable,
}
impl ManagedAttempt {
    /// Deliver this attempt to the host's in-process receiver. Same custody as
    /// the SDK transport: it is claimed once for `route` (a clone or replay
    /// returns `None`), never retried and never given a new identity; the
    /// reply is correlated and bounded exactly as a wire reply by `finish`.
    pub fn deliver_local(
        self,
        route: &RouteSpec,
        send: impl FnOnce(LocalRequest<'_>) -> Result<PublishedInvocationObservation, LocalFailure>,
    ) -> Option<ManagedAttemptResult> {
        let result = {
            let deadline = self.deadline();
            match self.claim(route)? {
                Err(error) => Err(error),
                Ok(_) if crate::time::now() >= deadline => Err(AttemptFailure::Deadline),
                Ok(view) => {
                    let request = match view {
                        RequestView::Invoke {
                            invocation,
                            binding,
                            revision,
                            resource,
                            action,
                            argument,
                            confirmation_json,
                        } => LocalRequest::Invoke {
                            invocation,
                            binding,
                            revision,
                            resource,
                            action,
                            argument,
                            confirmation_json,
                        },
                        RequestView::Observe { host, invocation } => {
                            LocalRequest::Observe { host, invocation }
                        }
                        RequestView::Cancel { host, invocation } => {
                            LocalRequest::Cancel { host, invocation }
                        }
                    };
                    send(request).map_err(|failure| match failure {
                        LocalFailure::Deadline => AttemptFailure::Deadline,
                        LocalFailure::Unavailable => AttemptFailure::Unavailable,
                    })
                }
            }
        };
        self.finish(result)
    }
}
impl ManagedAttemptResult {
    #[expect(
        clippy::expect_used,
        reason = "only Drop takes the shared result out of its slot"
    )]
    fn shared(&self) -> &Arc<ResultInner> {
        self.inner.as_ref().expect("live managed attempt result")
    }
    #[cfg(feature = "broker-transport")]
    pub(crate) fn meter_bytes(&self) -> usize {
        self.shared().attempt.meter_bytes()
    }
    pub(super) fn result(&self) -> &Result<PublishedInvocationObservation, AttemptFailure> {
        &self.shared().result
    }
}
#[cfg(test)]
mod tests;
