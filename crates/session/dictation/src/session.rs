//! One dictation, from the key that asks for it to the text it commits.
//!
//! A dictation is bound to the field the person was typing into and to the
//! revision its client had committed. Nothing records before the person
//! allowed the microphone for *this* dictation, in this session: neither the
//! recognizer being ready nor any action naming a dictation is consent.
//! [`Dictation::allow`] is the only way into the recognizer, and it names the
//! session it grants, so a prompt that was retired cannot grant its successor.
//!
//! Hypotheses stay here, in a private zeroized draft; they never reach the
//! workspace, the clipboard or diagnostics. Only the final text is committed,
//! once, into the bound field — and only if that field is still the person's.

use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_provider_config::speech;
use settings_store::personal_dictionary::Vocabulary;
use zeroize::Zeroizing;

use crate::runtime::{self, RuntimeEvent};
use crate::status::{DictationError, DictationStatus, RecoveryReason, SessionId};
use crate::target::{Field, FieldRefusal, Insertion, Target};

/// The most a dictation lives, including finishing: the start of the
/// recognizer, at most 105 seconds of capture, and a bounded finalization.
pub const SESSION_WINDOW: Duration = Duration::from_secs(180);
/// A strict bound on a draft's UTF-8 bytes, whatever the model returns.
pub const MAX_DRAFT_BYTES: usize = 16 * 1024;
/// What a field takes in one edit, protocol overhead included: a longer text
/// would have to be split, losing atomic selection and undo.
pub const MAX_COMMIT_BYTES: usize = 4000;
/// How long a kept transcript waits for the person to copy it.
const RECOVERY_WINDOW: Duration = Duration::from_secs(600);
/// How long the field may take to ask for typing after dictation was asked
/// for: a client enables its field on the focus it just got.
const DELIVERY_WINDOW: Duration = Duration::from_secs(2);

/// The recognizer serving one dictation. It stops when it is dropped, and its
/// presence is what "recording" means here.
struct Recognition {
    id: SessionId,
    actor: runtime::Runtime,
    phase: DictationStatus,
    language: String,
    literal: bool,
}

/// A dictation allowed to run against `target`.
struct Session {
    id: SessionId,
    target: Field,
    deadline: Instant,
    draft: Zeroizing<String>,
}

/// Waiting for the focused client to enable its field.
struct Preparation {
    id: SessionId,
    deadline: Instant,
}

/// A transcript that was not inserted, kept for the person to copy.
struct Recovery {
    reason: RecoveryReason,
    id: SessionId,
    text: Zeroizing<String>,
    deadline: Instant,
    copied: bool,
    copy_failed: bool,
}

/// What a session knows of dictation between its steps.
#[derive(Default)]
pub struct Dictation {
    next: u64,
    /// The last dictation asked for, so its card can name a failure that left
    /// no dictation behind.
    last_id: Option<SessionId>,
    preparation: Option<Preparation>,
    session: Option<Session>,
    recognition: Option<Recognition>,
    last_failure: Option<DictationError>,
    recovery: Option<Recovery>,
    vocabulary: Option<Arc<dyn Vocabulary>>,
}

impl Dictation {
    /// The authenticated host supplies the person's vocabulary, never an app
    /// and never the model. Rebinding retires queued words and a kept
    /// transcript before another person's reader can be used.
    pub fn set_vocabulary(&mut self, reader: Option<Arc<dyn Vocabulary>>) {
        self.discard_recovery();
        self.vocabulary = reader;
    }

    /// Where this dictation is.
    #[must_use]
    pub fn status(&self) -> DictationStatus {
        if let Some(recognition) = &self.recognition {
            return recognition.phase;
        }
        if self.preparation.is_some() {
            return DictationStatus::Preparing;
        }
        if self.session.is_some() {
            return DictationStatus::Consent;
        }
        if let Some(recovery) = &self.recovery {
            return DictationStatus::Recovery {
                reason: recovery.reason,
                copied: recovery.copied,
                copy_failed: recovery.copy_failed,
            };
        }
        self.last_failure.map_or(DictationStatus::Idle, DictationStatus::Unavailable)
    }

    /// The last dictation asked for: the one a failure card is about.
    #[must_use]
    pub const fn last_id(&self) -> Option<SessionId> {
        self.last_id
    }

