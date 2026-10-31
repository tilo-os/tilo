//! Dictation in the session: the microphone the person allows per dictation,
//! the recognizer behind it, and the field the words go into.
//!
//! The dictation itself is [`tilo_dictation`]'s: it holds the recognizer, the
//! microphone and the per-dictation consent, and publishes its status as a
//! resource beside the session's other services. This module is the session's
//! part of it: it carries the person's keys and the sheet's answers into the
//! service, shows what it says on the sheet, and is the one place that reads
//! the focused field and writes into it — the compositor's text-input
//! boundary for what the apps it shows draw, and the desktop's own published
//! field for the search the session draws itself.
//!
//! Nothing here answers consent. The card's buttons answer the sheet's own
//! seat ([`DictationSeat`]), and the service refuses an answer that names
//! another dictation than the one it is waiting for.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use settings_store::hardware::ShortcutAction;
use tilo_compositor::{Compositor, TEXT_MIME, TextField, TextFieldRefusal, TextInsertion};
use tilo_desktop_surface::{DesktopHandle, TypedField, TypedInsertion, TypedRefusal};
use tilo_dictation::{
    DictationError, DictationLink, DictationService, DictationStatus, Field, FieldRefusal,
    Insertion, Request, SessionId, Target,
};
use tilo_sheets::Seat as _;
use tilo_sheets::dictation::{Answer, DictationSeat, Failure, Prompt, RecoveryReason, Stage};

use crate::mount::clipboard::Clipboard;
use crate::sheets::Sheet;
use crate::{Held, Options};

/// How much of what is being heard the card shows: the last words, so the
/// person sees the recognizer following them without the sheet working
/// through a long transcript every frame.
const PREVIEW_SCALARS: usize = 160;

/// Dictation as the session has it: the service's handle, and the sheet the
/// person is asked on.
pub(crate) struct Dictation {
    link: DictationLink,
    /// The card's seat; `None` when the sheet did not mount.
    seat: Option<DictationSeat>,
    /// The card is up.
    up: Cell<bool>,
    /// The person asked to be taken where the speech models are set up.
    model_setup: Cell<bool>,
    /// What the card was last told, so it is woken only on a change.
    shown: RefCell<Option<Prompt>>,
}

impl Dictation {
    /// Start the dictation service and mount the sheet that asks about the
    /// microphone.
    pub(crate) fn mount(
        compositor: &mut Compositor,
        doors: &mut crate::Doors,
        services: &Rc<RefCell<tilo_service::Services>>,
        options: &Options,
    ) -> Self {
        let (service, link) = DictationService::start(options.wake.handle());
        services.borrow_mut().add(Box::new(service));
        let seat = mount_sheet(compositor, doors, options);
        Self {
            link,
            seat,
            up: Cell::new(false),
            model_setup: Cell::new(false),
            shown: RefCell::new(None),
        }
    }

    /// Whether the person asked the card where speech models are set up,
    /// once: the session opens it and clears the ask.
    pub(crate) fn take_model_setup(&self) -> bool {
        self.model_setup.replace(false)
    }

    /// The card's presentation: where it sits; `None` when the sheet did
    /// not mount.
    pub(crate) fn presentation(&self) -> Option<&tilo_sheets::Presentation> {
        self.seat.as_ref().map(tilo_sheets::Seat::presentation)
    }

    /// A key the person pressed asked for dictation.
    pub(crate) fn shortcut(&self, action: ShortcutAction) {
        let request = match action {
            ShortcutAction::DictationToggle => Request::Toggle,
            ShortcutAction::DictationFinish => Request::Finish,
            ShortcutAction::DictationCancel => Request::Cancel,
            _ => return,
        };
        self.link.request(request);
    }

    /// Carry the dictation on: what was asked of it, what the person answered
    /// on the sheet, what the recognizer heard, and the field it writes into.
    pub(crate) fn step(
        &self,
        compositor: &mut Compositor,
        own: &dyn OwnFields,
        clipboard: &Clipboard,
        locked: bool,
        now: Instant,
    ) {
        // Locking ends a dictation and keeps nothing: what was heard stays on
        // this device and only for the person who is here.
        if locked {
            self.link.discard();
        }
        for request in self.link.take_requests() {
            self.ask(request, now);
        }
        let mut fields = Fields { compositor, own };
        self.link.advance(&mut fields, now);
        self.answer(compositor, own, clipboard, locked, now);
        self.show();
    }

