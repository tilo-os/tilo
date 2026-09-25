//! Submitting edits: every change a view makes is an action invocation the
//! host resolves, checks against permissions, applies, records for undo and
//! history, and replicates.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use tilo_actions::{ActionAddress, ControlInvocation};
use tilo_resource::{Invoke, Resource};
use ui_core::LocalState;

/// Queues action invocations for the host. Cloneable; clones share one queue.
///
/// Nothing is applied here. The framework hands each invocation to the host
/// exactly as a control bound to an action would, so a custom surface's edit
/// takes the same permission-checked, undoable, attributed path as a button,
/// the palette, an agent or a script.
pub struct Dispatcher<R: Resource> {
    resource_id: Rc<str>,
    queue: Rc<RefCell<VecDeque<ControlInvocation>>>,
    /// The invocations of the open group, and how many groups are open.
    group: Rc<RefCell<(usize, Vec<Invoke<R>>)>>,
    read_only: LocalState<bool>,
    marker: std::marker::PhantomData<fn(R)>,
}

impl<R: Resource> Clone for Dispatcher<R> {
    fn clone(&self) -> Self {
        Self {
            resource_id: Rc::clone(&self.resource_id),
            queue: Rc::clone(&self.queue),
            group: Rc::clone(&self.group),
            read_only: self.read_only.clone(),
            marker: std::marker::PhantomData,
        }
    }
}

impl<R: Resource> Dispatcher<R> {
    pub(crate) fn new(resource_id: &str, read_only: bool) -> Self {
        Self {
            resource_id: Rc::from(resource_id),
            queue: Rc::default(),
            group: Rc::default(),
            read_only: LocalState::new(read_only),
            marker: std::marker::PhantomData,
        }
    }

    /// Whether edits from this participant will be refused, as of now: it
    /// follows role changes while the app runs. Surfaces use it to present
    /// themselves read-only instead of letting an edit bounce.
    pub fn read_only(&self) -> bool {
        self.read_only.get()
    }

    /// [`Self::read_only`] as observable state, for a widget that shows or
    /// disables something while the participant may only view.
    pub fn read_only_state(&self) -> LocalState<bool> {
        self.read_only.clone()
    }

    /// Record a change of the participant's permissions. Returns whether
    /// anything changed.
    pub(crate) fn set_read_only(&self, read_only: bool) -> bool {
        if self.read_only.get() == read_only {
            return false;
        }
        self.read_only.set(read_only);
        true
    }

    /// Invoke a declared action (`dispatcher.invoke(Note::EDIT.with(text))`).
    pub fn invoke(&self, invoke: Invoke<R>) {
        if self.read_only() {
            return;
        }
        {
            let mut group = self.group.borrow_mut();
            if group.0 > 0 {
                group.1.push(invoke);
                return;
            }
        }
        self.submit(invoke);
    }

    fn submit(&self, invoke: Invoke<R>) {
        self.queue.borrow_mut().push_back(ControlInvocation {
            address: ActionAddress::new(self.resource_id.as_ref(), invoke.id),
            arguments: invoke.arguments,
        });
    }

    /// Invoke an action published on a nested resource, such as one item of
    /// a collection.
    pub(crate) fn invoke_at(
        &self,
        address: ActionAddress,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) {
        if self.read_only() {
            return;
        }
        self.queue
            .borrow_mut()
            .push_back(ControlInvocation { address, arguments });
    }

    /// Submit one typed action value — an edit with no named declaration,
    /// such as a drag's final geometry.
    pub fn apply(&self, action: &R::Action) {
        match Invoke::<R>::apply(action) {
            Ok(invoke) => self.invoke(invoke),
            Err(error) => tracing::warn!(%error, "action does not serialize; not submitted"),
        }
    }

    /// Submit every edit `edits` makes through this dispatcher (or a clone
    /// of it) as one action: one change, one version in history, one undo
    /// step. For a gesture that is several edits — split a linked clip pair,
    /// delete a selection — which the user takes back with one Ctrl+Z.
    ///
    /// One action is one undo step by default; a group is the explicit
    /// exception. Groups nest: the outermost one submits. A group of one
    /// edit submits that edit unchanged.
    pub fn group<T>(&self, edits: impl FnOnce() -> T) -> T {
        let _group = self.begin_group();
        edits()
    }

    /// Open a group that submits when the returned guard drops: the
    /// [`group`](Self::group) for code whose edits are spread over a scope,
    /// such as one input event of a custom surface.
    #[must_use = "the group submits when this guard drops"]
    pub fn begin_group(&self) -> DispatchGroup<R> {
        let mut group = self.group.borrow_mut();
        group.0 = group.0.saturating_add(1);
        drop(group);
        DispatchGroup {
            dispatcher: self.clone(),
        }
    }

    fn end_group(&self) {
        let members = {
            let mut group = self.group.borrow_mut();
            group.0 = group.0.saturating_sub(1);
            if group.0 > 0 {
                return;
            }
            std::mem::take(&mut group.1)
        };
        let mut members = members.into_iter();
        match (members.next(), members.next()) {
            (None, _) => {}
            (Some(only), None) => self.submit(only),
            (Some(first), Some(second)) => {
                self.submit(Invoke::group([first, second].into_iter().chain(members)));
            }
        }
    }

    pub(crate) fn take(&self) -> Option<ControlInvocation> {
        self.queue.borrow_mut().pop_front()
    }

    pub(crate) fn has_pending(&self) -> bool {
        !self.queue.borrow().is_empty()
    }

    pub(crate) fn resource_id(&self) -> &str {
        &self.resource_id
    }
}

/// An open [`Dispatcher::begin_group`]: every edit submitted until it drops
/// becomes one action.
pub struct DispatchGroup<R: Resource> {
    dispatcher: Dispatcher<R>,
}

impl<R: Resource> Drop for DispatchGroup<R> {
    fn drop(&mut self) {
        self.dispatcher.end_group();
    }
}