    /// The dictation this is about: the running one, the one being prepared,
    /// or the one whose transcript is kept.
    #[must_use]
    pub fn id(&self) -> Option<SessionId> {
        self.recognition
            .as_ref()
            .map(|recognition| recognition.id)
            .or_else(|| self.session.as_ref().map(|session| session.id))
            .or_else(|| self.preparation.as_ref().map(|preparation| preparation.id))
            .or_else(|| self.recovery.as_ref().map(|recovery| recovery.id))
    }

    /// The live hypothesis of `id`'s dictation, for the sheet only.
    #[must_use]
    pub fn preview(&self, id: SessionId) -> Option<&str> {
        self.session
            .as_ref()
            .filter(|session| session.id == id)
            .map(|session| session.draft.as_str())
    }

    /// Why the last dictation did not run, for the sheet.
    #[must_use]
    pub fn failure(&self) -> Option<DictationError> {
        self.last_failure
    }

    /// Ask for a dictation. The microphone stays closed: the person is asked
    /// to allow it for this dictation ([`Self::allow`]).
    ///
    /// # Errors
    /// [`DictationError::Busy`] while another dictation, or its transcript,
    /// still owns the seat, and [`DictationError::SessionIdsExhausted`] once
    /// every identity has been used.
    pub fn begin(&mut self, now: Instant) -> Result<SessionId, DictationError> {
        if self.preparation.is_some()
            || self.session.is_some()
            || self.recognition.is_some()
            || self.recovery.is_some()
        {
            return Err(DictationError::Busy);
        }
        let id = self.allocate()?;
        self.last_failure = None;
        self.preparation = Some(Preparation {
            id,
            deadline: now.checked_add(DELIVERY_WINDOW).unwrap_or(now),
        });
        Ok(id)
    }

    /// Step the session on: the field asking for typing, what the recognizer
    /// heard, and the deadlines. Never blocks.
    pub fn advance(&mut self, target: &mut dyn Target, now: Instant) {
        self.expire_recovery(now);
        if let Some(preparation) = self.preparation.take() {
            if now >= preparation.deadline {
                self.last_failure = Some(DictationError::NoEditableField);
            } else {
                match target.field() {
                    Ok(field) => {
                        self.session = Some(Session {
                            id: preparation.id,
                            target: field,
                            deadline: now.checked_add(SESSION_WINDOW).unwrap_or(now),
                            draft: Zeroizing::new(String::new()),
                        });
                    }
                    // The client may still be enabling the field it just got.
                    Err(FieldRefusal::NoField) => self.preparation = Some(preparation),
                    Err(refusal) => self.last_failure = Some(refusal_error(refusal)),
                }
            }
        }
        self.pump(target, now);
    }

    /// The person allowed the microphone for `id`, once, in this field: the
    /// one way a dictation starts recording. Consent names the dictation it
    /// was given for, so a retired prompt cannot grant its successor.
    ///
    /// # Errors
    /// [`DictationError::StaleSession`] when `id` is not the dictation waiting
    /// for consent, and the field or recognizer failures of [`Self::validate`].
    pub fn allow(
        &mut self,
        id: SessionId,
        target: &mut dyn Target,
        now: Instant,
    ) -> Result<(), DictationError> {
        self.session
            .as_ref()
            .filter(|session| session.id == id)
            .ok_or(DictationError::StaleSession)?;
        self.validate(id, target, now)?;
        if self.recognition.is_some() {
            return Err(DictationError::Busy);
        }
        let multiline = self
            .session
            .as_ref()
            .is_some_and(|session| session.target.multiline);
        let language = tilo_accessibility_settings::AccessibilitySettings::load().dictation_language;
        let actor =
            runtime::Runtime::start_for_field(language.code(), self.vocabulary.clone(), multiline)
                .map_err(DictationError::RecognizerUnavailable)?;
        self.last_failure = None;
        self.recognition = Some(Recognition {
            id,
            actor,
            phase: DictationStatus::Loading,
            language: language.code().to_owned(),
            literal: tilo_accessibility_settings::AccessibilitySettings::load().dictation_literal,
        });
        Ok(())
    }