    /// Carry out one request, as far as it goes on its own: the rest waits on
    /// the person or the recognizer, and [`Self::step`] follows it.
    fn ask(&self, request: Request, now: Instant) {
        match request {
            // The dictation key stops what is listening and starts nothing
            // while a dictation or its transcript still owns the seat.
            Request::Toggle => match self.link.status() {
                DictationStatus::Listening => {
                    if let Some(id) = self.link.id() {
                        let _ = self.link.stop_recording(id);
                    }
                }
                // The kept transcript is the person's to copy or throw away,
                // not something the key throws away for them.
                DictationStatus::Consent
                | DictationStatus::Preparing
                | DictationStatus::Loading
                | DictationStatus::StoppingRecording
                | DictationStatus::Finishing
                | DictationStatus::Stopping
                | DictationStatus::Recovery { .. } => {}
                DictationStatus::Idle | DictationStatus::Unavailable(_) => {
                    self.start(now);
                }
            },
            Request::Finish => {
                if let Some(id) = self.link.id() {
                    let _ = self.link.stop_recording(id);
                }
            }
            Request::Cancel => {
                if let Some(id) = self.link.id() {
                    self.link.cancel(id);
                }
            }
            Request::DiscardTranscript => {
                if let Some(id) = self.link.id() {
                    self.link.discard_recovery(id);
                }
            }
        }
    }

    /// Ask for a dictation in the field the person is in. It records nothing
    /// until they allow the microphone on the card, and the field is looked
    /// at as the dictation prepares: an app may still be enabling the field
    /// the person just focused.
    fn start(&self, now: Instant) {
        // The field is read on the next step, as the dictation prepares.
        if let Err(error) = self.link.begin(now) {
            self.link.failed(error);
        }
    }

    /// What the person answered on the card.
    fn answer(
        &self,
        compositor: &mut Compositor,
        own: &dyn OwnFields,
        clipboard: &Clipboard,
        locked: bool,
        now: Instant,
    ) {
        let Some(seat) = &self.seat else {
            return;
        };
        for answer in seat.take_answers() {
            if locked {
                continue;
            }
            match answer {
                Answer::Allow(id) => {
                    let mut fields = Fields { compositor, own };
                    if let Err(error) = self.link.allow(SessionId::new(id), &mut fields, now) {
                        self.link.failed(error);
                    }
                }
                Answer::Deny(id) | Answer::Cancel(id) => {
                    self.link.cancel(SessionId::new(id));
                }
                Answer::Finish(id) => {
                    let _ = self.link.stop_recording(SessionId::new(id));
                }
                Answer::Copy(id) => {
                    let id = SessionId::new(id);
                    let copied = self.link.recovery(id).is_some_and(|text| {
                        clipboard.copy_for_person(compositor, TEXT_MIME, text.as_bytes().to_vec())
                    });
                    self.link.copied(id, copied);
                }
                Answer::Discard(id) => {
                    self.link.discard_recovery(SessionId::new(id));
                }
                Answer::SetUp(id) => {
                    // The model the dictation needed is installed where the
                    // person manages them: the card closes, and the session
                    // takes them there (`take_model_setup`).
                    self.link.cancel(SessionId::new(id));
                    self.model_setup.set(true);
                }
            }
        }
    }

    /// Put the card up with where the dictation stands, and take it down when
    /// there is nothing left to answer.
    fn show(&self) {
        let Some(seat) = &self.seat else {
            return;
        };
        let prompt = self.prompt();
        if *self.shown.borrow() != prompt {
            seat.show(prompt.clone());
            self.shown.borrow_mut().clone_from(&prompt);
        }
        match (&prompt, self.up.get()) {
            (Some(_), false) => {
                seat.presentation().open(true);
                self.up.set(true);
            }
            (None, true) => {
                seat.presentation().close();
                seat.presentation().gone();
                self.up.set(false);
            }
            _ => {}
        }
    }

    /// Where the dictation stands, as the card shows it.
    fn prompt(&self) -> Option<Prompt> {
        let status = self.link.status();
        let stage = stage(status)?;
        let recovery = match status {
            DictationStatus::Recovery {
                reason,
                copied,
                copy_failed,
            } => Some((reason, copied, copy_failed)),
            _ => None,
        };
        let prompt = Prompt {
            id: self.link.id().or_else(|| self.link.last_id())?.get(),
            stage,
            draft: self.preview(),
            failure: self.link.failure().map(failure),
            recovery: recovery.map(|(reason, _, _)| match reason {
                tilo_dictation::RecoveryReason::VocabularyChanged => {
                    RecoveryReason::VocabularyChanged
                }
                tilo_dictation::RecoveryReason::TooLong => RecoveryReason::TooLong,
            }),
            copied: recovery.is_some_and(|(_, copied, _)| copied),
            copy_failed: recovery.is_some_and(|(_, _, copy_failed)| copy_failed),
        };
        Some(prompt)
    }

