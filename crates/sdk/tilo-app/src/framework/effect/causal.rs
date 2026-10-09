//! Opt-in projections caused by the current admitted document action.

use super::{
    ActingFor, ActionFailure, ActionOutput, EffectAnswer, EffectCx, EffectEnv, Effected, Handle,
    InvocationId, InvocationUpdate, Pending, Polled, ResourceOperation, ResourceOperationStart,
    Running, failed, failure,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::rc::{Rc, Weak};
use tilo_resource::Resource;
use ui_core::invocation_scope::ResourceInvocationScope;
use ui_core::widget::document_effects::AppliedDocument;

const MAX_CAUSES: usize = 256;
const MAX_INVOCATIONS: usize = 128;

pub(super) struct Completion {
    invocation: InvocationId,
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

pub(super) trait CausalDrainer<R: Resource> {
    fn start(
        &mut self,
        before: &R::State,
        after: &R::State,
        operation: &ResourceOperation<'_>,
        group: &Group,
    ) -> Result<(), String>;
    fn poll(&mut self);
    fn cancel(&mut self, invocation: InvocationId);
    fn retire(&mut self);
}

struct CausalDrain<R: Resource, K, T> {
    env: EffectEnv<R>,
    pending: Pending<R, K, T>,
    run: Handle<R, T>,
    jobs: Running<R, (), K>,
    groups: HashMap<K, (InvocationId, Group)>,
}

impl<R: Resource, K: Eq + Hash + Clone + 'static, T: 'static> CausalDrainer<R>
    for CausalDrain<R, K, T>
{
    fn start(
        &mut self,
        before: &R::State,
        after: &R::State,
        operation: &ResourceOperation<'_>,
        group: &Group,
    ) -> Result<(), String> {
        let previous: HashSet<K> = (self.pending)(before)
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        let causes: Vec<_> = (self.pending)(after)
            .into_iter()
            .filter(|(key, _)| !previous.contains(key))
            .collect();
        let unique: HashSet<&K> = causes.iter().map(|(key, _)| key).collect();
        if causes.len().saturating_add(group.borrow().admitted) > MAX_CAUSES
            || unique.len() != causes.len()
            || causes.iter().any(|(key, _)| self.jobs.contains(key))
        {
            return Err("document consequences exceed the budget or are already running".into());
        }
        let (request, custody) = operation.with_custody();
        for (key, entry) in causes {
            operation.check()?;
            let mut cx = EffectCx::new(self.env.clone(), Some(request.invocation))
                .with_custody(custody.cloned())?;
            {
                let mut completion = group.borrow_mut();
                completion.remaining = completion.remaining.saturating_add(1);
                completion.admitted = completion.admitted.saturating_add(1);
            }
            let answer = {
                let _acting = ActingFor::enter(Some(request.invocation));
                match custody {
                    Some(custody) => custody
                        .with_effect(|| (self.run)(entry, &mut cx))
                        .and_then(|answer| custody.check().map(|()| answer)),
                    None => (self.run)(entry, &mut cx),
                }
            };
            match (answer, cx.started.take()) {
                (Ok(Effected::Pending), Some(started)) => {
                    self.jobs.insert(key.clone(), started);
                    self.groups
                        .insert(key, (request.invocation, Rc::clone(group)));
                }
                (answer, started) => {
                    if let Some(started) = started {
                        started.stop();
                    }
                    let result = match answer {
                        Ok(Effected::Done(output)) => Ok(output),
                        Ok(Effected::Pending) => {
                            Err(failure("document consequence has no job".into()))
                        }
                        Err(error) => Err(failure(error)),
                    };
                    group.borrow_mut().finished(result);
                }
            }
        }
        Ok(())
    }

    fn poll(&mut self) {
        for key in self.jobs.keys() {
            let Some((invocation, group)) = self.groups.get(&key).cloned() else {
                continue;
            };
            while let Some(polled) = self.jobs.poll(&key, &mut (), &self.env, Some(invocation)) {
                if let Polled::Finished(result) = polled {
                    if let Some(result) = result {
                        group.borrow_mut().finished(result);
                        self.groups.remove(&key);
                    }
                    break;
                }
            }
        }
    }

    fn cancel(&mut self, invocation: InvocationId) {
        let keys: Vec<K> = self
            .groups
            .iter()
            .filter(|(_, (_, group))| group.borrow().invocation == invocation)
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            self.jobs.cancel(&key);
            if let Some((_, group)) = self.groups.remove(&key) {
                group
                    .borrow_mut()
                    .finished(Err(failure("document consequence cancelled".into())));
            }
        }
    }

    fn retire(&mut self) {
        let invocations: HashSet<InvocationId> = self
            .groups
            .values()
            .map(|(_, group)| group.borrow().invocation)
            .collect();
        for invocation in invocations {
            self.cancel(invocation);
        }
    }
}

struct RegisteredCausal<R: Resource> {
    alive: Weak<()>,
    drain: Box<dyn CausalDrainer<R>>,
}

pub(super) struct CausalEffects<R: Resource> {
    drains: Vec<RegisteredCausal<R>>,
    groups: HashMap<InvocationId, Group>,
}

impl<R: Resource> Default for CausalEffects<R> {
    fn default() -> Self {
        Self {
            drains: Vec::new(),
            groups: HashMap::new(),
        }
    }
}

impl<R: Resource> CausalEffects<R> {
    pub(super) fn register<K: Eq + Hash + Clone + 'static, T: 'static>(
        &mut self,
        alive: Weak<()>,
        env: EffectEnv<R>,
        pending: impl Fn(&R::State) -> Vec<(K, T)> + 'static,
        run: impl FnMut(T, &mut EffectCx<R>) -> EffectAnswer + 'static,
    ) {
        self.drains.push(RegisteredCausal {
            alive,
            drain: Box::new(CausalDrain {
                env,
                pending: Box::new(pending),
                run: Box::new(run),
                jobs: Running::default(),
                groups: HashMap::new(),
            }),
        });
    }

    pub(super) fn register_drainer(&mut self, alive: Weak<()>, drain: Box<dyn CausalDrainer<R>>) {
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
        let before = R::read(&before_doc).map_err(|error| error.to_string())?;
        let after = R::read(applied.document()).map_err(|error| error.to_string())?;
        let group = Rc::new(RefCell::new(Completion {
            invocation: applied.invocation(),
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
            if let Err(error) = entry.drain.start(&before, &after, operation, &group) {
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
impl<R, T, O> CausalDrainer<R> for super::SharedOutbox<R, T, O>
where
    R: Resource<State = R> + tilo_resource::Model + 'static,
    T: tilo_resource::Model + Clone + 'static,
    O: serde::Serialize + serde::de::DeserializeOwned + Default + 'static,
{
    fn start(
        &mut self,
        before: &R::State,
        after: &R::State,
        operation: &ResourceOperation<'_>,
        group: &Group,
    ) -> Result<(), String> {
        let mut drain = self.0.borrow_mut();
        let previous: HashSet<tilo_resource::Id> =
            drain.owned(before).into_iter().map(|(id, _)| id).collect();
        let causes: Vec<_> = drain
            .owned(after)
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
        let revision = self.0.borrow().revision;
        super::Drainer::poll(&mut *self.0.borrow_mut(), revision);
    }

    fn cancel(&mut self, invocation: InvocationId) {
        let mut drain = self.0.borrow_mut();
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
        }
    }

    fn retire(&mut self) {
        let invocations: HashSet<InvocationId> = self
            .0
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