    /// Stop the microphone and ask for one final, complete result.
    ///
    /// # Errors
    /// [`DictationError::StaleSession`] when `id` is not the dictation that is
    /// listening, [`DictationError::Busy`] while it is not ready yet, and
    /// [`DictationError::RecognizerUnavailable`] when the worker is gone.
    pub fn stop_recording(&mut self, id: SessionId) -> Result<(), DictationError> {
        let recognition = self
            .recognition
            .as_mut()
            .filter(|recognition| recognition.id == id)
            .ok_or(DictationError::StaleSession)?;
        if matches!(
            recognition.phase,
            DictationStatus::StoppingRecording | DictationStatus::Finishing
        ) {
            return Ok(());
        }
        if recognition.phase != DictationStatus::Listening {
            return Err(DictationError::Busy);
        }
        recognition
            .actor
            .finish()
            .map_err(DictationError::RecognizerUnavailable)?;
        recognition.phase = DictationStatus::StoppingRecording;
        Ok(())
    }

    /// Commit the latest corrected text into the original field, once.
    ///
    /// # Errors
    /// The stale, changed, timed-out and oversized failures of
    /// [`Self::validate`], and the field's own refusal.
    pub fn finish(
        &mut self,
        id: SessionId,
        target: &mut dyn Target,
        now: Instant,
    ) -> Result<(), DictationError> {
        self.validate(id, target, now)?;
        let Some(session) = self.session.take() else {
            return Err(DictationError::StaleSession);
        };
        if session.draft.len() > MAX_COMMIT_BYTES {
            self.keep(RecoveryReason::TooLong, session.id, session.draft, now);
            self.cancel_active();
            return Ok(());
        }
        if session.draft.is_empty() {
            self.cancel_active();
            return Ok(());
        }
        let outcome = target.insert(session.target, &session.draft);
        self.cancel_active();
        match outcome {
            Insertion::Inserted => Ok(()),
            // The text was not written anywhere: the person's field moved.
            Insertion::Changed => {
                self.last_failure = Some(DictationError::TargetChanged);
                Err(DictationError::TargetChanged)
            }
            Insertion::Refused(refusal) => {
                let error = refusal_error(refusal);
                self.last_failure = Some(error);
                Err(error)
            }
        }
    }

    /// Stop what `id` asked for. A late answer cannot cancel its successor.
    pub fn cancel(&mut self, id: SessionId) -> bool {
        // A failure that left no dictation behind is still the person's to
        // close, by the id of the dictation that failed.
        if self.id().or(self.last_id) != Some(id) {
            return false;
        }
        self.discard_recovery();
        self.last_failure = None;
        self.cancel_active();
        true
    }

    /// Stop the running dictation and throw away a kept transcript: the
    /// session is locking.
    pub fn discard(&mut self) {
        self.discard_recovery();
        self.last_failure = None;
        self.cancel_active();
    }

    /// The transcript kept for the person to copy, if `id` names it.
    #[must_use]
    pub fn recovery(&self, id: SessionId) -> Option<&str> {
        self.recovery
            .as_ref()
            .filter(|recovery| recovery.id == id)
            .map(|recovery| recovery.text.as_str())
    }

    /// Note what became of copying the kept transcript.
    pub fn note_copied(&mut self, id: SessionId, copied: bool) {
        let Some(recovery) = self
            .recovery
            .as_mut()
            .filter(|recovery| recovery.id == id)
        else {
            return;
        };
        recovery.copied |= copied;
        recovery.copy_failed = !copied;
    }

    /// Throw away the kept transcript of `id`, and stop what `id` asked for.
    pub fn discard_recovery_of(&mut self, id: SessionId) -> bool {
        if self
            .recovery
            .as_ref()
            .is_some_and(|recovery| recovery.id == id)
        {
            self.discard_recovery();
            self.cancel_active();
            return true;
        }
        false
    }

    /// Note what the person's answer ran into, so the sheet says why nothing
    /// ran, and stop what it would have started.
    pub fn note_failure(&mut self, error: DictationError) {
        self.last_failure = Some(error);
        self.cancel_active();
    }

    /// Throw away the kept transcript; the next dictation starts clean.
    pub fn discard_recovery(&mut self) {
        self.recovery = None;
    }