    /// The last words of what is being heard, and nothing else: the card
    /// shows the live hypothesis while the recognizer fills it.
    fn preview(&self) -> String {
        let Some(id) = self.link.id() else {
            return String::new();
        };
        let Some(draft) = self.link.preview(id) else {
            return String::new();
        };
        let start = draft
            .char_indices()
            .rev()
            .nth(PREVIEW_SCALARS.saturating_sub(1))
            .map_or(0, |(index, _)| index);
        draft
            .get(start..)
            .map_or_else(String::new, ToOwned::to_owned)
    }
}

/// The stage the card shows for `status`; `None` when there is nothing to
/// show.
const fn stage(status: DictationStatus) -> Option<Stage> {
    Some(match status {
        DictationStatus::Idle => return None,
        DictationStatus::Consent => Stage::Asking,
        DictationStatus::Preparing => Stage::Preparing,
        DictationStatus::Loading => Stage::Loading,
        DictationStatus::Listening => Stage::Listening,
        DictationStatus::StoppingRecording => Stage::StoppingMicrophone,
        DictationStatus::Finishing => Stage::Finishing,
        DictationStatus::Stopping => Stage::Stopping,
        DictationStatus::Recovery { .. } => Stage::Recovery,
        DictationStatus::Unavailable(_) => Stage::Unavailable,
    })
}

/// Why the dictation stopped, in the words the card says.
const fn failure(error: DictationError) -> Failure {
    match error {
        DictationError::NoEditableField | DictationError::SensitiveField => Failure::Field,
        DictationError::UnsupportedField => Failure::UnsupportedField,
        DictationError::Busy => Failure::Busy,
        DictationError::StaleSession | DictationError::TargetChanged => Failure::TargetChanged,
        DictationError::TimedOut => Failure::TimedOut,
        DictationError::DraftTooLarge
        | DictationError::ControlCharacter
        | DictationError::SessionIdsExhausted => Failure::Unavailable,
        DictationError::RecognizerUnavailable(reason) => recognizer_failure(reason.word()),
    }
}

/// Why the recognizer or its microphone stopped, in the words the card says.
const fn recognizer_failure(reason: &str) -> Failure {
    match reason.as_bytes() {
        b"model" => Failure::ModelRequired,
        b"language" => Failure::LanguageUnavailable,
        b"microphone" => Failure::MicrophoneUnavailable,
        b"vocabulary" => Failure::VocabularyUnavailable,
        b"audio" => Failure::AudioInterrupted,
        b"timed-out" => Failure::TimedOut,
        _ => Failure::Unavailable,
    }
}

/// The fields dictation may write into, in the session's terms: what the
/// compositor's text-input boundary sees, and what the session draws itself.
///
/// One target, one contract: a dictation binds a field and its revision, and
/// text goes in only while that is still the field the person types into.
struct Fields<'a> {
    compositor: &'a mut dyn CompositorFields,
    own: &'a dyn OwnFields,
}

impl Target for Fields<'_> {
    fn field(&mut self) -> Result<Field, FieldRefusal> {
        // What the session draws itself stands over the windows it shows:
        // while the person types into the desk's own search, that is the
        // field. It answers whether it owns the keys at all, so a refusal
        // there never falls through into a window behind it.
        match self.own.field() {
            Some(field) => field,
            None => self.compositor.field(),
        }
    }

    fn insert(&mut self, field: Field, text: &str) -> Insertion {
        if field.id & OWN_FIELD != 0 {
            return self.own.insert(field, text);
        }
        self.compositor.insert(field, text)
    }
}

/// The identity space of the fields the session draws itself: above the
/// compositor's, so a field dictation bound is never taken for another.
const OWN_FIELD: u64 = 1 << 63;

/// The compositor's fields, translated into the dictation's terms. The
/// compositor reads them at the text-input boundary of the apps it shows.
trait CompositorFields {
    fn field(&self) -> Result<Field, FieldRefusal>;

    fn insert(&mut self, field: Field, text: &str) -> Insertion;
}

