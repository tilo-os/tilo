//! The resource state views read, kept current as the document changes.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_resource::{Demand, Doc, ModelError, Reader, Resource};

/// A shared handle to the current state of one resource.
///
/// The framework brings the state up to date when the document's heads move
/// — a local action, a collaborator's edit, an undo — reading only what the
/// change touched ([`Resource::refresh`]), and bumps the revision so bound
/// widgets refresh. Views never write it: edits are actions.
///
/// A view of a large resource says what it shows with
/// [`demand`](Self::demand), and only that is read
/// ([`Resource::load`]).
pub struct Live<R: Resource> {
    inner: Rc<RefCell<Reader<R>>>,
}

impl<R: Resource> Clone for Live<R> {
    fn clone(&self) -> Self {
        Self {
            inner: Rc::clone(&self.inner),
        }
    }
}

impl<R: Resource> Live<R> {
    pub(crate) fn new(reader: Reader<R>) -> Self {
        Self {
            inner: Rc::new(RefCell::new(reader)),
        }
    }

    /// The current state. Cheap: shares the materialised value.
    ///
    /// Keep it only as long as a frame needs it. A state still shared when
    /// the document next changes cannot be updated in place, so the
    /// framework reads the whole resource again instead; read through
    /// [`with`](Self::with) or call this again when you need it.
    pub fn get(&self) -> Rc<R::State> {
        Rc::clone(self.inner.borrow().state())
    }

    /// Read the current state.
    pub fn with<T>(&self, read: impl FnOnce(&R::State) -> T) -> T {
        read(self.inner.borrow().state())
    }

    /// Increases whenever the state changes.
    pub fn revision(&self) -> u64 {
        self.inner.borrow().revision()
    }

    /// Show the parts of a large resource `demand` names, reading those not
    /// read yet from `doc` — the document the view was synced with, which in
    /// History mode is the past version on screen. Returns whether the state
    /// changed.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document does not hold the resource.
    pub fn demand(&self, doc: &Doc, demand: Demand) -> Result<bool, ModelError>
    where
        R::State: Clone,
    {
        self.inner.borrow_mut().demand(doc, demand)
    }

    /// Ask everything bound to the state to take it again, unchanged. After
    /// an edit was refused or is waiting for confirmation, a surface that
    /// previewed it optimistically is re-presented the stored state.
    pub(crate) fn represent(&self) {
        self.inner.borrow_mut().represent();
    }

    /// What views currently demand.
    pub fn current_demand(&self) -> Demand {
        self.inner.borrow().current_demand().clone()
    }

    /// Apply what changed in `doc` since the last sync, reading only what
    /// changed. The framework does this before every frame; a surface calls
    /// it after an immediate edit ([`Dispatcher::apply_now`](super::Dispatcher::apply_now))
    /// to read its own result within the same event. Returns whether the
    /// state changed.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document no longer holds the resource.
    pub fn sync(&self, doc: &mut Doc) -> Result<bool, ModelError> {
        self.inner.borrow_mut().sync(doc)
    }
}