    fn allocate(&mut self) -> Result<SessionId, DictationError> {
        self.next = self
            .next
            .checked_add(1)
            .ok_or(DictationError::SessionIdsExhausted)?;
        let id = SessionId::new(self.next);
        self.last_id = Some(id);
        Ok(id)
    }

    fn keep(&mut self, reason: RecoveryReason, id: SessionId, text: Zeroizing<String>, now: Instant) {
        if text.is_empty() {
            return;
        }
        self.recovery = Some(Recovery {
            reason,
            id,
            text,
            deadline: now.checked_add(RECOVERY_WINDOW).unwrap_or(now),
            copied: false,
            copy_failed: false,
        });
    }

    fn expire_recovery(&mut self, now: Instant) {
        if self
            .recovery
            .as_ref()
            .is_some_and(|recovery| now >= recovery.deadline)
        {
            self.recovery = None;
            self.last_failure = Some(DictationError::TimedOut);
        }
    }

    /// Stop everything this dictation holds. The recognizer keeps its slot
    /// until its worker is actually gone.
    fn cancel_active(&mut self) {
        self.preparation = None;
        self.session = None;
        if let Some(recognition) = self.recognition.as_mut() {
            recognition.actor.cancel();
            recognition.phase = DictationStatus::Stopping;
        }
    }

    /// Check that `id` may still write into the field it was bound to, and
    /// stop it otherwise.
    fn validate(
        &mut self,
        id: SessionId,
        target: &mut dyn Target,
        now: Instant,
    ) -> Result<(), DictationError> {
        let session = self
            .session
            .as_ref()
            .filter(|session| session.id == id)
            .ok_or(DictationError::StaleSession)?;
        let result = if now >= session.deadline {
            Err(DictationError::TimedOut)
        } else {
            match target.field() {
                Ok(field) if field == session.target => Ok(()),
                Ok(_) | Err(FieldRefusal::NoField) => Err(DictationError::TargetChanged),
                Err(refusal) => Err(refusal_error(refusal)),
            }
        };
        if let Err(error) = result {
            self.last_failure = Some(error);
            self.cancel_active();
        }
        result
    }

    /// What the recognizer said, and what has to follow from it.
    fn pump(&mut self, target: &mut dyn Target, now: Instant) {
        let Some(mut recognition) = self.recognition.take() else {
            return;
        };
        if self.validate(recognition.id, target, now).is_err() {
            recognition.actor.cancel();
            recognition.phase = DictationStatus::Stopping;
        }
        while let Some(event) = recognition.actor.poll() {
            let result = match event {
                RuntimeEvent::Ready if recognition.phase == DictationStatus::Loading => self
                    .validate(recognition.id, target, now)
                    .and_then(|()| {
                        recognition
                            .actor
                            .capture()
                            .map_err(DictationError::RecognizerUnavailable)
                    }),
                RuntimeEvent::Listening if recognition.phase == DictationStatus::Loading => {
                    recognition.phase = DictationStatus::Listening;
                    Ok(())
                }
                RuntimeEvent::Finishing
                    if matches!(
                        recognition.phase,
                        DictationStatus::Listening | DictationStatus::StoppingRecording
                    ) =>
                {
                    recognition.phase = DictationStatus::Finishing;
                    Ok(())
                }
                RuntimeEvent::Draft(text)
                    if matches!(
                        recognition.phase,
                        DictationStatus::Listening
                            | DictationStatus::StoppingRecording
                            | DictationStatus::Finishing
                    ) =>
                {
                    self.update(recognition.id, text, target, now)
                }
                RuntimeEvent::Done(text)
                    if matches!(
                        recognition.phase,
                        DictationStatus::Listening
                            | DictationStatus::StoppingRecording
                            | DictationStatus::Finishing
                    ) =>
                {
                    let recognized = Zeroizing::new(text);
                    let text = speech::clean_final(
                        &recognized,
                        &recognition.language,
                        recognition.literal,
                    )
                    .into_owned();
                    let result = self
                        .update(recognition.id, text, target, now)
                        .and_then(|()| self.finish(recognition.id, target, now));
                    recognition.phase = DictationStatus::Stopping;
                    result
                }
                // The person stopped it: nothing to report.
                RuntimeEvent::Stopped(runtime::RuntimeError::Canceled) => {
                    self.cancel_active();
                    recognition.phase = DictationStatus::Stopping;
                    Ok(())
                }
                RuntimeEvent::Stopped(error) => {
                    self.last_failure = Some(failure_of(error));
                    if error == runtime::RuntimeError::VocabularyChanged {
                        self.retain_draft(recognition.id, now);
                    }
                    self.cancel_active();
                    recognition.phase = DictationStatus::Stopping;
                    Ok(())
                }
                _ => Err(DictationError::StaleSession),
            };
            if result.is_err() {
                recognition.actor.cancel();
                recognition.phase = DictationStatus::Stopping;
                self.cancel_active();
            }
        }
        if !recognition.actor.is_stopped() {
            self.recognition = Some(recognition);
        } else if self.session.is_some() {
            // No terminal reply, most of all a full or disconnected result
            // queue, is never success: keep no draft and leave nothing live.
            self.last_failure = Some(DictationError::RecognizerUnavailable(
                runtime::RuntimeError::Protocol,
            ));
            self.cancel_active();
        }
    }

