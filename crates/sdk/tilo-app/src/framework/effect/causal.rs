//! Outbox entries an admitted local invocation queued, carried out under
//! that invocation, which completes once they settled
//! ([`RecordEnv::outbox_awaited`](crate::framework::RecordEnv::outbox_awaited)).

use super::{
    ActionFailure, ActionOutput, InvocationId, InvocationUpdate, ResourceOperation,
    ResourceOperationStart, failed, failure,
};
use std::any::Any;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use ui_core::invocation_scope::ResourceInvocationScope;
use ui_core::widget::document_effects::AppliedDocument;

const MAX_CAUSES: usize = 256;
const MAX_INVOCATIONS: usize = 128;

pub(super) struct Completion {
    remaining: usize,
    admitted: usize,
    output: ActionOutput,
    error: Option<ActionFailure>,
    custody: Option<ResourceInvocationScope>,
}

impl Completion {
    pub(super) fn finished(&mut self, result: Result<ActionOutput, ActionFailure>) {
        let Some(remaining) = self.remaining.checked_sub(1) else {
            self.error.get_or_insert_with(|| {
                failure("document receipt has no pending consequence".into())
            });
            return;
        };
        self.remaining = remaining;
        if let Err(error) = result {
            self.error.get_or_insert(error);
        }
    }
}

pub(super) type Group = Rc<RefCell<Completion>>;

/// A causal drain, type-erased for the root: the states it starts from are
/// the resource's.
pub(super) trait CausalDrainer {
    fn start(
        &mut self,
        before: &dyn Any,
        after: &dyn Any,
        operation: &ResourceOperation<'_>,
        group: &Group,
    ) -> Result<(), String>;
    fn poll(&mut self);
    fn cancel(&mut self, invocation: InvocationId);
    fn retire(&mut self);
}

struct RegisteredCausal {
    alive: Weak<()>,
    drain: Box<dyn CausalDrainer>,
}

#[derive(Default)]
pub(super) struct CausalEffects {
    drains: Vec<RegisteredCausal>,
    groups: HashMap<InvocationId, Group>,
}

pub(super) type ReadState = fn(&tilo_resource::Doc) -> Result<Box<dyn Any>, String>;

impl CausalEffects {
    pub(super) fn register_drainer(&mut self, alive: Weak<()>, drain: Box<dyn CausalDrainer>) {
        self.drains.push(RegisteredCausal { alive, drain });
    }

    pub(super) fn registered(&self) -> bool {
        self.drains
            .iter()
            .any(|entry| entry.alive.strong_count() > 0)
    }

    pub(super) fn start(
        &mut self,
        operation: &ResourceOperation<'_>,
        applied: &AppliedDocument<'_>,
        read: ReadState,
    ) -> Result<Option<ResourceOperationStart>, String> {
        if !self.registered() {
            return Ok(None);
        }
        operation.check()?;
        if self.groups.len() >= MAX_INVOCATIONS || self.groups.contains_key(&applied.invocation()) {
            return Err("too many pending document consequences".into());
        }
        // Only newly pending keys between the actual executor frontiers may
        // inherit this caller. Old/replayed/remote entries inherit nothing.
        let mut snapshot = applied.document().clone();
        let before_doc = snapshot
            .fork_at(applied.before())
            .map_err(|error| error.to_string())?;
        let before = read(&before_doc)?;
        let after = read(applied.document())?;
        let group = Rc::new(RefCell::new(Completion {
            remaining: 0,
            admitted: 0,
            output: applied.output().clone(),
            error: None,
            custody: operation.with_custody().1.cloned(),
        }));
        for entry in &mut self.drains {
            if entry.alive.strong_count() == 0 {
                continue;
            }
            if let Err(error) = entry.drain.start(&*before, &*after, operation, &group) {
                group
                    .borrow_mut()
                    .error
                    .get_or_insert_with(|| failure(error));
                break;
            }
        }
        if group.borrow().remaining == 0 && group.borrow().error.is_none() {
            return Ok(Some(ResourceOperationStart::Completed(
                applied.output().clone(),
            )));
        }
        self.groups.insert(applied.invocation(), group);
        Ok(Some(ResourceOperationStart::Accepted))
    }

