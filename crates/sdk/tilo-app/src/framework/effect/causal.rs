//! Opt-in projections caused by the current admitted document action.

use super::{
    ActionFailure, ActionOutput, EffectAnswer, EffectCx, EffectEnv, Effected, Handle, InvocationId,
    InvocationUpdate, Pending, Polled, ResourceOperation, ResourceOperationStart, Running,
    answer_under, failed, failure,
};
use std::any::Any;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::rc::{Rc, Weak};
use tilo_resource::Resource;
use ui_core::invocation_scope::ResourceInvocationScope;
use ui_core::widget::document_effects::AppliedDocument;

const MAX_CAUSES: usize = 256;
const MAX_INVOCATIONS: usize = 128;

struct Completion {
    invocation: InvocationId,
    remaining: usize,
    admitted: usize,
    output: ActionOutput,
    error: Option<ActionFailure>,
    custody: Option<ResourceInvocationScope>,
}

impl Completion {
    fn finished(&mut self, result: Result<ActionOutput, ActionFailure>) {
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

type Group = Rc<RefCell<Completion>>;

/// A causal drain, type-erased: `before` and `after` are its resource's
/// state.
trait CausalDrainer {
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

struct CausalDrain<R: Resource, K, T> {
    env: EffectEnv<R>,
    pending: Pending<R, K, T>,
    run: Handle<R, T>,
    jobs: Running<R, (), K>,
    groups: HashMap<K, (InvocationId, Group)>,
}

impl<R: Resource, K: Eq + Hash + Clone + 'static, T: 'static> CausalDrainer
    for CausalDrain<R, K, T>
{
    fn start(
        &mut self,
        before: &dyn Any,
        after: &dyn Any,
        operation: &ResourceOperation<'_>,
        group: &Group,
    ) -> Result<(), String> {
        let (Some(before), Some(after)) = (
            before.downcast_ref::<R::State>(),
            after.downcast_ref::<R::State>(),
        ) else {
            return Err("a document consequence read another resource's state".into());
        };
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
                .with_source_invocation(operation.source_invocation())
                .with_custody(custody.cloned())?;
            {
                let mut completion = group.borrow_mut();
                completion.remaining = completion.remaining.saturating_add(1);
                completion.admitted = completion.admitted.saturating_add(1);
            }
            let run = &mut self.run;
            let mut entry = Some(entry);
            let answer = answer_under(
                custody,
                Some(request.invocation),
                &mut || match entry.take() {
                    Some(entry) => run(entry, &mut cx),
                    None => Err("a document consequence ran twice".into()),
                },
            );
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

struct RegisteredCausal {
    alive: Weak<()>,
    drain: Box<dyn CausalDrainer>,
}

/// Every causal drain of one resource, type-erased so that admitting,
/// polling and settling them is compiled once in the shared runtime.
#[derive(Default)]
pub(super) struct CausalEffects {
    drains: Vec<RegisteredCausal>,
    groups: HashMap<InvocationId, Group>,
}

/// Reads a resource's state from a document, type-erased.
pub(super) type ReadState = dyn Fn(&tilo_resource::Doc) -> Result<Box<dyn Any>, String>;

impl CausalEffects {
    pub(super) fn register<R: Resource, K: Eq + Hash + Clone + 'static, T: 'static>(
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

    pub(super) fn registered(&self) -> bool {
        self.drains
            .iter()
            .any(|entry| entry.alive.strong_count() > 0)
    }

    pub(super) fn start(
        &mut self,
        operation: &ResourceOperation<'_>,
        applied: &AppliedDocument<'_>,
        read: &ReadState,
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
