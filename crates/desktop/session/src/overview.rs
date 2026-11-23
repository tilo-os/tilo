//! Whether the overview is the surface up: the shell draws it, and the session's surfaces and the
//! shell's state read the one flag, which turns with the surface itself.

use std::cell::Cell;
use std::rc::Rc;

/// Whether the overview is open, shared by the session's surfaces and whoever reads the shell's
/// state.
#[derive(Clone, Default)]
pub(crate) struct Overview {
    open: Rc<Cell<bool>>,
}

impl Overview {
    /// The flag, shared.
    pub(crate) fn open_flag(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.open)
    }

    /// The same flag, borrowed: for a reader that lives for one call.
    pub(crate) fn open_cell(&self) -> &Cell<bool> {
        &self.open
    }
}