impl CompositorFields for Compositor {
    fn field(&self) -> Result<Field, FieldRefusal> {
        self.dictation_field()
            .map_err(|refusal| match refusal {
                TextFieldRefusal::NoField => FieldRefusal::NoField,
                TextFieldRefusal::Secret => FieldRefusal::Secret,
                TextFieldRefusal::Unsupported => FieldRefusal::Unsupported,
            })
            .map(|field| Field {
                id: field.id,
                revision: field.revision,
                multiline: field.multiline,
            })
    }

    fn insert(&mut self, field: Field, text: &str) -> Insertion {
        let field = TextField {
            id: field.id,
            revision: field.revision,
            multiline: field.multiline,
        };
        match self.insert_dictation(field, text) {
            TextInsertion::Inserted => Insertion::Inserted,
            TextInsertion::Changed => Insertion::Changed,
            TextInsertion::Refused(refusal) => Insertion::Refused(match refusal {
                TextFieldRefusal::NoField => FieldRefusal::NoField,
                TextFieldRefusal::Secret => FieldRefusal::Secret,
                TextFieldRefusal::Unsupported => FieldRefusal::Unsupported,
            }),
        }
    }
}

/// The fields the session draws itself, which no compositor's text-input
/// boundary sees: the desktop's own search.
pub(crate) trait OwnFields {
    /// The field the session draws that takes the typing, or `None` when
    /// nothing of its own does and the windows' fields stand. It answers
    /// `Some` with a refusal while something of the session's holds the keys
    /// without a field to write into, so nothing behind it takes the words.
    fn field(&self) -> Option<Result<Field, FieldRefusal>>;

    fn insert(&self, field: Field, text: &str) -> Insertion;
}

impl OwnFields for DesktopHandle {
    fn field(&self) -> Option<Result<Field, FieldRefusal>> {
        match self.typed_field() {
            Ok(field) => Some(Ok(Field {
                id: field.id | OWN_FIELD,
                revision: field.revision,
                multiline: field.multiline,
            })),
            // The open search takes the keys even where its own field does
            // not: its rows, not a window behind them, are what the person
            // sees.
            Err(refusal) if self.search_open() => Some(Err(match refusal {
                TypedRefusal::Secret => FieldRefusal::Secret,
                TypedRefusal::NoField => FieldRefusal::NoField,
            })),
            Err(_) => None,
        }
    }

    fn insert(&self, field: Field, text: &str) -> Insertion {
        let typed = TypedField {
            id: field.id & !OWN_FIELD,
            revision: field.revision,
            multiline: field.multiline,
        };
        match self.commit_typed(typed, text) {
            TypedInsertion::Inserted => Insertion::Inserted,
            TypedInsertion::Changed => Insertion::Changed,
            TypedInsertion::Refused(refusal) => Insertion::Refused(match refusal {
                TypedRefusal::NoField => FieldRefusal::NoField,
                TypedRefusal::Secret => FieldRefusal::Secret,
            }),
        }
    }
}

