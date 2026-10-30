//! What the shell lends the overview: whether it is up, what it shows, the
//! pictures the compositor draws for it, and where what the person chose
//! goes.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use tilo_desktop::overview_panel::{OverviewDesk, OverviewPick};
use tilo_desktop::window_flight::WindowFlight;
use tilo_sheets::{Presentation, Seat};

/// The overview's side of the shell and the shell's side of the overview,
/// shared in one process.
#[derive(Clone, Default)]
pub struct OverviewSeat {
    presentation: Presentation,
    /// What the overview shows, and how many times it changed.
    desk: Rc<RefCell<(u64, OverviewDesk)>>,
    asked: Rc<RefCell<Vec<OverviewPick>>>,
    open_asked: Rc<Cell<bool>>,
    /// A strip's name is being typed: every key is the overview's.
    typing: Rc<Cell<bool>>,
    flight: WindowFlight,
}

impl Seat for OverviewSeat {
    fn presentation(&self) -> &Presentation {
        &self.presentation
    }
}

impl OverviewSeat {
    /// The shell's side: the overview shows `desk` from now on.
    pub fn set_desk(&self, desk: OverviewDesk) {
        {
            let mut shared = self.desk.borrow_mut();
            shared.0 = shared.0.wrapping_add(1);
            shared.1 = desk;
        }
        self.presentation.wake();
    }

    /// What the overview shows, and its generation.
    pub fn desk(&self) -> (u64, OverviewDesk) {
        self.desk.borrow().clone()
    }

    /// The generation of what the overview shows.
    pub fn desk_generation(&self) -> u64 {
        self.desk.borrow().0
    }

    /// What was chosen since the shell last asked, oldest first.
    pub fn take_asked(&self) -> Vec<OverviewPick> {
        std::mem::take(&mut *self.asked.borrow_mut())
    }

    /// Whether the overview was asked to come up (by its Open action)
    /// since the shell last asked.
    pub fn take_open_asked(&self) -> bool {
        self.open_asked.take()
    }

    /// The pictures of the windows: the overview names them, the
    /// compositor draws them and keeps the clock.
    pub fn flight(&self) -> &WindowFlight {
        &self.flight
    }

    /// Whether the overview holds the keyboard whole (a strip's name is
    /// being typed), shell chords included.
    pub fn holds_keyboard(&self) -> bool {
        self.typing.get()
    }

    pub(crate) fn set_holds_keyboard(&self, typing: bool) {
        self.typing.set(typing);
    }

    pub(crate) fn ask(&self, pick: OverviewPick) {
        self.asked.borrow_mut().push(pick);
        self.presentation.wake();
    }

    pub(crate) fn ask_open(&self) {
        self.open_asked.set(true);
        self.presentation.wake();
    }
}

impl std::fmt::Debug for OverviewSeat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OverviewSeat")
            .field("presentation", &self.presentation)
            .field("desk", &self.desk_generation())
            .finish_non_exhaustive()
    }
}