    /// Replace the hypothesis. Nothing is inserted before [`Self::finish`].
    fn update(
        &mut self,
        id: SessionId,
        draft: String,
        target: &mut dyn Target,
        now: Instant,
    ) -> Result<(), DictationError> {
        let draft = Zeroizing::new(draft);
        self.validate(id, target, now)?;
        let multiline = self
            .session
            .as_ref()
            .is_some_and(|session| session.target.multiline);
        let error = if draft.len() > MAX_DRAFT_BYTES {
            Some(DictationError::DraftTooLarge)
        } else if draft
            .chars()
            .any(|character| character.is_control() && !(multiline && character == '\n'))
        {
            Some(DictationError::ControlCharacter)
        } else {
            None
        };
        if let Some(error) = error {
            self.last_failure = Some(error);
            self.cancel_active();
            return Err(error);
        }
        if let Some(session) = self.session.as_mut() {
            session.draft = draft;
        }
        Ok(())
    }

    /// An ordinary dictionary edit stops recognition but never throws away
    /// words the person already saw: the draft is kept for copying, never
    /// inserted.
    fn retain_draft(&mut self, id: SessionId, now: Instant) {
        let text = self
            .session
            .as_mut()
            .filter(|session| session.id == id)
            .map(|session| Zeroizing::new(std::mem::take(&mut *session.draft)));
        if let Some(text) = text {
            self.keep(RecoveryReason::VocabularyChanged, id, text, now);
        }
    }
}

/// The session's error for a field that may not take dictated text.
const fn refusal_error(refusal: FieldRefusal) -> DictationError {
    match refusal {
        FieldRefusal::NoField => DictationError::NoEditableField,
        FieldRefusal::Secret => DictationError::SensitiveField,
        FieldRefusal::Unsupported => DictationError::UnsupportedField,
    }
}

