//! Submitting edits: every change a view makes is an action invocation the
//! host resolves, checks against permissions, applies, records for undo and
//! history, and replicates.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use tilo_actions::{ActionAddress, ControlInvocation};
use tilo_resource::{Invoke, Resource};

/// Queues action invocations for the host. Cloneable; clones share one queue.
///
/// Nothing is applied here. The framework hands each invocation to the host
/// exactly as a control bound to an action would, so a custom surface's edit
/// takes the same permission-checked, undoable, attributed path as a button,
/// the palette, an agent or a script.
pub struct Dispatcher<R: Resource> {
    resource_id: Rc<str>,
    queue: Rc<RefCell<VecDeque<ControlInvocation>>>,
    read_only: bool,
    marker: std::marker::PhantomData<fn(R)>,
}

impl<R: Resource> Clone for Dispatcher<R> {
    fn clone(&self) -> Self {
        Self {
            resource_id: Rc::clone(&self.resource_id),
            queue: Rc::clone(&self.queue),
            read_only: self.read_only,
            marker: std::marker::PhantomData,
        }
    }
}

impl<R: Resource> Dispatcher<R> {
    pub(crate) fn new(resource_id: &str, read_only: bool) -> Self {
        Self {
            resource_id: Rc::from(resource_id),
            queue: Rc::default(),
            read_only,
            marker: std::marker::PhantomData,
        }
    }

    /// Whether edits from this participant will be refused. Surfaces use it
    /// to present themselves read-only instead of letting an edit bounce.
    pub fn read_only(&self) -> bool {
        self.read_only
    }

    /// Invoke a declared action (`dispatcher.invoke(Note::EDIT.with(text))`).
    pub fn invoke(&self, invoke: Invoke<R>) {
        if self.read_only {
            return;
        }
        self.queue.borrow_mut().push_back(ControlInvocation {
            address: ActionAddress::new(self.resource_id.as_ref(), invoke.id),
            arguments: invoke.arguments,
        });
    }

    /// Submit one typed action value — an edit with no named declaration,
    /// such as a drag's final geometry.
    pub fn apply(&self, action: &R::Action) {
        match Invoke::<R>::apply(action) {
            Ok(invoke) => self.invoke(invoke),
            Err(error) => tracing::warn!(%error, "action does not serialize; not submitted"),
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
