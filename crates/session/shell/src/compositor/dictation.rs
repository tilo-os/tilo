//! Guarded compositor insertion for a local speech recognizer.
//!
//! Drafts stay in this session, rather than repeatedly rewriting an app's
//! CRDT or deleting text with synthetic backspaces. A final result is committed
//! once through the existing trusted input-method session. No clipboard,
//! keyboard injection, surrounding-text collection, recording, or networking.
//!
//! This is the insertion boundary, not a microphone or model service. Callers
//! must stop their capture/recognizer on cancellation or any error, and establish
//! an input-method session before starting. Field identity and its committed
//! revision bind results to the original edit position, including switches
//! between fields sharing a Wayland surface and text-input object.

use std::time::{Duration, Instant};

use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::backend::ObjectId;
use smithay::wayland::text_input::TextInputSeat;

use super::input_method::TrustedInputMethod;
use super::state::State;
use super::text_commit::{DELIVERY_WINDOW, LentInputMethod, Progress};

mod speech;
pub use speech::{PhononTranscript, SpeechError, SpeechUpdate};

/// Maximum lifetime, including final recognition, without retaining audio.
pub const SESSION_WINDOW: Duration = Duration::from_secs(120);
/// A strict bound on a draft's UTF-8 bytes, independent of model output.
pub const MAX_DRAFT_BYTES: usize = 16 * 1024;

/// An explicitly started microphone stream, owned by the trusted speech worker.
/// Drop stops and reaps only its `PipeWire` process and joins its reader. Start
/// only after user authorization, model readiness and field activation; perform
/// it off the compositor thread because source probing is bounded blocking I/O.
pub struct DictationMicrophone(super::audio_capture::AudioCapture);

impl DictationMicrophone {
    /// Request 16 kHz mono S16LE from the current `PipeWire` microphone.
    ///
    /// # Errors
    /// Fails on denied/unavailable source access or failed process startup.
    pub fn start() -> Result<Self, String> {
        super::audio_capture::AudioCapture::start_dictation_microphone().map(Self)
    }

    /// Drain complete samples. Queue overflow is a fatal discontinuity, never
    /// a silent gap from which a recognizer could manufacture missing words.
    ///
    /// # Errors
    /// Fails when the microphone process exits or any audio chunk was lost.
    pub fn drain(&mut self) -> Result<Vec<u8>, String> {
        self.0.drain(true)
    }
}

/// Identifies one recognizer request. Late callbacks cannot affect its successor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionId(u64);

/// Failures contain no audio, transcript, or field contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DictationError {
    Busy,
    NoEditableField,
    SensitiveField,
    InputMethodUnavailable,
    StaleSession,
    TargetChanged,
    TimedOut,
    DraftTooLarge,
    ControlCharacter,
    SessionIdsExhausted,
}

#[derive(PartialEq, Eq)]
struct Target {
    field: ObjectId,
    surface: ObjectId,
    revision: u64,
    owner: TrustedInputMethod,
}

struct Session {
    id: SessionId,
    target: Target,
    deadline: Instant,
    draft: String,
}

struct Preparation {
    id: SessionId,
    surface: ObjectId,
    deadline: Instant,
    input_method: LentInputMethod,
}

#[derive(Default)]
pub(crate) struct Dictation {
    next: u64,
    session: Option<Session>,
    preparation: Option<Preparation>,
}

impl Dictation {
    fn allocate_id(&mut self) -> Result<SessionId, DictationError> {
        self.next = self
            .next
            .checked_add(1)
            .ok_or(DictationError::SessionIdsExhausted)?;
        Ok(SessionId(self.next))
    }
}

