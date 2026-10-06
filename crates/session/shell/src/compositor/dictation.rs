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

pub(crate) mod frontend;
pub mod runtime;
mod speech;
pub use speech::{PhononTranscript, SpeechError, SpeechUpdate};

/// Maximum lifetime, including final recognition, without retaining audio.
pub const SESSION_WINDOW: Duration = Duration::from_secs(120);
/// A strict bound on a draft's UTF-8 bytes, independent of model output.
pub const MAX_DRAFT_BYTES: usize = 16 * 1024;
/// Fits the standard 4096-byte Wayland frame, including protocol overhead.
/// Keep one final commit: splitting would lose atomic selection and undo.
pub const MAX_COMMIT_BYTES: usize = 4000;

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
    RecognizerUnavailable,
}

/// Status exposes no recording or draft to a remote collaborator.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DictationStatus {
    #[default]
    Idle,
    Preparing,
    Loading,
    Listening,
    StoppingRecording,
    Finishing,
    Stopping,
    Unavailable(runtime::RuntimeError),
}

struct Recognition {
    id: SessionId,
    actor: runtime::Runtime,
    phase: DictationStatus,
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
    recognition: Option<Recognition>,
    last_failure: Option<runtime::RuntimeError>,
    native_commit_pending: bool,
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
            || self.text_commit.dictation.recognition.is_some()
            || self.text_commit.dictation.native_commit_pending
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
            || self.text_commit.dictation.recognition.is_some()
            || self.text_commit.dictation.native_commit_pending
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
        if session.draft.len() > MAX_COMMIT_BYTES {
            self.cancel_active_dictation();
            return Err(DictationError::DraftTooLarge);
        }
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
        // Native fields are on this same compositor thread. The native loop
        // must drain and dispatch their bridge before another input turn can
        // change the selected range. Keep successors out until that happens.
        self.text_commit.dictation.native_commit_pending = self.shell_field_active;
        Ok(())
    }

    pub(crate) fn take_native_dictation_commit(&mut self) -> bool {
        std::mem::take(&mut self.text_commit.dictation.native_commit_pending)
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
        if let Some(recognition) = self.text_commit.dictation.recognition.as_mut() {
            recognition.actor.cancel();
            recognition.phase = DictationStatus::Stopping;
        }
    }

    /// Begin local recognition only after the trusted desktop has obtained
    /// explicit microphone consent and prepared this original edit target.
    /// Neither model readiness nor an untrusted action request is consent.
    /// # Errors
    /// Fails closed on a changed field, another actor or unavailable worker.
    pub fn start_authorized_dictation(
        &mut self,
        id: SessionId,
        language: &str,
        now: Instant,
    ) -> Result<(), DictationError> {
        self.validate_dictation(id, now)?;
        if self.text_commit.dictation.recognition.is_some() {
            return Err(DictationError::Busy);
        }
        let actor =
            runtime::Runtime::start(language).map_err(|_| DictationError::RecognizerUnavailable)?;
        self.text_commit.dictation.last_failure = None;
        self.text_commit.dictation.recognition = Some(Recognition {
            id,
            actor,
            phase: DictationStatus::Loading,
        });
        Ok(())
    }

    /// Stop the microphone and request one final complete corrected result.
    /// # Errors
    /// Rejects a stale or unready session without starting another recording.
    pub fn finish_recording(&mut self, id: SessionId) -> Result<(), DictationError> {
        let recognition = self
            .text_commit
            .dictation
            .recognition
            .as_mut()
            .filter(|recognition| recognition.id == id)
            .ok_or(DictationError::StaleSession)?;
        if recognition.phase != DictationStatus::Listening {
            return Err(DictationError::Busy);
        }
        recognition
            .actor
            .finish()
            .map_err(|_| DictationError::RecognizerUnavailable)?;
        recognition.phase = DictationStatus::StoppingRecording;
        Ok(())
    }

    pub fn dictation_status(&self) -> DictationStatus {
        if let Some(recognition) = &self.text_commit.dictation.recognition {
            return recognition.phase;
        }
        if self.text_commit.dictation.preparation.is_some() {
            return DictationStatus::Preparing;
        }
        self.text_commit
            .dictation
            .last_failure
            .map_or(DictationStatus::Idle, DictationStatus::Unavailable)
    }

    pub(crate) fn has_dictation_target(&self) -> bool {
        self.dictation_target_id().is_some()
    }

    pub(crate) fn dictation_target_id(&self) -> Option<SessionId> {
        self.text_commit
            .dictation
            .session
            .as_ref()
            .map(|session| session.id)
            .or_else(|| {
                self.text_commit
                    .dictation
                    .preparation
                    .as_ref()
                    .map(|preparation| preparation.id)
            })
    }

    /// Wake an otherwise idle compositor while a private actor or target lives.
    /// No periodic work is scheduled while dictation is inactive.
    pub(crate) fn dictation_service_deadline(&self, now: Instant) -> Option<Instant> {
        (self.has_dictation_target() || self.text_commit.dictation.recognition.is_some())
            .then(|| now.checked_add(Duration::from_millis(50)))
            .flatten()
    }

    /// Poll on every native service tick, even without paint/input events.
    /// A canceled actor occupies its slot until actual capture/worker cleanup.
    pub(crate) fn pump_dictation_worker(&mut self, now: Instant) {
        let Some(mut recognition) = self.text_commit.dictation.recognition.take() else {
            return;
        };
        if self.validate_dictation(recognition.id, now).is_err() {
            recognition.actor.cancel();
            recognition.phase = DictationStatus::Stopping;
        }
        while let Some(event) = recognition.actor.poll() {
            let result = match event {
                runtime::RuntimeEvent::Ready if recognition.phase == DictationStatus::Loading => {
                    self.validate_dictation(recognition.id, now).and_then(|()| {
                        recognition
                            .actor
                            .capture()
                            .map_err(|_| DictationError::RecognizerUnavailable)
                    })
                }
                runtime::RuntimeEvent::Listening
                    if recognition.phase == DictationStatus::Loading =>
                {
                    recognition.phase = DictationStatus::Listening;
                    Ok(())
                }
                runtime::RuntimeEvent::Finishing
                    if matches!(
                        recognition.phase,
                        DictationStatus::Listening | DictationStatus::StoppingRecording
                    ) =>
                {
                    recognition.phase = DictationStatus::Finishing;
                    Ok(())
                }
                runtime::RuntimeEvent::Draft(text)
                    if matches!(
                        recognition.phase,
                        DictationStatus::Listening
                            | DictationStatus::StoppingRecording
                            | DictationStatus::Finishing
                    ) =>
                {
                    self.update_dictation(recognition.id, text, now)
                }
                runtime::RuntimeEvent::Done(text)
                    if matches!(
                        recognition.phase,
                        DictationStatus::Listening
                            | DictationStatus::StoppingRecording
                            | DictationStatus::Finishing
                    ) =>
                {
                    let result = self
                        .update_dictation(recognition.id, text, now)
                        .and_then(|()| self.finish_dictation(recognition.id, now));
                    recognition.phase = DictationStatus::Stopping;
                    result
                }
                runtime::RuntimeEvent::Stopped(error) => {
                    if error != runtime::RuntimeError::Canceled {
                        self.text_commit.dictation.last_failure = Some(error);
                    }
                    self.cancel_active_dictation();
                    recognition.phase = DictationStatus::Stopping;
                    Ok(())
                }
                _ => Err(DictationError::StaleSession),
            };
            if result.is_err() {
                recognition.actor.cancel();
                recognition.phase = DictationStatus::Stopping;
                self.cancel_active_dictation();
            }
        }
        if !recognition.actor.is_stopped() {
            self.text_commit.dictation.recognition = Some(recognition);
        } else if self.text_commit.dictation.session.is_some() {
            // No terminal reply (including a full/disconnected result queue)
            // is never success. Do not retain a draft or leave a session live.
            self.text_commit.dictation.last_failure = Some(runtime::RuntimeError::Worker(
                agent_provider_config::speech::SpeechError::WorkerStopped,
            ));
            self.cancel_active_dictation();
        }
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