/// Mount the dictation sheet: it asks, and the session answers, without
/// taking the person's typing from the field they are dictating into.
fn mount_sheet(
    compositor: &mut Compositor,
    doors: &mut crate::Doors,
    options: &Options,
) -> Option<DictationSeat> {
    let who = (options.device.as_str(), options.person.as_str());
    match Sheet::mount(
        tilo_sheets::dictation::definition(),
        DictationSeat::default(),
        who,
        (options.presentation)(),
        options.scale,
    ) {
        Ok(sheet) => {
            let seat = sheet.seat().clone();
            let sheet = Rc::new(RefCell::new(sheet.typing_through()));
            compositor.add_host(Box::new(Held(Rc::clone(&sheet))));
            doors.add(sheet);
            Some(seat)
        }
        Err(error) => {
            tracing::warn!(%error, "dictation sheet not mounted");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPOSITOR_FIELD: Field = Field {
        id: 7,
        revision: 3,
        multiline: false,
    };
    const OWN_FIELD_STATE: Field = Field {
        id: OWN_FIELD | 1,
        revision: 9,
        multiline: false,
    };

    #[derive(Default)]
    struct FakeCompositor {
        field: Option<Field>,
        inserted: RefCell<Vec<(Field, String)>>,
    }

    impl CompositorFields for FakeCompositor {
        fn field(&self) -> Result<Field, FieldRefusal> {
            self.field.ok_or(FieldRefusal::NoField)
        }

        fn insert(&mut self, field: Field, text: &str) -> Insertion {
            self.inserted.borrow_mut().push((field, text.to_owned()));
            Insertion::Inserted
        }
    }

    struct FakeOwn {
        field: Option<Result<Field, FieldRefusal>>,
        inserted: RefCell<Vec<(Field, String)>>,
    }

    impl OwnFields for FakeOwn {
        fn field(&self) -> Option<Result<Field, FieldRefusal>> {
            self.field
        }

        fn insert(&self, field: Field, text: &str) -> Insertion {
            // A field that moved since it was bound takes nothing, as the
            // desk's own field rechecks its revision.
            if self.field != Some(Ok(field)) {
                return Insertion::Changed;
            }
            self.inserted.borrow_mut().push((field, text.to_owned()));
            Insertion::Inserted
        }
    }

    fn own(field: Result<Field, FieldRefusal>) -> FakeOwn {
        FakeOwn {
            field: Some(field),
            inserted: RefCell::new(Vec::new()),
        }
    }

    fn without_own_field() -> FakeOwn {
        FakeOwn {
            field: None,
            inserted: RefCell::new(Vec::new()),
        }
    }

    #[test]
    fn the_desks_own_field_stands_over_the_compositors() {
        let mut compositor = FakeCompositor {
            field: Some(COMPOSITOR_FIELD),
            ..FakeCompositor::default()
        };
        let own = own(Ok(OWN_FIELD_STATE));
        let mut fields = Fields {
            compositor: &mut compositor,
            own: &own,
        };
        assert_eq!(fields.field(), Ok(OWN_FIELD_STATE));
        assert_eq!(fields.insert(OWN_FIELD_STATE, "hello"), Insertion::Inserted);
        assert_eq!(
            own.inserted.borrow().as_slice(),
            [(OWN_FIELD_STATE, "hello".to_owned())]
        );
        assert!(
            compositor.inserted.borrow().is_empty(),
            "the app's field behind the desk is not typed into"
        );
    }

    #[test]
    fn without_an_own_field_the_compositors_field_is_dictated_into() {
        let mut compositor = FakeCompositor {
            field: Some(COMPOSITOR_FIELD),
            ..FakeCompositor::default()
        };
        let own = without_own_field();
        let mut fields = Fields {
            compositor: &mut compositor,
            own: &own,
        };
        assert_eq!(fields.field(), Ok(COMPOSITOR_FIELD));
        assert_eq!(
            fields.insert(COMPOSITOR_FIELD, "hello"),
            Insertion::Inserted
        );
        assert_eq!(
            compositor.inserted.borrow().as_slice(),
            [(COMPOSITOR_FIELD, "hello".to_owned())]
        );
        assert!(own.inserted.borrow().is_empty());
    }

    #[test]
    fn a_secret_own_field_is_refused_rather_than_skipped() {
        let mut compositor = FakeCompositor {
            field: Some(COMPOSITOR_FIELD),
            ..FakeCompositor::default()
        };
        let own = own(Err(FieldRefusal::Secret));
        let mut fields = Fields {
            compositor: &mut compositor,
            own: &own,
        };
        assert_eq!(fields.field(), Err(FieldRefusal::Secret));
    }

    #[test]
    fn a_desk_holding_the_keys_without_a_field_takes_none_either() {
        let mut compositor = FakeCompositor {
            field: Some(COMPOSITOR_FIELD),
            ..FakeCompositor::default()
        };
        // The search's rows take the keys, so no window behind them does.
        let own = own(Err(FieldRefusal::NoField));
        let mut fields = Fields {
            compositor: &mut compositor,
            own: &own,
        };
        assert_eq!(fields.field(), Err(FieldRefusal::NoField));
    }

    #[test]
    fn text_goes_where_the_field_it_bound_lives() {
        let mut compositor = FakeCompositor {
            field: Some(COMPOSITOR_FIELD),
            ..FakeCompositor::default()
        };
        // The field moved on since the dictation bound it.
        let own = own(Ok(Field {
            revision: 10,
            ..OWN_FIELD_STATE
        }));
        let mut fields = Fields {
            compositor: &mut compositor,
            own: &own,
        };
        assert_eq!(fields.insert(OWN_FIELD_STATE, "late"), Insertion::Changed);
        assert!(own.inserted.borrow().is_empty());
        assert!(
            compositor.inserted.borrow().is_empty(),
            "the text does not fall through into another field"
        );
    }
}
