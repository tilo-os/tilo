//! The shortcut guide in the session: the sheet over the desktop that names
//! every key the desktop answers, the chords each answers to now, and how to
//! leave it.
//!
//! The guide explains one table, the one the session's keys resolve against
//! ([`crate::keys::SessionKeys`]); it is handed the same table rather than a
//! copy, so a chord the person changes reads as changed here too. The guide
//! itself is the sheet app's ([`tilo_sheets::shortcut_guide`]): this module
//! shows it, keeps what it shows current, and answers what it asks.

use std::cell::RefCell;
use std::rc::Rc;

use settings_store::hardware::ShortcutConfig;
use tilo_sheets::Seat as _;
use tilo_sheets::shortcut_guide::{Guide, GuideEffect, GuideSeat};

/// The guide's sheet, and the shortcut table it explains.
pub(crate) struct ShortcutGuide {
    /// The sheet's seat; `None` when the sheet did not mount.
    seat: Option<GuideSeat>,
    /// The table the session answers keys with, shared with the key surface.
    table: Rc<RefCell<ShortcutConfig>>,
    /// The table the guide was last built from, and that guide: it is built
    /// again only when the table it explains changed.
    built: RefCell<Option<(ShortcutConfig, Guide)>>,
}

impl ShortcutGuide {
    pub(crate) fn new(seat: Option<GuideSeat>, table: Rc<RefCell<ShortcutConfig>>) -> Self {
        Self {
            seat,
            table,
            built: RefCell::new(None),
        }
    }

    /// The guide's sheet's seat: whether it is up, where it sits, and what it
    /// shows; `None` when the sheet did not mount.
    pub(crate) fn seat(&self) -> Option<&GuideSeat> {
        self.seat.as_ref()
    }

    /// The guide's presentation: where it sits; `None` when the sheet did
    /// not mount.
    pub(crate) fn presentation(&self) -> Option<&tilo_sheets::Presentation> {
        self.seat.as_ref().map(tilo_sheets::Seat::presentation)
    }

    /// The person asked for the guide, or asked for it to go: put it up with
    /// the keys as they are, or take it down.
    pub(crate) fn toggle(&self) {
        let Some(seat) = &self.seat else {
            return;
        };
        if seat.presentation().is_open() {
            self.close();
        } else {
            self.refresh();
            seat.presentation().open(true);
        }
    }

    /// Take the guide down: it is asked for no longer.
    ///
    /// Closing alone would leave the last frame of the guide on screen, drawn
    /// until the app is told it is gone.
    fn close(&self) {
        if let Some(seat) = &self.seat {
            seat.presentation().close();
            seat.presentation().gone();
        }
    }

    /// Carry out what the person chose on the guide, and keep it showing the
    /// table as it stands.
    pub(crate) fn follow(&self) {
        if let Some(seat) = &self.seat {
            for effect in seat.take_asked() {
                match effect {
                    GuideEffect::Done => self.close(),
                    other => {
                        tracing::debug!(?other, "shortcut the guide cannot change from here yet");
                    }
                }
            }
        }
        self.refresh();
    }

    /// Tell the guide what the table holds, when that changed since it was
    /// last told.
    fn refresh(&self) {
        let Some(seat) = &self.seat else {
            return;
        };
        let table = self.table.borrow();
        if self
            .built
            .borrow()
            .as_ref()
            .is_some_and(|(built, _)| built == &*table)
        {
            return;
        }
        let guide = tilo_sheets::shortcut_guide::keyboard_guide(&table);
        seat.show(guide.clone());
        *self.built.borrow_mut() = Some((table.clone(), guide));
    }
}
