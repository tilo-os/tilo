//! Whether a sheet is up: what the shell hosting a sheet and the sheet
//! share. The shell opens it and may take it down; the sheet says when it
//! dismissed itself and when nothing of it is left on screen.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Where a sheet is in its life.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    /// Not on screen.
    #[default]
    Closed,
    /// On screen and taking input.
    Open,
    /// On its way out: still drawn, taking no input.
    Leaving,
}

/// What the shell lends a sheet's app: its presentation, and whatever else
/// that sheet takes from the shell and gives back.
pub trait Seat: Clone + Default + 'static {
    fn presentation(&self) -> &Presentation;
}

/// One sheet's presentation, shared by the shell and the sheet's app.
#[derive(Clone, Default)]
pub struct Presentation(Rc<Inner>);

#[derive(Default)]
struct Inner {
    phase: Cell<Phase>,
    /// How many times the shell opened the sheet: the sheet shows each
    /// opening afresh.
    openings: Cell<u64>,
    /// The last opening came from a key, so the sheet starts on its first
    /// control.
    by_keyboard: Cell<bool>,
    /// The rectangle the shell wants the sheet on, when it named one: the
    /// output the person is working on, say.
    place: Cell<Option<(i32, i32, u32, u32)>>,
    /// Wakes the sheet's app when the shell changes the phase.
    wake: RefCell<Option<ui::WakeHandle>>,
}

impl Presentation {
    /// The shell puts the sheet up; `by_keyboard` when a key asked.
    pub fn open(&self, by_keyboard: bool) {
        self.0.phase.set(Phase::Open);
        self.0.openings.set(self.0.openings.get().wrapping_add(1));
        self.0.by_keyboard.set(by_keyboard);
        self.wake();
    }

    /// The shell takes the sheet down. `true` when it was up.
    pub fn close(&self) -> bool {
        let open = self.is_open();
        if open {
            self.0.phase.set(Phase::Leaving);
            self.wake();
        }
        open
    }

    pub fn phase(&self) -> Phase {
        self.0.phase.get()
    }

    /// Up and taking input.
    pub fn is_open(&self) -> bool {
        self.phase() == Phase::Open
    }

    /// Up or on its way out: something of it is drawn.
    pub fn is_drawn(&self) -> bool {
        self.phase() != Phase::Closed
    }

    /// Which opening the sheet shows now.
    pub fn openings(&self) -> u64 {
        self.0.openings.get()
    }

    pub fn by_keyboard(&self) -> bool {
        self.0.by_keyboard.get()
    }

    /// The sheet's app is woken through `wake` when the shell opens or
    /// closes it.
    pub fn wake_with(&self, wake: ui::WakeHandle) {
        *self.0.wake.borrow_mut() = Some(wake);
    }

    /// The shell puts the sheet at `place` (`None`: wherever it puts sheets
    /// of its own), in logical pixels.
    pub fn place_at(&self, place: Option<(i32, i32, u32, u32)>) {
        if self.0.place.get() == place {
            return;
        }
        self.0.place.set(place);
        self.wake();
    }

    /// Where the shell wants the sheet now: a rectangle of the display, or
    /// `None` for the sheet's own place.
    pub fn place(&self) -> Option<(i32, i32, u32, u32)> {
        self.0.place.get()
    }

    /// The sheet dismissed itself (Escape, a press outside it, a choice).
    pub fn dismissed(&self) {
        if self.is_open() {
            self.0.phase.set(Phase::Leaving);
        }
    }

    /// Nothing of the sheet is drawn any more.
    pub fn gone(&self) {
        self.0.phase.set(Phase::Closed);
    }

    /// Wake the sheet's app: something the shell lent it changed.
    pub fn wake(&self) {
        if let Some(wake) = self.0.wake.borrow().as_ref() {
            wake.wake();
        }
    }
}

impl std::fmt::Debug for Presentation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Presentation")
            .field("phase", &self.phase())
            .field("openings", &self.openings())
            .finish_non_exhaustive()
    }
}
