//! The shortcut guide in the session: the sheet over the desktop that names
//! every key the desktop answers, the chords each answers to now, and how to
//! leave it.
//!
//! The guide explains one table, the one the session's keys resolve against
//! ([`crate::keys::SessionKeys`]); it is handed the same table rather than a
//! copy, so a chord the person changes reads as changed here too. The guide
//! itself is the sheet app's ([`tilo_sheets::shortcut_guide`]): this module
//! shows it, keeps what it shows current, and answers what it asks: the new
//! keys of a shortcut, the pair's defaults back, the guide gone.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use settings_store::hardware::{KeyChord, ShortcutAction, ShortcutConfig, ShortcutConfigError};
use tilo_sheets::Seat as _;
use tilo_sheets::shortcut_guide::{Faces, Guide, GuideEffect, GuideSeat, Modality};
use ui::ControllerFamily;

use crate::controller_nav::{FaceRole, face_button};
use crate::keys::{Caught, GuideKeys};

/// What the guide last showed, as the guide was given it: the table, the
/// shortcut waiting for a chord, why the last change was refused, and the
/// controller the person last used, if any.
type Shown = (
    ShortcutConfig,
    Option<String>,
    Option<String>,
    Option<ControllerFamily>,
);

/// The guide's sheet, and the shortcut table it explains.
pub(crate) struct ShortcutGuide {
    /// The sheet's seat; `None` when the sheet did not mount.
    seat: Option<GuideSeat>,
    /// The table the session answers keys with, shared with the key surface.
    table: Rc<RefCell<ShortcutConfig>>,
    /// The guide's keys while it is up, shared with the key surface: what
    /// the guide waits for, and the chords pressed for it.
    keys: GuideKeys,
    /// The apps' global shortcuts: their chords step aside for the table the
    /// session answers, so a change has to reach them as it reaches the keys.
    portal: Option<tilo_portal::global_shortcuts::Shortcuts>,
    /// Why the last change was refused, shown while the guide waits for the
    /// keys it explains.
    message: RefCell<Option<String>>,
    /// The controller the person last drove the desktop with: the guide
    /// explains its buttons rather than the keyboard while it is the input
    /// in hand.
    controller: Rc<Cell<Option<ControllerFamily>>>,
    /// The table, shortcut and message the guide was last built from, and
    /// that guide: it is built again only when one of them changed.
    built: RefCell<Option<(Shown, Guide)>>,
}

impl ShortcutGuide {
    pub(crate) fn new(
        seat: Option<GuideSeat>,
        table: Rc<RefCell<ShortcutConfig>>,
        keys: GuideKeys,
        portal: Option<tilo_portal::global_shortcuts::Shortcuts>,
        controller: Rc<Cell<Option<ControllerFamily>>>,
    ) -> Self {
        Self {
            seat,
            table,
            keys,
            portal,
            message: RefCell::new(None),
            controller,
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
            self.keys.set_up(true);
            seat.presentation().open(true);
        }
    }

    /// Take the guide down: it is asked for no longer, and waits for no
    /// chord.
    ///
    /// Closing alone would leave the last frame of the guide on screen, drawn
    /// until the app is told it is gone.
    fn close(&self) {
        if let Some(seat) = &self.seat {
            seat.presentation().close();
            seat.presentation().gone();
        }
        self.keys.set_up(false);
        self.message.replace(None);
    }

    /// Carry out what the person chose on the guide, and keep it showing the
    /// table as it stands.
    pub(crate) fn follow(&self) {
        if let Some(seat) = &self.seat {
            for effect in seat.take_asked() {
                match effect {
                    GuideEffect::Done => self.close(),
                    GuideEffect::Listen { action } => {
                        self.message.replace(None);
                        self.keys.listen_for(&action);
                    }
                    GuideEffect::Reset { first, second } => self.reset_to_defaults(&first, &second),
                }
            }
        }
        for caught in self.keys.take_caught() {
            match caught {
                Caught::Cancelled => {
                    self.message.replace(None);
                }
                Caught::Chord(chord) => {
                    if let Some(action) = self.keys.listening() {
                        self.bind(&action, &chord);
                    }
                }
            }
        }
        self.refresh();
    }

    /// Give the shortcut `action` its keys: `chord`, once the table accepts
    /// it and the person's settings keep it.
    fn bind(&self, action: &str, chord: &KeyChord) {
        let Some(action) = ShortcutAction::from_id(action) else {
            return;
        };
        // A key that types a character needs a modifier: without one the
        // shortcut would run instead of the letter everywhere.
        if types_a_character(chord) {
            self.refuse(tilo_sheets::word("guide-rebind-needs-modifier"));
            return;
        }
        let mut candidate = self.table.borrow().clone();
        if let Err(error) = candidate.set(action, chord.clone()) {
            self.refuse(match error {
                ShortcutConfigError::Conflict(other) => {
                    tilo_sheets::words("guide-rebind-in-use", &[("action", &shortcut_name(other))])
                }
                ShortcutConfigError::ReservedSystemKey => {
                    tilo_sheets::word("guide-rebind-reserved")
                }
                ShortcutConfigError::Fixed(id) => tilo_sheets::words(
                    "guide-rebind-in-use",
                    &[("action", &fixed_shortcut_name(id))],
                ),
                ShortcutConfigError::InvalidChord => tilo_sheets::word("guide-rebind-unsupported"),
                ShortcutConfigError::TakenByAction => {
                    tilo_sheets::word("guide-rebind-taken-by-action")
                }
            });
            return;
        }
        self.accept(candidate, "guide-rebind-save-failed");
    }