    pub(super) fn poll(&mut self) {
        for entry in &mut self.drains {
            if entry.alive.strong_count() == 0 {
                entry.drain.retire();
            } else {
                entry.drain.poll();
            }
        }
        self.drains.retain(|entry| entry.alive.strong_count() > 0);
    }

    pub(super) fn poll_invocation(
        &mut self,
        invocation: InvocationId,
        cancel: bool,
    ) -> Option<InvocationUpdate> {
        let group = Rc::clone(self.groups.get(&invocation)?);
        let custody = group.borrow().custody.clone();
        if cancel {
            if let Some(custody) = &custody {
                custody.cancel();
            }
            for entry in &mut self.drains {
                entry.drain.cancel(invocation);
            }
            self.groups.remove(&invocation);
            return Some(InvocationUpdate::Cancelled {
                invocation,
                message: None,
            });
        }
        if let Some(error) = custody.as_ref().and_then(|custody| custody.check().err()) {
            for entry in &mut self.drains {
                entry.drain.cancel(invocation);
            }
            self.groups.remove(&invocation);
            return Some(failed(invocation, failure(error)));
        }
        self.poll();
        if group.borrow().remaining != 0 {
            return None;
        }
        self.groups.remove(&invocation);
        let mut completion = group.borrow_mut();
        Some(match completion.error.take() {
            Some(error) => failed(invocation, error),
            None => InvocationUpdate::Succeeded {
                invocation,
                output: completion.output.clone(),
            },
        })
    }
}

/// An outbox whose entries an admitted local invocation queues run at once
/// under that invocation, which completes once they settled.
impl CausalDrainer for Rc<RefCell<super::OutboxDrain>> {
    fn start(
        &mut self,
        before: &dyn Any,
        after: &dyn Any,
        operation: &ResourceOperation<'_>,
        group: &Group,
    ) -> Result<(), String> {
        let mut drain = self.borrow_mut();
        let (Some(before), Some(after)) = (drain.owned(before), drain.owned(after)) else {
            return Err("a document consequence read another resource".into());
        };
        let previous: HashSet<tilo_resource::Id> = before.into_iter().map(|(id, _)| id).collect();
        let causes: Vec<_> = after
            .into_iter()
            .filter(|(id, _)| !previous.contains(id))
            .collect();
        if causes.len().saturating_add(group.borrow().admitted) > MAX_CAUSES
            || causes.iter().any(|(id, _)| drain.jobs.contains(id))
        {
            return Err("document consequences exceed the budget or are already running".into());
        }
        let (request, custody) = operation.with_custody();
        for (id, value) in causes {
            operation.check()?;
            {
                let mut completion = group.borrow_mut();
                completion.remaining = completion.remaining.saturating_add(1);
                completion.admitted = completion.admitted.saturating_add(1);
            }
            drain
                .caused
                .insert(id.clone(), (request.invocation, Rc::clone(group)));
            if let Err(error) =
                drain.start_entry(id.clone(), value, Some(request.invocation), custody)
                && let Some((_, group)) = drain.caused.remove(&id)
            {
                group.borrow_mut().finished(Err(failure(error)));
            }
        }
        Ok(())
    }

    fn poll(&mut self) {
        let revision = self.borrow().revision;
        super::Drainer::poll(&mut *self.borrow_mut(), revision);
    }

    fn cancel(&mut self, invocation: InvocationId) {
        let mut drain = self.borrow_mut();
        let ids: Vec<tilo_resource::Id> = drain
            .caused
            .iter()
            .filter(|(_, (caused, _))| *caused == invocation)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            drain.jobs.cancel(&id);
            if let Some((_, group)) = drain.caused.remove(&id) {
                group
                    .borrow_mut()
                    .finished(Err(failure("document consequence cancelled".into())));
            }
            // Abandoned by whoever it was for: it is not carried out later
            // as nobody's.
            let value = (drain.abandoned)();
            let _ = drain.finish(
                &id,
                Ok(ActionOutput {
                    value,
                    resources: Vec::new(),
                }),
            );
        }
    }

    fn retire(&mut self) {
        let invocations: HashSet<InvocationId> = self
            .borrow()
            .caused
            .values()
            .map(|(invocation, _)| *invocation)
            .collect();
        for invocation in invocations {
            CausalDrainer::cancel(self, invocation);
        }
    }
}