impl State {
    /// Prepare a session before opening the microphone. On an idle seat,
    /// lend the compositor an in-process input method using the existing private
    /// credential. An existing OSK or composing IME retains ownership.
    /// Pump `advance_dictation_preparation` until ready before recording.
    ///
    /// # Errors
    /// Fails on absent focus or an unavailable trusted connection. Starting
    /// while another request is preparing or recording returns `Busy`.
    pub fn prepare_dictation(&mut self, now: Instant) -> Result<SessionId, DictationError> {
        if self.text_commit.dictation.session.is_some()
            || self.text_commit.dictation.preparation.is_some()
        {
            return Err(DictationError::Busy);
        }
        if self.input_method_owner_kind().is_some() {
            return self.begin_dictation(now);
        }
        let surface = self
            .seat
            .text_input()
            .focus()
            .filter(Resource::is_alive)
            .ok_or(DictationError::NoEditableField)?;
        let id = self.text_commit.dictation.allocate_id()?;
        let input_method =
            LentInputMethod::connect(self).map_err(|_| DictationError::InputMethodUnavailable)?;
        self.text_commit.dictation.preparation = Some(Preparation {
            id,
            surface: surface.id(),
            deadline: now.checked_add(DELIVERY_WINDOW).unwrap_or(now),
            input_method,
        });
        Ok(id)
    }

    /// Pump activation without blocking; `true` means recording may begin.
    /// A late tick cannot prepare a successor or redirect a canceled request.
    ///
    /// # Errors
    /// Fails on cancellation, focus loss, timeout, sensitive fields or a failed
    /// input-method connection. Matching failed requests release their lease.
    pub fn advance_dictation_preparation(
        &mut self,
        id: SessionId,
        now: Instant,
    ) -> Result<bool, DictationError> {
        if self
            .text_commit
            .dictation
            .session
            .as_ref()
            .is_some_and(|s| s.id == id)
        {
            return self.validate_dictation(id, now).map(|()| true);
        }
        if !self
            .text_commit
            .dictation
            .preparation
            .as_ref()
            .is_some_and(|p| p.id == id)
        {
            return Err(DictationError::StaleSession);
        }
        let mut preparation = self
            .text_commit
            .dictation
            .preparation
            .take()
            .ok_or(DictationError::StaleSession)?;
        if !self
            .seat
            .text_input()
            .focus()
            .is_some_and(|surface| surface.is_alive() && surface.id() == preparation.surface)
        {
            return Err(DictationError::TargetChanged);
        }
        if now >= preparation.deadline {
            return Err(DictationError::TimedOut);
        }
        let ready = match preparation.input_method.pump() {
            Progress::Waiting => false,
            Progress::Active => true,
            Progress::Finished | Progress::Failed => {
                return Err(DictationError::InputMethodUnavailable);
            }
        };
        self.text_commit.dictation.preparation = Some(preparation);
        if ready {
            let target = match self.dictation_target() {
                Ok(target) => target,
                Err(error) => {
                    self.cancel_active_dictation();
                    return Err(error);
                }
            };
            self.text_commit.dictation.session = Some(Session {
                id,
                target,
                deadline: now.checked_add(SESSION_WINDOW).unwrap_or(now),
                draft: String::new(),
            });
        }
        Ok(ready)
    }

    /// Start only against an enabled, non-secret field. This grants no mic access.
    ///
    /// # Errors
    /// Returns a typed failure when the field or trusted input method is absent,
    /// the field is secret, or another session is still running.
    pub fn begin_dictation(&mut self, now: Instant) -> Result<SessionId, DictationError> {
        if self.text_commit.dictation.session.is_some()
            || self.text_commit.dictation.preparation.is_some()
        {
            return Err(DictationError::Busy);
        }
        let target = self.dictation_target()?;
        let dictation = &mut self.text_commit.dictation;
        let id = dictation.allocate_id()?;
        dictation.session = Some(Session {
            id,
            target,
            deadline: now.checked_add(SESSION_WINDOW).unwrap_or(now),
            draft: String::new(),
        });
        Ok(id)
    }

