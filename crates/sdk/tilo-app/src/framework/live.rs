//! The resource state views read, refreshed whenever the document changes.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use tilo_resource::Resource;

struct Inner<R: Resource> {
    state: RefCell<Rc<R::State>>,
    revision: Cell<u64>,
}

/// A shared handle to the current state of one resource.
///
/// The framework replaces the state when the document's heads move — a local
/// action, a collaborator's edit, an undo — and bumps the revision so bound
/// widgets refresh. Views never write it: edits are actions.
pub struct Live<R: Resource> {
    inner: Rc<Inner<R>>,
}

impl<R: Resource> Clone for Live<R> {
    fn clone(&self) -> Self {
        Self {
            inner: Rc::clone(&self.inner),
        }
    }
}

impl<R: Resource> Live<R> {
    pub fn new(state: R::State) -> Self {
        Self {
            inner: Rc::new(Inner {
                state: RefCell::new(Rc::new(state)),
                revision: Cell::new(0),
            }),
        }
    }

    /// The current state. Cheap: shares the materialised value.
    pub fn get(&self) -> Rc<R::State> {
        Rc::clone(&self.inner.state.borrow())
    }

    /// Read the current state.
    pub fn with<T>(&self, read: impl FnOnce(&R::State) -> T) -> T {
        read(&self.inner.state.borrow())
    }

    /// Increases whenever the state is replaced.
    pub fn revision(&self) -> u64 {
        self.inner.revision.get()
    }

    /// Ask everything bound to the state to take it again, unchanged. After
    /// an edit was refused or is waiting for confirmation, a surface that
    /// previewed it optimistically is re-presented the stored state.
    pub(crate) fn represent(&self) {
        self.inner
            .revision
            .set(self.inner.revision.get().wrapping_add(1));
    }

    pub(crate) fn replace(&self, state: R::State) {
        *self.inner.state.borrow_mut() = Rc::new(state);
        self.inner
            .revision
            .set(self.inner.revision.get().wrapping_add(1));
    }
}