/// The session's error for a recognizer that stopped.
const fn failure_of(error: runtime::RuntimeError) -> DictationError {
    match error {
        runtime::RuntimeError::TimedOut => DictationError::TimedOut,
        error => DictationError::RecognizerUnavailable(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A field the compositor tests drive by hand.
    struct FakeField {
        field: Result<Field, FieldRefusal>,
        inserted: Vec<(Field, String)>,
        insertion: Option<Insertion>,
    }

    impl FakeField {
        fn open(revision: u64) -> Self {
            Self {
                field: Ok(Field {
                    id: 7,
                    revision,
                    multiline: true,
                }),
                inserted: Vec::new(),
                insertion: None,
            }
        }

        fn moved() -> Self {
            Self {
                field: Ok(Field {
                    id: 7,
                    revision: 2,
                    multiline: true,
                }),
                inserted: Vec::new(),
                insertion: None,
            }
        }
    }

    impl Target for FakeField {
        fn field(&mut self) -> Result<Field, FieldRefusal> {
            self.field
        }

        fn insert(&mut self, field: Field, text: &str) -> Insertion {
            self.inserted.push((field, text.to_owned()));
            self.insertion.unwrap_or(Insertion::Inserted)
        }
    }

    fn bound(draft: &str) -> Dictation {
        let now = Instant::now();
        Dictation {
            next: 1,
            last_id: Some(SessionId::new(1)),
            preparation: None,
            session: Some(Session {
                id: SessionId::new(1),
                target: Field {
                    id: 7,
                    revision: 1,
                    multiline: true,
                },
                deadline: now.checked_add(SESSION_WINDOW).unwrap(),
                draft: Zeroizing::new(draft.to_owned()),
            }),
            recognition: None,
            last_failure: None,
            recovery: None,
            vocabulary: None,
        }
    }

    #[test]
    fn a_dictation_waits_for_the_person_before_it_records() {
        let now = Instant::now();
        let mut target = FakeField::open(1);
        let mut dictation = Dictation::default();
        let id = dictation.begin(now).unwrap();
        assert_eq!(dictation.status(), DictationStatus::Preparing);
        dictation.advance(&mut target, now);
        assert_eq!(dictation.status(), DictationStatus::Consent);
        assert!(
            dictation.preview(id).is_some_and(str::is_empty),
            "a bound dictation has an empty draft and nothing else"
        );
        // No recognizer exists, so no worker and no microphone is open.
        assert!(dictation.recognition.is_none());
        // No recognizer can speak before the person allows it, and the stop
        // refuses a dictation that is not listening.
        assert_eq!(
            dictation.stop_recording(id),
            Err(DictationError::StaleSession)
        );
        assert_eq!(
            dictation.stop_recording(id),
            Err(DictationError::StaleSession)
        );
        // Nothing was heard, so finishing ends the dictation and writes
        // nothing.
        assert_eq!(dictation.finish(id, &mut target, now), Ok(()));
        assert!(target.inserted.is_empty());
        assert_eq!(dictation.status(), DictationStatus::Idle);
    }

    #[test]
    fn only_the_dictation_the_person_was_asked_about_can_be_allowed() {
        let now = Instant::now();
        let mut target = FakeField::open(1);
        let mut dictation = Dictation::default();
        let id = dictation.begin(now).unwrap();
        dictation.advance(&mut target, now);
        // A retired prompt names its own dictation, never a successor.
        assert_eq!(
            dictation.allow(SessionId::new(id.get().saturating_add(1)), &mut target, now),
            Err(DictationError::StaleSession)
        );
        assert!(dictation.recognition.is_none());
        assert_eq!(dictation.status(), DictationStatus::Consent);
        // The person's own answer, for the dictation they were asked about,
        // is the only thing that starts the recognizer.
        let _ = dictation.allow(id, &mut target, now);
        assert_eq!(
            dictation.id(),
            Some(id),
            "the dictation keeps its identity while it runs or fails"
        );
    }

    #[test]
    fn a_field_that_moved_before_insertion_gets_no_text() {
        let now = Instant::now();
        let mut moved = FakeField::moved();
        let mut dictation = bound("spoken words");
        assert_eq!(
            dictation.finish(SessionId::new(1), &mut moved, now),
            Err(DictationError::TargetChanged)
        );
        assert!(moved.inserted.is_empty(), "nothing is written into the new field");
        assert_eq!(
            dictation.status(),
            DictationStatus::Unavailable(DictationError::TargetChanged)
        );
    }

    #[test]
    fn a_field_that_refused_the_text_keeps_it_out() {
        let now = Instant::now();
        let mut secret = FakeField::open(1);
        secret.field = Err(FieldRefusal::Secret);
        let mut dictation = bound("spoken words");
        assert_eq!(
            dictation.finish(SessionId::new(1), &mut secret, now),
            Err(DictationError::SensitiveField)
        );
        assert!(secret.inserted.is_empty());
    }

    #[test]
    fn unchanged_field_takes_the_text_once() {
        let now = Instant::now();
        let mut target = FakeField::open(1);
        let mut dictation = bound("spoken words");
        dictation
            .finish(SessionId::new(1), &mut target, now)
            .unwrap();
        assert_eq!(
            target.inserted,
            vec![(
                Field {
                    id: 7,
                    revision: 1,
                    multiline: true
                },
                "spoken words".to_owned()
            )]
        );
        assert_eq!(dictation.status(), DictationStatus::Idle);
    }

    #[test]
    fn a_transcript_too_long_for_a_field_is_kept_for_the_person_to_copy() {
        let now = Instant::now();
        let mut target = FakeField::open(1);
        let long = "a".repeat(MAX_COMMIT_BYTES.saturating_add(1));
        let mut dictation = bound(&long);
        dictation
            .finish(SessionId::new(1), &mut target, now)
            .unwrap();
        assert!(target.inserted.is_empty(), "a field never takes half a text");
        assert_eq!(
            dictation.recovery(SessionId::new(1)).map(str::len),
            Some(long.len())
        );
        assert_eq!(
            dictation.status(),
            DictationStatus::Recovery {
                reason: RecoveryReason::TooLong,
                copied: false,
                copy_failed: false
            }
        );
        // Only copying takes the transcript out, and it says whether it worked.
        dictation.note_copied(SessionId::new(1), false);
        assert_eq!(
            dictation.status(),
            DictationStatus::Recovery {
                reason: RecoveryReason::TooLong,
                copied: false,
                copy_failed: true
            }
        );
        dictation.note_copied(SessionId::new(1), true);
        assert!(dictation.discard_recovery_of(SessionId::new(1)));
        assert_eq!(dictation.status(), DictationStatus::Idle);
    }

    #[test]
    fn a_draft_with_a_control_character_never_reaches_a_field() {
        let now = Instant::now();
        let mut target = FakeField::open(1);
        let mut dictation = bound("");
        assert_eq!(
            dictation.update(SessionId::new(1), "two\nlines".into(), &mut target, now),
            Ok(()),
            "a multiline field takes a line feed"
        );
        assert_eq!(
            dictation.update(SessionId::new(1), "bell\u{7}".into(), &mut target, now),
            Err(DictationError::ControlCharacter)
        );
        assert_eq!(
            dictation.status(),
            DictationStatus::Unavailable(DictationError::ControlCharacter)
        );
    }

    #[test]
    fn a_late_answer_cannot_cancel_its_successor() {
        let now = Instant::now();
        let mut target = FakeField::open(1);
        let mut dictation = Dictation::default();
        let first = dictation.begin(now).unwrap();
        dictation.advance(&mut target, now);
        assert!(dictation.cancel(first));
        assert_eq!(dictation.status(), DictationStatus::Idle);
        let second = dictation.begin(now).unwrap();
        assert_ne!(first, second);
        dictation.advance(&mut target, now);
        assert!(!dictation.cancel(first), "the first dictation is over");
        assert_eq!(dictation.status(), DictationStatus::Consent);
    }

    #[test]
    fn a_field_that_never_asks_for_typing_ends_the_dictation() {
        let now = Instant::now();
        let mut target = FakeField::open(1);
        target.field = Err(FieldRefusal::NoField);
        let mut dictation = Dictation::default();
        dictation.begin(now).unwrap();
        dictation.advance(&mut target, now);
        assert_eq!(dictation.status(), DictationStatus::Preparing);
        dictation.advance(
            &mut target,
            now.checked_add(DELIVERY_WINDOW).unwrap(),
        );
        assert_eq!(
            dictation.status(),
            DictationStatus::Unavailable(DictationError::NoEditableField)
        );
        // The card names the dictation that failed, and closing it clears the
        // failure even though no dictation is left behind.
        let failed = dictation.last_id().expect("the dictation asked for");
        assert_eq!(dictation.id(), None);
        assert!(dictation.cancel(failed), "the person closes the failure");
        assert_eq!(dictation.status(), DictationStatus::Idle);
    }

    #[test]
    fn dictation_starts_nothing_while_a_transcript_is_kept() {
        let now = Instant::now();
        let mut target = FakeField::open(1);
        let long = "a".repeat(MAX_COMMIT_BYTES.saturating_add(1));
        let mut dictation = bound(&long);
        dictation
            .finish(SessionId::new(1), &mut target, now)
            .unwrap();
        assert_eq!(dictation.begin(now), Err(DictationError::Busy));
        dictation.discard();
        assert_eq!(dictation.status(), DictationStatus::Idle);
        assert!(dictation.recovery(SessionId::new(1)).is_none());
        dictation.begin(now).unwrap();
    }
}