    /// Replace the ephemeral hypothesis, including recognizer corrections.
    /// Nothing is inserted until `finish_dictation`.
    ///
    /// # Errors
    /// Rejects late callbacks, changed targets, expired sessions, oversized
    /// output, and control characters (which could execute a terminal command).
    /// A failure on the current session cancels it.
    pub fn update_dictation(
        &mut self,
        id: SessionId,
        draft: String,
        now: Instant,
    ) -> Result<(), DictationError> {
        self.validate_dictation(id, now)?;
        let error = if draft.len() > MAX_DRAFT_BYTES {
            Some(DictationError::DraftTooLarge)
        } else if draft.chars().any(char::is_control) {
            Some(DictationError::ControlCharacter)
        } else {
            None
        };
        if let Some(error) = error {
            self.cancel_active_dictation();
            return Err(error);
        }
        if let Some(session) = self.text_commit.dictation.session.as_mut() {
            session.draft = draft;
        }
        Ok(())
    }

    /// Commit the latest corrected hypothesis once, using native UTF-8 text.
    /// Empty recognition ends the session without sending a protocol edit.
    /// Dictated text never enters the emoji picker's recents.
    ///
    /// # Errors
    /// Returns a failure if the request expired or its original edit target
    /// changed. Failure cancels the current session and inserts nothing.
    pub fn finish_dictation(&mut self, id: SessionId, now: Instant) -> Result<(), DictationError> {
        self.validate_dictation(id, now)?;
        let session = self
            .text_commit
            .dictation
            .session
            .take()
            .ok_or(DictationError::StaleSession)?;
        if session.draft.is_empty() {
            self.cancel_active_dictation();
            return Ok(());
        }
        // Validation and commit run together on the compositor thread. There
        // is no delayed request that could re-target itself while waiting.
        self.seat.text_input().with_active_text_input(|field, _| {
            field.commit_string(Some(session.draft.clone()));
        });
        self.seat.text_input().done(false);
        self.cancel_active_dictation();
        Ok(())
    }

    /// Cancel one request. A delayed cancellation cannot cancel its successor.
    pub fn cancel_dictation(&mut self, id: SessionId) -> bool {
        if self
            .text_commit
            .dictation
            .session
            .as_ref()
            .is_some_and(|session| session.id == id)
            || self
                .text_commit
                .dictation
                .preparation
                .as_ref()
                .is_some_and(|p| p.id == id)
        {
            self.cancel_active_dictation();
            true
        } else {
            false
        }
    }

    /// Check liveness each service tick so capture can stop even without a result.
    ///
    /// # Errors
    /// Uses the same target and lifetime checks as insertion.
    pub fn check_dictation(&mut self, id: SessionId, now: Instant) -> Result<(), DictationError> {
        self.validate_dictation(id, now)
    }

    pub(crate) fn cancel_active_dictation(&mut self) {
        self.text_commit.dictation.session = None;
        self.text_commit.dictation.preparation = None;
    }

    fn validate_dictation(&mut self, id: SessionId, now: Instant) -> Result<(), DictationError> {
        let session = self
            .text_commit
            .dictation
            .session
            .as_ref()
            .filter(|session| session.id == id)
            .ok_or(DictationError::StaleSession)?;
        let result = if now >= session.deadline {
            Err(DictationError::TimedOut)
        } else {
            self.dictation_target().and_then(|target| {
                if target == session.target {
                    Ok(())
                } else {
                    Err(DictationError::TargetChanged)
                }
            })
        };
        if result.is_err() {
            self.cancel_active_dictation();
        }
        result
    }

    fn dictation_target(&self) -> Result<Target, DictationError> {
        let owner = self
            .input_method_owner_kind()
            .or_else(|| {
                self.text_commit
                    .dictation
                    .preparation
                    .as_ref()
                    .map(|_| TrustedInputMethod::Compositor)
            })
            .ok_or(DictationError::InputMethodUnavailable)?;
        let mut target = Err(DictationError::NoEditableField);
        self.seat
            .text_input()
            .with_active_text_input(|field, surface| {
                target = if self.text_fields.content_type(field).is_sensitive() {
                    Err(DictationError::SensitiveField)
                } else {
                    self.text_fields
                        .revision(field)
                        .map(|revision| Target {
                            field: field.id(),
                            surface: surface.id(),
                            revision,
                            owner,
                        })
                        .ok_or(DictationError::NoEditableField)
                };
            });
        target
    }
}