    /// Put the pair `first` and `second` back on their default chords: both
    /// at once, so a default the pair shares among itself is free to take.
    fn reset_to_defaults(&self, first: &str, second: &str) {
        let (Some(first), Some(second)) = (
            ShortcutAction::from_id(first),
            ShortcutAction::from_id(second),
        ) else {
            return;
        };
        let defaults = ShortcutConfig::default();
        let mut candidate = self.table.borrow().clone();
        candidate.bindings.remove(&first);
        candidate.bindings.remove(&second);
        let restore = [first, second].into_iter().try_for_each(|action| {
            defaults
                .bindings
                .get(&action)
                .cloned()
                .ok_or(ShortcutConfigError::InvalidChord)
                .and_then(|chord| candidate.set(action, chord))
        });
        if let Err(error) = restore {
            self.refuse(match error {
                ShortcutConfigError::Conflict(other) => {
                    tilo_sheets::words("guide-reset-in-use", &[("action", &shortcut_name(other))])
                }
                ShortcutConfigError::TakenByAction => {
                    tilo_sheets::word("guide-reset-taken-by-action")
                }
                ShortcutConfigError::ReservedSystemKey
                | ShortcutConfigError::InvalidChord
                | ShortcutConfigError::Fixed(_) => tilo_sheets::word("guide-reset-invalid"),
            });
            return;
        }
        self.accept(candidate, "guide-reset-save-failed");
    }

    /// Keep `candidate` as the table the session answers: saved first, so a
    /// table the person's settings cannot keep never runs, then shared with
    /// the keys, the apps' shortcuts and the guide, and the waiting ends.
    fn accept(&self, candidate: ShortcutConfig, on_save_failure: &str) {
        if candidate.save().is_err() {
            self.refuse(tilo_sheets::word(on_save_failure));
            return;
        }
        *self.table.borrow_mut() = candidate.clone();
        if let Some(portal) = &self.portal {
            portal.set_shortcut_table(candidate);
        }
        self.keys.stop_listening();
        self.message.replace(None);
    }

    /// Say why the last press changed nothing; the guide keeps waiting for
    /// the keys of the shortcut it asks about.
    fn refuse(&self, message: String) {
        self.message.replace(Some(message));
    }

    /// Tell the guide what the table holds and what it waits for, when either
    /// changed since it was last told.
    fn refresh(&self) {
        let Some(seat) = &self.seat else {
            return;
        };
        let table = self.table.borrow();
        let listening = self.keys.listening();
        let message = self.message.borrow().clone();
        let controller = self.controller.get();
        let shown = (
            table.clone(),
            listening.clone(),
            message.clone(),
            controller,
        );
        if self
            .built
            .borrow()
            .as_ref()
            .is_some_and(|(built, _)| built == &shown)
        {
            return;
        }
        let mut guide = tilo_sheets::shortcut_guide::keyboard_guide(&table);
        guide.listening = listening;
        guide.message = message;
        // A controller in hand is explained by its own buttons, drawn with
        // the legends that controller prints.
        if let Some(family) = controller {
            guide.modality = Modality::Controller;
            guide.controller = Some(family);
            guide.faces = faces(family);
        }
        seat.show(guide.clone());
        *self.built.borrow_mut() = Some((shown, guide));
    }
}

/// Which face button carries each role on `family`, as the guide draws them.
fn faces(family: ControllerFamily) -> Faces {
    Faces {
        confirm: face_button(family, FaceRole::Confirm),
        cancel: face_button(family, FaceRole::Cancel),
        third: face_button(family, FaceRole::Third),
        fourth: face_button(family, FaceRole::Fourth),
    }
}

/// Whether `chord` is a key that types: a printable key with no modifier
/// held would run the shortcut instead of typing.
fn types_a_character(chord: &KeyChord) -> bool {
    chord.modifiers.is_empty() && (chord.key == "space" || chord.key.chars().count() == 1)
}

/// What the person calls the shortcut `action`, as the guide's own words say
/// it.
fn shortcut_name(action: ShortcutAction) -> String {
    tilo_sheets::word(&action.label_key())
}

/// What the person calls the fixed shortcut `id`.
fn fixed_shortcut_name(id: &str) -> String {
    settings_store::keyboard_shortcuts::fixed_shortcut(id)
        .map_or_else(|| id.to_owned(), |fixed| tilo_sheets::word(fixed.label_key))
}
