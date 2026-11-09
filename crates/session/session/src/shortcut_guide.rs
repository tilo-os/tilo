//! The shortcut guide in the session: the sheet over the desktop that names every key the desktop
//! answers, the chords each answers to now, and how to leave it.
//! The guide explains one table, the one the session's keys resolve against
//! ([`crate::keys::SessionKeys`]); it is handed the same table rather than a copy, so a chord the
//! person changes reads as changed here too. The guide itself is the sheet app's
//! ([`tilo_sheets::shortcut_guide`]): this module shows it and answers what it asks.

use std::rc::Rc;

use settings_store::hardware::{ActionChord, ShortcutConfig};
use tilo_sheets::Seat as _;
use tilo_sheets::shortcut_guide::{GuideEffect, GuideSeat, State};

/// The guide's sheet, its state, and the apps' shortcuts that step aside for its table.
pub(crate) struct ShortcutGuide {
    /// The sheet's seat; `None` when the sheet did not mount.
    seat: Option<GuideSeat>,
    /// What the guide explains and waits for, shared with the key surface.
    state: Rc<State>,
}

impl ShortcutGuide {
    pub(crate) fn new(seat: Option<GuideSeat>, state: Rc<State>) -> Self {
        Self { seat, state }
    }

    /// The guide's sheet's seat: whether it is up, where it sits, and what it shows; `None` when
    /// the sheet did not mount.
    pub(crate) fn seat(&self) -> Option<&GuideSeat> {
        self.seat.as_ref()
    }

    /// The guide's presentation: where it sits; `None` when the sheet did not mount.
    pub(crate) fn presentation(&self) -> Option<&tilo_sheets::Presentation> {
        self.seat.as_ref().map(tilo_sheets::Seat::presentation)
    }

    /// The person asked for the guide, or asked for it to go: put it up with the keys as they are,
    /// or take it down.
    pub(crate) fn toggle(&self) {
        let Some(seat) = &self.seat else {
            return;
        };
        if self.state.toggle() {
            seat.show(self.state.shaped());
            seat.presentation().open(true);
        } else {
            seat.presentation().close();
        }
    }

    /// Carry out what the person chose on the guide, and keep it showing the table as it stands.
    pub(crate) fn follow(&self) {
        let Some(seat) = &self.seat else {
            return;
        };
        for effect in seat.take_asked() {
            let done = effect == GuideEffect::Done;
            self.state.effect(effect);
            if done {
                // Its own close: the card leaves the screen, and the shell tells it gone once
                // nothing of it is drawn.
                seat.presentation().close();
            }
        }
        self.state.follow();
        if self.state.up() {
            seat.show(self.state.shaped());
        }
    }

    /// Let the table follow `chords` , the keys the armed shortcuts answer: the keys claim them,
    /// the guide lists what each runs, and the apps' shortcuts step aside for them. Nothing is
    /// written to the person's settings: the chords are the shortcuts' own, taken from the
    /// workspaces they are saved in, and the table is what shows them.
    pub(crate) fn follow_trigger_chords(&self, chords: Vec<ActionChord>) {
        let mut table = self.state.table().borrow().clone();
        if !tilo_action_broker::automation::replace_trigger_chords(&mut table, chords) {
            return;
        }
        self.state.set_table(table);
    }
}

/// The apps' global shortcuts, told the table the desktop answers.
pub(crate) struct PortalShortcuts(pub(crate) tilo_portal::global_shortcuts::Shortcuts);

impl tilo_sheets::shortcut_guide::AppShortcuts for PortalShortcuts {
    fn set_shortcut_table(&self, table: ShortcutConfig) {
        self.0.set_shortcut_table(table);
    }
}
