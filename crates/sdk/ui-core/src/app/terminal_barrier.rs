//! Retain original completed outcomes while document queue admission is busy.
//! Retry only publication of unsent owner frontiers, never action execution.
use super::App;
use crate::collaboration::{CollabRequest, PrimaryRouteWitness};
use std::collections::VecDeque;
use std::io::Write;
use tilo_actions::{InvocationId, InvocationUpdate};

const MAX_TERMINALS: usize = 32;
// Serialized outcomes plus newly owned metadata, not a total process/RSS
// limit. Shared owner/backend/check allocations remain under their original
// admission budgets; retaining handles here does not clone their payloads.
const MAX_TERMINAL_BYTES: usize = runtime_collab::document_contribution::MAX_SIGNED_OP_BYTES;

struct PendingTerminal {
    wire_id: u64,
    update: InvocationUpdate,
    route: PrimaryRouteWitness,
    bytes: usize,
    owners: Vec<(
        crate::collaboration::RouteSpec,
        crate::history_mode::HistoryOwner,
    )>,
    guard: Option<crate::invocation_scope::TerminalPublicationGuard>,
}

#[derive(Default)]
pub(super) struct PendingTerminals {
    queue: VecDeque<PendingTerminal>,
    bytes: usize,
}

struct CountBytes(usize);
impl Write for CountBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|len| *len <= MAX_TERMINAL_BYTES)
            .ok_or_else(|| std::io::Error::other("terminal retention capacity"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl PendingTerminals {
    pub(super) fn contains(&self, invocation: InvocationId) -> bool {
        self.queue
            .iter()
            .any(|pending| pending.update.invocation() == invocation)
    }
    fn retain(
        &mut self,
        wire_id: u64,
        update: InvocationUpdate,
        route: PrimaryRouteWitness,
        owners: Vec<(
            crate::collaboration::RouteSpec,
            crate::history_mode::HistoryOwner,
        )>,
        guard: Option<crate::invocation_scope::TerminalPublicationGuard>,
    ) -> Result<(), ()> {
        if self.queue.len() >= MAX_TERMINALS
            || owners.len() > 128
            || self.contains(update.invocation())
        {
            return Err(());
        }
        let mut size = CountBytes(0);
        serde_json::to_writer(&mut size, &update).map_err(|_| ())?;
        size.0 = size
            .0
            .checked_add(guard.as_ref().map_or(
                0,
                crate::invocation_scope::TerminalPublicationGuard::retained_bytes,
            ))
            .ok_or(())?;
        size.0 = size
            .0
            .checked_add(std::mem::size_of::<PendingTerminal>())
            .and_then(|bytes| bytes.checked_add(route.route.app_id.capacity()))
            .and_then(|bytes| {
                bytes.checked_add(route.route.record.as_ref().map_or(0, String::capacity))
            })
            .and_then(|bytes| {
                bytes.checked_add(owners.capacity().checked_mul(std::mem::size_of::<(
                    crate::collaboration::RouteSpec,
                    crate::history_mode::HistoryOwner,
                )>())?)
            })
            .ok_or(())?;
        for (route, _) in &owners {
            size.0 = size
                .0
                .checked_add(route.app_id.capacity())
                .and_then(|bytes| {
                    bytes.checked_add(route.record.as_ref().map_or(0, String::capacity))
                })
                .ok_or(())?;
        }
        let total = self
            .bytes
            .checked_add(size.0)
            .filter(|total| *total <= MAX_TERMINAL_BYTES)
            .ok_or(())?;
        self.queue.push_back(PendingTerminal {
            wire_id,
            update,
            route,
            bytes: size.0,
            owners,
            guard,
        });
        self.bytes = total;
        Ok(())
    }
    fn pop(&mut self) -> Option<PendingTerminal> {
        let pending = self.queue.pop_front()?;
        self.bytes -= pending.bytes;
        Some(pending)
    }
}

impl App {
    fn refuse_terminal_retention(&mut self) {
        self.layout.layout_dirty = true;
        self.action_publication_failure = Some(
            "Action outcome is unknown; original terminal publication custody is unavailable."
                .into(),
        );
        self.restore_primary_publication_obligations();
        if let Some(room) = self.persistence.room_index.as_mut() {
            room.advance_outbound(Vec::new());
        }
        self.actions.pending_terminals = PendingTerminals::default();
        self.collaboration.manager.stop_after_publication_failure();
    }

    pub(super) fn forward_invocation_update(
        &mut self,
        wire_id: u64,
        update: InvocationUpdate,
        guard: Option<crate::invocation_scope::TerminalPublicationGuard>,
    ) -> bool {
        if update.is_terminal() {
            if guard.as_ref().is_some_and(|guard| guard.check().is_err()) {
                self.refuse_terminal_retention();
                return false;
            }
            match self.queue_documents_before_terminal() {
                super::document_routes::TerminalDocumentQueue::Queued => {}
                super::document_routes::TerminalDocumentQueue::Refused => return false,
                super::document_routes::TerminalDocumentQueue::Busy
                | super::document_routes::TerminalDocumentQueue::NotReady => {
                    let Some(route) = self.bound_broker_route().cloned() else {
                        self.refuse_terminal_retention();
                        return false;
                    };
                    let owners = self
                        .document_routes
                        .owners
                        .iter()
                        .flat_map(|entry| {
                            entry
                                .owners
                                .iter()
                                .map(|owner| (entry.route.clone(), owner.clone()))
                        })
                        .take(129)
                        .collect();
                    if self
                        .actions
                        .pending_terminals
                        .retain(wire_id, update, route, owners, guard)
                        .is_err()
                    {
                        self.refuse_terminal_retention();
                        return false;
                    }
                    return true;
                }
            }
        }
        self.collaboration
            .manager
            .send(CollabRequest::UpdateInvocation {
                id: wire_id,
                update,
            });
        true
    }

    pub(super) fn flush_pending_terminals(&mut self) -> bool {
        if self.actions.pending_terminals.queue.is_empty()
            || self.action_publication_failure.is_some()
        {
            return false;
        }
        let Some(route) = self.bound_broker_route().cloned() else {
            self.refuse_terminal_retention();
            return false;
        };
        self.refresh_document_routes();
        if self.actions.pending_terminals.queue.iter().any(|pending| {
            pending.route != route
                || pending
                    .guard
                    .as_ref()
                    .is_some_and(|guard| guard.check().is_err())
                || pending.owners.iter().any(|(owner_route, owner)| {
                    owner.validate().is_err()
                        || !self.document_routes.owners.iter().any(|entry| {
                            entry.route == *owner_route
                                && entry.owners.iter().any(|current| current.same_owner(owner))
                        })
                })
        }) {
            self.refuse_terminal_retention();
            return false;
        }
        if !matches!(
            self.queue_documents_before_terminal(),
            super::document_routes::TerminalDocumentQueue::Queued
        ) {
            return false;
        }
        while let Some(pending) = self.actions.pending_terminals.pop() {
            self.collaboration
                .manager
                .send(CollabRequest::UpdateInvocation {
                    id: pending.wire_id,
                    update: pending.update,
                });
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route() -> PrimaryRouteWitness {
        PrimaryRouteWitness {
            route: crate::collaboration::RouteSpec::app("test", uuid::Uuid::from_u128(1)),
            epoch: uuid::Uuid::from_u128(2),
        }
    }
    fn outcome(id: u128) -> InvocationUpdate {
        InvocationUpdate::Cancelled {
            invocation: InvocationId(uuid::Uuid::from_u128(id)),
            message: None,
        }
    }
    #[test]
    fn retention_caps_count_bytes_and_duplicate_originals_before_insertion() {
        let mut pending = PendingTerminals::default();
        pending
            .retain(1, outcome(1), route(), vec![], None)
            .unwrap();
        let charged = pending.bytes;
        assert!(
            pending
                .retain(2, outcome(1), route(), vec![], None)
                .is_err()
        );
        assert_eq!(pending.bytes, charged);
        for id in 2..=32 {
            pending
                .retain(
                    u64::try_from(id).unwrap(),
                    outcome(id),
                    route(),
                    vec![],
                    None,
                )
                .unwrap();
        }
        assert!(
            pending
                .retain(33, outcome(33), route(), vec![], None)
                .is_err()
        );
        while pending.pop().is_some() {}
        assert_eq!(pending.bytes, 0);
        let huge = InvocationUpdate::Cancelled {
            invocation: InvocationId(uuid::Uuid::from_u128(100)),
            message: Some("x".repeat(MAX_TERMINAL_BYTES)),
        };
        assert!(pending.retain(100, huge, route(), vec![], None).is_err());
        assert!(pending.queue.is_empty());
        assert_eq!(pending.bytes, 0);
    }
}
