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

use smithay::reexports::wayland_protocols::wp::text_input::zv3::server::zwp_text_input_v3::{
    ContentHint, ContentPurpose,
};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::backend::ObjectId;
use smithay::wayland::text_input::TextInputSeat;

use super::input_method::TrustedInputMethod;
use super::state::State;
use super::text_commit::{DELIVERY_WINDOW, LentInputMethod, Progress};

pub(crate) mod frontend;
pub(crate) mod host;
pub mod runtime;
mod speech;
pub use speech::{PhononTranscript, SpeechError, SpeechUpdate};

/// Maximum lifetime, including final recognition, without retaining audio.
// Startup + at most 105 seconds of capture + a bounded CPU finalization pass.
pub const SESSION_WINDOW: Duration = Duration::from_secs(180);
/// A strict bound on a draft's UTF-8 bytes, independent of model output.
pub const MAX_DRAFT_BYTES: usize = 16 * 1024;
/// Fits the standard 4096-byte Wayland frame, including protocol overhead.
/// Keep one final commit: splitting would lose atomic selection and undo.
pub const MAX_COMMIT_BYTES: usize = 4000;
const RECOVERY_WINDOW: Duration = Duration::from_secs(600);

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
    /// A terminal exposes process input, not a qualified editable inner field.
    UnsupportedField,
    InputMethodUnavailable,
    StaleSession,
    TargetChanged,
    TimedOut,
    DraftTooLarge,
    ControlCharacter,
    SessionIdsExhausted,
    RecognizerUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryReason {
    VocabularyChanged,
    TooLong,
    Rejected,
    UnknownDelivery,
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
    Recovery {
        reason: RecoveryReason,
        copied: bool,
        copy_failed: bool,
    },
}

struct Recognition {
    id: SessionId,
    actor: runtime::Runtime,
    phase: DictationStatus,
    language: String,
    literal: bool,
}

#[derive(PartialEq, Eq)]
struct Target {
    field: ObjectId,
    surface: ObjectId,
    revision: u64,
    multiline: bool,
    owner: TrustedInputMethod,
    private: Option<super::private_text_input::Identity>,
}

struct Session {
    id: SessionId,
    native_deferred: bool,
    native_start: Option<ui::webview::text_input::Receipt>,
    native_warm: Option<ui::webview::text_input::Receipt>,
    target: Target,
    deadline: Instant,
    draft: zeroize::Zeroizing<String>,
}

// Private bounded transcript; never part of workspace state or diagnostic output.
struct Recovery {
    reason: RecoveryReason,
    id: SessionId,
    text: zeroize::Zeroizing<String>,
    deadline: Instant,
    copied: bool,
    copy_failed: bool,
}

struct NativeDelivery {
    requires_ack: bool,
    recovery: Recovery,
    receipt: Option<ui::webview::text_input::Receipt>,
}

struct Preparation {
    id: SessionId,
    surface: ObjectId,
    deadline: Instant,
    input_method: Option<LentInputMethod>,
    warm: Option<ui::webview::text_input::Receipt>,
    private: Option<super::private_text_input::Identity>,
}
impl Drop for Preparation {
    fn drop(&mut self) {
        if let Some(warm) = self.warm.take() {
            warm.cancel();
        }
    }
}

struct FieldStart {
    id: SessionId,
    language: String,
    receipt: ui::webview::text_input::Receipt,
}

