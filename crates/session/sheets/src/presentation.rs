//! Whether a sheet is up: what the shell hosting a sheet and the sheet
//! share. The shell opens it and may take it down; the sheet says when it
//! dismissed itself and when nothing of it is left on screen.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ui::motion::Appearance;
use ui::tokens::MotionSource;
use ui::vello::kurbo::Rect;
use ui::widget::{ModalDialog, Widget};

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

    /// Where the displays are that the sheet stands on, in the sheet's own
    /// coordinates: told to a sheet that covers every display at once, so it
    /// can draw something on each of them.
    fn covers(&self, _displays: Vec<Rect>) {}
}

/// One sheet's presentation, shared by the shell and the sheet's app.
#[derive(Clone, Default)]
pub struct Presentation(Rc<Inner>);

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
    /// The sheet stands on every display at once rather than on one: what
    /// it draws has to reach each of them.
    every_display: Cell<bool>,
    /// Wakes the sheet's app when the shell changes the phase.
    wake: RefCell<Option<ui::WakeHandle>>,
    /// The motion the card moves on: the person's setting, which the shell
    /// says when it mounts the sheet. No draw of the card has happened yet
    /// when it first arrives, so nothing else could.
    motion: Cell<ui::tokens::EffectiveMotion>,
    /// The card's arrival and leaving, kept here rather than in the app's
    /// tree: the app builds a new tree whenever its state changes, and a
    /// card that took its motion with the tree would replay its arrival on
    /// every rebuild, and drop out of sight on the rebuild that closed it.
    appear: Rc<Cell<ui::motion::Appearing>>,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            phase: Cell::default(),
            openings: Cell::default(),
            by_keyboard: Cell::default(),
            place: Cell::default(),
            every_display: Cell::default(),
            wake: RefCell::default(),
            motion: Cell::new(ui::tokens::EffectiveMotion::default()),
            appear: Rc::new(Cell::new(ui::motion::Appearing::closed())),
        }
    }
}

impl Presentation {
    /// The card's arrival and leaving, which the sheet's dialog draws with
    /// ([`presented`]): one handle for every tree the sheet's app builds.
    #[must_use]
    pub fn appearance(&self) -> Appearance {
        Appearance::Shared(Rc::clone(&self.0.appear))
    }

    /// Move the card towards `open` as `source` asked, on the motion the
    /// person allows (reduced motion only fades it).
    fn show(&self, open: bool, source: MotionSource) {
        let mut appear = self.0.appear.get();
        appear.follow_from(open, self.0.motion.get(), source);
        self.0.appear.set(appear);
    }

    /// Move the card on `motion`: what the shell says when it mounts the
    /// sheet, before the card has ever been drawn to read it from its theme.
    pub fn set_motion(&self, motion: ui::tokens::EffectiveMotion) {
        self.0.motion.set(motion);
    }

    /// The shell puts the sheet up; `by_keyboard` when a key asked.
    pub fn open(&self, by_keyboard: bool) {
        self.0.phase.set(Phase::Open);
        self.0.openings.set(self.0.openings.get().wrapping_add(1));
        self.0.by_keyboard.set(by_keyboard);
        let source = if by_keyboard {
            MotionSource::Keyboard
        } else {
            MotionSource::Pointer
        };
        // Its card arrives with the shell's own answer, however long ago the
        // app built the tree that draws it: a key that asked lands at once,
        // a press moves it.
        self.show(true, source);
        self.wake();
    }

    /// The shell takes the sheet down. `true` when it was up.
    pub fn close(&self) -> bool {
        let open = self.is_open();
        if open {
            self.0.phase.set(Phase::Leaving);
            self.show(false, MotionSource::Pointer);
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

    /// The sheet stands on every display at once: the shell spans it over
    /// all of them and says where each one is ([`Seat::covers`]). A
    /// question that has to be seen may be about the display it would
    /// otherwise be put on.
    pub fn cover_every_display(&self) {
        self.0.every_display.set(true);
    }

    /// Whether the sheet stands on every display at once.
    pub fn covers_every_display(&self) -> bool {
        self.0.every_display.get()
    }

    /// The sheet dismissed itself (Escape, a press outside it, a choice).
    /// Its app animates it out and calls [`Self::gone`] once nothing of it is
    /// drawn.
    pub fn dismissed(&self) {
        if self.is_open() {
            self.0.phase.set(Phase::Leaving);
            self.show(false, MotionSource::Pointer);
            self.wake();
        }
    }

    /// Nothing of the sheet is drawn any more.
    pub fn gone(&self) {
        if self.phase() != Phase::Closed {
            self.0.phase.set(Phase::Closed);
            self.wake();
        }
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

/// The sheet's card, `dialog`, as `presentation` shows it: up while the
/// shell has the sheet open, arriving once on the presentation's own
/// appearance however often the app rebuilds its tree, and leaving until it
/// has gone rather than vanishing on the rebuild that closed it.
///
/// `None` is a sheet nothing lends a presentation to — a capture, a test's
/// fixture — which stands open at rest.
pub fn presented<C: Widget + 'static>(
    presentation: Option<&Presentation>,
    dialog: ModalDialog<C>,
) -> ModalDialog<C> {
    let Some(presentation) = presentation else {
        return dialog.open(true);
    };
    // Its card is up while the presentation says so, wherever it is in its
    // arrival or leaving: the handle carries that across every tree the
    // app builds, so a rebuild never closes or replays it.
    dialog.appearing(presentation.appearance())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "in a test this is the assertion, not a hazard"
    )]
    use super::*;

    /// The card moves on the motion the shell says: the person's reduced
    /// motion takes the travel out of its arrival, before any frame of the
    /// sheet has drawn to read a theme.
    #[test]
    fn the_card_moves_on_the_motion_the_shell_says() {
        let presentation = Presentation::default();
        presentation.set_motion(ui::Theme::dark().with_reduce_motion(true).motion());
        presentation.open(false);
        let appearance = presentation.appearance();
        assert!(appearance.is_moving(), "it arrives");
        let pose = appearance.pose(
            ui::vello::kurbo::Rect::new(0.0, 0.0, 400.0, 300.0),
            ui::vello::kurbo::Point::new(400.0, 0.0),
        );
        assert_eq!(
            pose.transform,
            ui::vello::kurbo::Affine::IDENTITY,
            "reduced motion: it does not travel"
        );
    }
}