#[derive(Default)]
pub(crate) struct Dictation {
    next: u64,
    session: Option<Session>,
    preparation: Option<Preparation>,
    recognition: Option<Recognition>,
    last_failure: Option<runtime::RuntimeError>,
    native_commit_pending: bool,
    native_field_deferred: bool,
    native_delivery: Option<NativeDelivery>,
    field_start: Option<FieldStart>,
    recovery: Option<Recovery>,
    vocabulary: Option<std::sync::Arc<dyn settings_store::personal_dictionary::Vocabulary>>,
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
    /// The authenticated session host supplies the reader, never an app or model.
    /// Rebinding invalidates queued words and original-field leases before any
    /// new person's reader can be used. Filesystem work stays on the actor.
    pub fn set_dictation_vocabulary(
        &mut self,
        reader: Option<std::sync::Arc<dyn settings_store::personal_dictionary::Vocabulary>>,
    ) {
        self.discard_dictation_recovery();
        self.text_commit.dictation.vocabulary = reader;
    }
    /// A private live hypothesis for the original local input session only.
    /// It is not workspace state and must not enter diagnostics or history.
    pub(crate) fn dictation_preview(&self, id: SessionId) -> Option<&str> {
        self.text_commit
            .dictation
            .session
            .as_ref()
            .filter(|session| session.id == id)
            .map(|session| session.draft.as_str())
    }
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
            || self.text_commit.dictation.native_delivery.is_some()
            || self.text_commit.dictation.recovery.is_some()
        {
            return Err(DictationError::Busy);
        }
        let surface = self
            .seat
            .text_input()
            .focus()
            .filter(Resource::is_alive)
            .ok_or(DictationError::NoEditableField)?;
        let private = self.private_text_input.identity(&surface.id());
        if self.input_method_owner_kind().is_some() && private.is_none() {
            return self.begin_dictation(now);
        }
        let id = self.text_commit.dictation.allocate_id()?;
        let input_method = if self.input_method_owner_kind().is_some() {
            None
        } else {
            Some(
                LentInputMethod::connect(self)
                    .map_err(|_| DictationError::InputMethodUnavailable)?,
            )
        };
        let warm = private
            .as_ref()
            .map(|identity| {
                self.private_text_input
                    .request(
                        identity,
                        super::private_text_input::protocol::tilo_text_input_v1::Kind::Prepare,
                    )
                    .ok_or(DictationError::TargetChanged)
            })
            .transpose()?;
        self.text_commit.dictation.preparation = Some(Preparation {
            id,
            surface: surface.id(),
            deadline: now.checked_add(DELIVERY_WINDOW).unwrap_or(now),
            input_method,
            warm,
            private,
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
        if preparation.private.is_some()
            && (self.private_text_input.identity(&preparation.surface) != preparation.private
                || preparation
                    .warm
                    .as_ref()
                    .is_some_and(ui::webview::text_input::Receipt::was_canceled))
        {
            return Err(DictationError::TargetChanged);
        }
        let warmed = match preparation
            .warm
            .as_ref()
            .and_then(ui::webview::text_input::Receipt::poll)
        {
            Some(Ok(_)) => true,
            Some(Err(_)) => return Err(DictationError::TargetChanged),
            None => preparation.warm.is_none(),
        };
        let ready = warmed
            && match preparation.input_method.as_mut().map(LentInputMethod::pump) {
                Some(Progress::Waiting) => false,
                Some(Progress::Active) => true,
                Some(Progress::Finished | Progress::Failed) => {
                    return Err(DictationError::InputMethodUnavailable);
                }
                None => match self.dictation_target() {
                    Ok(_) => true,
                    // The client may still be enabling its original field.
                    Err(DictationError::NoEditableField) => false,
                    // An acknowledged preparation cannot turn an explicit
                    // target refusal into a wait, or authorize a later mode.
                    Err(error) => return Err(error),
                },
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
                native_deferred: target.private.is_some()
                    || (self.shell_field_active
                        && self.text_commit.dictation.native_field_deferred),
                native_start: None,
                native_warm: self
                    .text_commit
                    .dictation
                    .preparation
                    .as_mut()
                    .and_then(|preparation| preparation.warm.take()),
                target,
                deadline: now.checked_add(SESSION_WINDOW).unwrap_or(now),
                draft: zeroize::Zeroizing::new(String::new()),
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
            || self.text_commit.dictation.native_delivery.is_some()
            || self.text_commit.dictation.recovery.is_some()
        {
            return Err(DictationError::Busy);
        }
        let target = self.dictation_target()?;
        let native_deferred = target.private.is_some()
            || (self.shell_field_active && self.text_commit.dictation.native_field_deferred);
        let dictation = &mut self.text_commit.dictation;
        let id = dictation.allocate_id()?;
        dictation.session = Some(Session {
            id,
            native_deferred,
            native_start: None,
            native_warm: None,
            target,
            deadline: now.checked_add(SESSION_WINDOW).unwrap_or(now),
            draft: zeroize::Zeroizing::new(String::new()),
        });
        Ok(id)
    }

    /// Replace the ephemeral hypothesis, including recognizer corrections.
    /// Nothing is inserted until `finish_dictation`.
    ///
    /// # Errors
    /// Rejects late callbacks, changed targets, expired sessions, oversized
    /// output, and control characters. LF is allowed only for an original
    /// explicitly multiline field; it is never a terminal key or paste.
    /// A failure on the current session cancels it.
    pub fn update_dictation(
        &mut self,
        id: SessionId,
        draft: String,
        now: Instant,
    ) -> Result<(), DictationError> {
        let draft = zeroize::Zeroizing::new(draft);
        self.validate_dictation(id, now)?;
        let multiline = self
            .text_commit
            .dictation
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
            for receipt in [session.native_start.as_ref(), session.native_warm.as_ref()]
                .into_iter()
                .flatten()
            {
                receipt.cancel();
            }
            self.text_commit.dictation.recovery = Some(Recovery {
                reason: RecoveryReason::TooLong,
                id,
                text: session.draft,
                deadline: now.checked_add(RECOVERY_WINDOW).unwrap_or(now),
                copied: false,
                copy_failed: false,
            });
            self.cancel_active_dictation();
            return Ok(());
        }
        if session.draft.is_empty() {
            for receipt in [session.native_start.as_ref(), session.native_warm.as_ref()]
                .into_iter()
                .flatten()
            {
                receipt.cancel();
            }
            self.cancel_active_dictation();
            return Ok(());
        }
        // Emit the Wayland final on this compositor thread. Deferred native
        // owners additionally retain this original draft until their private ACK;
        // the receiving guard never acquires a replacement target for this final.
        let private = session.target.private.as_ref().map(|identity| {
            self.private_text_input
                .request(
                    identity,
                    super::private_text_input::protocol::tilo_text_input_v1::Kind::Final,
                )
                .unwrap_or_else(|| {
                    ui::webview::text_input::Receipt::failed(
                        ui::webview::text_input::Failure::Rejected,
                    )
                })
        });
        let may_deliver = private
            .as_ref()
            .is_none_or(|receipt| receipt.poll().is_none());
        if !may_deliver {
            for receipt in [session.native_start.as_ref(), session.native_warm.as_ref()]
                .into_iter()
                .flatten()
            {
                receipt.cancel();
            }
        }
        if may_deliver {
            self.seat.text_input().with_active_text_input(|field, _| {
                field.commit_string(Some(session.draft.to_string()));
            });
            self.seat.text_input().done(false);
        }
        if self.shell_field_active || private.is_some() {
            self.text_commit.dictation.native_delivery = Some(NativeDelivery {
                requires_ack: session.native_deferred,
                recovery: Recovery {
                    reason: RecoveryReason::UnknownDelivery,
                    id,
                    text: session.draft,
                    deadline: now.checked_add(Duration::from_secs(10)).unwrap_or(now),
                    copied: false,
                    copy_failed: false,
                },
                receipt: private,
            });
        }
        self.cancel_active_dictation();
        // Native fields are on this same compositor thread. The native loop
        // must drain and dispatch their bridge before another input turn can
        // change the selected range. Keep successors out until that happens.
        self.text_commit.dictation.native_commit_pending = self.shell_field_active;
        Ok(())
    }

    /// Explicit local export only: never paste or retarget an insertion.
    /// Failure retains the complete original transcript for another attempt.
    pub(crate) fn copy_dictation_recovery(
        &mut self,
        id: SessionId,
        now: Instant,
    ) -> Result<(), DictationError> {
        self.expire_dictation_recovery(now);
        let recovery = self
            .text_commit
            .dictation
            .recovery
            .as_mut()
            .filter(|recovery| recovery.id == id)
            .ok_or(DictationError::StaleSession)?;
        let copied = ui::text_selection::try_clipboard_copy(&recovery.text);
        recovery.copied |= copied;
        recovery.copy_failed = !copied;
        Ok(())
    }

    pub(crate) fn discard_dictation_recovery(&mut self) {
        if let Some(delivery) = self.text_commit.dictation.native_delivery.take()
            && let Some(receipt) = delivery.receipt
        {
            receipt.cancel();
        }
        self.text_commit.dictation.recovery = None;
        self.cancel_active_dictation();
    }

    pub(crate) fn expire_dictation_recovery(&mut self, now: Instant) {
        if self
            .text_commit
            .dictation
            .recovery
            .as_ref()
            .is_some_and(|r| now >= r.deadline)
        {
            self.text_commit.dictation.recovery = None;
            self.text_commit.dictation.last_failure = Some(runtime::RuntimeError::TimedOut);
        }
    }

    pub(crate) fn set_native_dictation_deferred(&mut self, required: bool) {
        self.text_commit.dictation.native_field_deferred = required;
    }

    pub(crate) fn native_dictation_requires_ack(&self) -> bool {
        self.text_commit
            .dictation
            .native_delivery
            .as_ref()
            .is_some_and(|delivery| delivery.requires_ack)
    }

    /// Called in the same native dispatch turn. None acknowledges a synchronous
    /// native edit; browser receipts remain private until the original reply.
    pub(crate) fn acknowledge_native_dictation(
        &mut self,
        receipt: Option<ui::webview::text_input::Receipt>,
    ) {
        if let Some(delivery) = self.text_commit.dictation.native_delivery.as_mut() {
            if let Some(receipt) = receipt {
                delivery.receipt = Some(receipt);
            } else if delivery.requires_ack {
                delivery.receipt = Some(ui::webview::text_input::Receipt::failed(
                    ui::webview::text_input::Failure::Rejected,
                ));
            } else {
                self.text_commit.dictation.native_delivery = None;
            }
        }
    }

    pub(crate) fn poll_native_dictation_delivery(&mut self, now: Instant) {
        let Some(delivery) = self.text_commit.dictation.native_delivery.as_ref() else {
            return;
        };
        let outcome = delivery
            .receipt
            .as_ref()
            .and_then(ui::webview::text_input::Receipt::poll);
        if outcome.is_none() && now < delivery.recovery.deadline {
            return;
        }
        let Some(mut delivery) = self.text_commit.dictation.native_delivery.take() else {
            return;
        };
        if !matches!(outcome, Some(Ok(None))) {
            if outcome.is_none()
                && let Some(receipt) = delivery.receipt.as_ref()
            {
                receipt.cancel();
            }
            delivery.recovery.reason = if matches!(
                outcome,
                Some(Err(ui::webview::text_input::Failure::Rejected
                    | ui::webview::text_input::Failure::Retired
                    | ui::webview::text_input::Failure::Busy))
            ) {
                RecoveryReason::Rejected
            } else {
                RecoveryReason::UnknownDelivery
            };
            delivery.recovery.deadline = now.checked_add(RECOVERY_WINDOW).unwrap_or(now);
            self.text_commit.dictation.recovery = Some(delivery.recovery);
        }
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
            || self
                .text_commit
                .dictation
                .recovery
                .as_ref()
                .is_some_and(|r| r.id == id)
            || self
                .text_commit
                .dictation
                .native_delivery
                .as_ref()
                .is_some_and(|delivery| delivery.recovery.id == id)
        {
            self.discard_dictation_recovery();
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
        if let Some(start) = self.text_commit.dictation.field_start.take() {
            start.receipt.cancel();
        }
        if let Some(session) = self.text_commit.dictation.session.take() {
            for receipt in [session.native_start, session.native_warm]
                .into_iter()
                .flatten()
            {
                receipt.cancel();
            }
        }
        self.text_commit.dictation.preparation = None;
        if let Some(recognition) = self.text_commit.dictation.recognition.as_mut() {
            recognition.actor.cancel();
            recognition.phase = DictationStatus::Stopping;
        }
    }

    /// An ordinary dictionary edit stops recognition but does not silently
    /// discard words already shown to this person. Never insert a partial draft.
    fn retain_dictionary_edit_draft(&mut self, id: SessionId, now: Instant) {
        let text = self
            .text_commit
            .dictation
            .session
            .as_mut()
            .filter(|session| session.id == id)
            .map(|session| std::mem::take(&mut session.draft));
        self.cancel_active_dictation();
        if let Some(text) = text.filter(|text| !text.is_empty()) {
            self.text_commit.dictation.recovery = Some(Recovery {
                reason: RecoveryReason::VocabularyChanged,
                id,
                text,
                deadline: now.checked_add(RECOVERY_WINDOW).unwrap_or(now),
                copied: false,
                copy_failed: false,
            });
        }
    }

    pub(crate) fn native_dictation_start_required(&mut self, id: SessionId, now: Instant) -> bool {
        self.validate_dictation(id, now).is_ok()
            && self
                .text_commit
                .dictation
                .session
                .as_ref()
                .is_some_and(|session| session.id == id && session.native_deferred)
    }
    pub(crate) fn standalone_dictation_start(
        &mut self,
    ) -> Option<ui::webview::text_input::Receipt> {
        let identity = self
            .text_commit
            .dictation
            .session
            .as_ref()?
            .target
            .private
            .as_ref()?;
        self.private_text_input.request(
            identity,
            super::private_text_input::protocol::tilo_text_input_v1::Kind::Start,
        )
    }

    pub(crate) fn start_dictation_after_field_ack(
        &mut self,
        id: SessionId,
        language: &str,
        now: Instant,
        receipt: Option<ui::webview::text_input::Receipt>,
    ) -> Result<(), DictationError> {
        self.validate_dictation(id, now)?;
        if !self
            .text_commit
            .dictation
            .session
            .as_ref()
            .is_some_and(|session| session.native_deferred)
        {
            return self.start_authorized_dictation(id, language, now);
        }
        if self.text_commit.dictation.field_start.is_some()
            || self.text_commit.dictation.recognition.is_some()
        {
            return Err(DictationError::Busy);
        }
        let receipt = receipt.ok_or(DictationError::TargetChanged)?;
        self.text_commit.dictation.field_start = Some(FieldStart {
            id,
            language: language.into(),
            receipt,
        });
        Ok(())
    }

    pub(crate) fn advance_authorized_dictation(
        &mut self,
        now: Instant,
    ) -> Result<(), DictationError> {
        let Some(id) = self
            .text_commit
            .dictation
            .field_start
            .as_ref()
            .map(|start| start.id)
        else {
            return Ok(());
        };
        self.validate_dictation(id, now)?;
        let Some(result) = self
            .text_commit
            .dictation
            .field_start
            .as_ref()
            .and_then(|start| start.receipt.poll())
        else {
            return Ok(());
        };
        let Some(start) = self.text_commit.dictation.field_start.take() else {
            return Ok(());
        };
        match result {
            Ok(None) => {
                if let Some(session) = self.text_commit.dictation.session.as_mut() {
                    session.native_start = Some(start.receipt);
                }
                self.launch_authorized_dictation(start.id, &start.language, now)
            }
            Err(ui::webview::text_input::Failure::Unknown) => {
                Err(DictationError::RecognizerUnavailable)
            }
            _ => Err(DictationError::TargetChanged),
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
        if self
            .text_commit
            .dictation
            .session
            .as_ref()
            .is_some_and(|session| session.native_deferred)
        {
            return Err(DictationError::TargetChanged);
        }
        self.launch_authorized_dictation(id, language, now)
    }

    fn launch_authorized_dictation(
        &mut self,
        id: SessionId,
        language: &str,
        now: Instant,
    ) -> Result<(), DictationError> {
        self.validate_dictation(id, now)?;
        if self.text_commit.dictation.recognition.is_some() {
            return Err(DictationError::Busy);
        }
        let multiline = self
            .text_commit
            .dictation
            .session
            .as_ref()
            .is_some_and(|session| session.target.multiline);
        let actor = runtime::Runtime::start_for_field(
            language,
            self.text_commit.dictation.vocabulary.clone(),
            multiline,
        )
        .map_err(|_| DictationError::RecognizerUnavailable)?;
        self.text_commit.dictation.last_failure = None;
        self.text_commit.dictation.recognition = Some(Recognition {
            id,
            actor,
            phase: DictationStatus::Loading,
            language: language.to_owned(),
            literal: settings_store::load_section("accessibility")
                .get("dictation_literal")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
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
            .map_err(|_| DictationError::RecognizerUnavailable)?;
        recognition.phase = DictationStatus::StoppingRecording;
        Ok(())
    }

    pub fn dictation_status(&self) -> DictationStatus {
        if self.text_commit.dictation.field_start.is_some() {
            return DictationStatus::Loading;
        }
        if self.text_commit.dictation.native_delivery.is_some() {
            return DictationStatus::Finishing;
        }
        if let Some(recovery) = &self.text_commit.dictation.recovery {
            return DictationStatus::Recovery {
                reason: recovery.reason,
                copied: recovery.copied,
                copy_failed: recovery.copy_failed,
            };
        }
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
        if self.has_dictation_target()
            || self.text_commit.dictation.recognition.is_some()
            || self.text_commit.dictation.native_delivery.is_some()
        {
            return now.checked_add(Duration::from_millis(50));
        }
        self.text_commit
            .dictation
            .recovery
            .as_ref()
            .map(|recovery| recovery.deadline.max(now))
    }

    /// Poll on every native service tick, even without paint/input events.
    /// A canceled actor occupies its slot until actual capture/worker cleanup.
    pub(crate) fn pump_dictation_worker(&mut self, now: Instant) {
        self.poll_native_dictation_delivery(now);
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
                    let recognized = zeroize::Zeroizing::new(text);
                    let text = agent_provider_config::speech::clean_final(
                        &recognized,
                        &recognition.language,
                        recognition.literal,
                    )
                    .into_owned();
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
                    if error == runtime::RuntimeError::VocabularyChanged {
                        self.retain_dictionary_edit_draft(recognition.id, now);
                    } else {
                        self.cancel_active_dictation();
                    }
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
                let content = self.text_fields.content_type(field);
                target = if content.is_sensitive() {
                    Err(DictationError::SensitiveField)
                } else if content.purpose == u32::from(ContentPurpose::Terminal) {
                    // Terminal purpose does not identify an inner program's edit,
                    // command or secret mode. Neither a caret nor a private
                    // receipt supplies that contract. Refuse speech before capture
                    // and on every revalidation; ordinary keyboard IME is separate.
                    Err(DictationError::UnsupportedField)
                } else {
                    self.text_fields
                        .revision(field)
                        .map(|revision| Target {
                            field: field.id(),
                            surface: surface.id(),
                            revision,
                            multiline: content.hint & u32::from(ContentHint::Multiline) != 0,
                            owner,
                            private: self.private_text_input.identity(&surface.id()),
                        })
                        .ok_or(DictationError::NoEditableField)
                };
            });
        target
    }
}
