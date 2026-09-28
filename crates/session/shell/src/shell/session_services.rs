//! The session's recorder, replay and record custody, kept apart from chrome.
//!
//! Chrome holds plain state only. Its actions validate their arguments and
//! queue a [`ServiceIntent`]; [`SessionServices`], owned by the overlay host,
//! carries the intents out once per frame against the real services and
//! writes back what chrome shows of them as a [`SessionView`]: the status
//! `state-dump` reports, the replay transport, and what the remark surface
//! needs to know about the recent buffer and the sessions awaiting review.
//! A refusal reaches the caller through the invocation's result, and the
//! remark surface's own notice when it asked.

use std::sync::Arc;

use session_trace::frames::PixelPurpose;
use ui::record::RecordId;

use crate::shell::chrome_state::ChromeState;
use crate::shell::record_custody::RecordCustody;
use crate::shell::recorder::Recorder;
use crate::shell::remark::{Flow, Notice, Stage};
use crate::shell::replay::{Presentation, ReplayController};
use crate::shell::state::{RoomId, UserId};

/// Why a recording action refuses in a session without a broker.
pub const NO_RECORDER: &str = "this session has no collaboration broker to record through";
/// Why a replay action refuses in a session without a broker.
pub const NO_REPLAY: &str = "this session has no collaboration broker to replay through";

/// Something chrome asked of the session's services, carried out on the
/// host's next frame in the order it was asked.
#[derive(Clone, Debug)]
pub enum ServiceIntent {
    /// Record `rooms` (and only `apps`, when named) as `user`.
    StartRecording {
        rooms: Vec<String>,
        apps: Vec<String>,
        pixels: Option<PixelPurpose>,
        user: UserId,
    },
    /// Stop recording and keep the session for review.
    StopRecording {
        user: UserId,
    },
    /// Seal what the recent buffer kept.
    CaptureRecent,
    /// Turn the recent buffer on or off, and remember it.
    SetRecentBuffer {
        enabled: bool,
        user: UserId,
    },
    /// Record for the open remark; on success its flow becomes `Recording`.
    RecordForRemark {
        rooms: Vec<String>,
        pixels: PixelPurpose,
        user: UserId,
    },
    /// Stop the open remark's recording and attach the sealed session.
    StopForRemark {
        user: UserId,
    },
    /// Seal the recent buffer for the open remark.
    CaptureRecentForRemark,
    /// Forget a session waiting for review without it leaving memory.
    ForgetSession {
        record: RecordId,
    },
    /// Stop a running recording and forget what it sealed.
    StopAndForget {
        user: UserId,
    },
    /// Replay the session record `record` for `user`.
    OpenReplay {
        record: RecordId,
        user: UserId,
        presentation: Presentation,
    },
    CloseReplay,
    PlayReplay,
    PauseReplay,
    ReplayStepNext,
    ReplayStepPrevious,
    ReplaySeekStep(usize),
    ReplaySeekOffset(session_trace::Offset),
    ReplaySpeed(f64),
}

/// What chrome shows of the session's services, as of the last frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionView {
    /// `None` when the session has no recorder to record through.
    pub recorder: Option<RecorderView>,
    /// `None` when the session has no replay to replay through.
    pub replay: Option<ReplayView>,
}

/// The recorder, as chrome shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecorderView {
    /// What `state-dump` reports under `feedback`.
    pub status: serde_json::Value,
    /// Whether the recent buffer keeps the last minutes.
    pub recent_buffer: bool,
    /// Record ids of the sessions waiting for review, oldest first.
    pub awaiting_review: Vec<String>,
}

/// The replay, as chrome shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReplayView {
    /// What `state-dump` reports under `replay`; `null` when nothing is
    /// replayed.
    pub status: serde_json::Value,
    /// The room being replayed into.
    pub room: Option<RoomId>,
    /// The replays checked out of sight for review, and how far each got.
    pub checks: serde_json::Value,
}

/// The recorder, the replay and record custody, once the broker is up.
#[derive(Debug, Default)]
pub struct SessionServices {
    recorder: Option<Arc<Recorder>>,
    replay: Option<Arc<ReplayController>>,
    custody: Option<Arc<RecordCustody>>,
}

impl SessionServices {
    pub fn recorder(&self) -> Option<&Arc<Recorder>> {
        self.recorder.as_ref()
    }

    pub fn replay(&self) -> Option<&Arc<ReplayController>> {
        self.replay.as_ref()
    }

    pub fn custody(&self) -> Option<&Arc<RecordCustody>> {
        self.custody.as_ref()
    }

    pub fn set_recorder(&mut self, recorder: Arc<Recorder>) {
        self.recorder = Some(recorder);
    }

    pub fn set_replay(&mut self, replay: Arc<ReplayController>) {
        self.replay = Some(replay);
    }

    pub fn set_custody(&mut self, custody: Arc<RecordCustody>) {
        self.custody = Some(custody);
    }

    /// Carry out what chrome asked for, in order, and record each outcome
    /// where the invocation that asked reads it. Returns whether anything
    /// was asked.
    pub fn serve(&self, st: &mut ChromeState) -> bool {
        let intents = st.take_service_intents();
        if intents.is_empty() {
            return false;
        }
        for (id, intent) in intents {
            let result = self.carry_out(st, intent);
            if let Err(error) = &result {
                tracing::debug!(%error, "session services: a request was refused");
            }
            st.complete_service_intent(id, result);
        }
        st.bump();
        true
    }

    /// Write back what chrome shows of the services.
    pub fn publish(&self, st: &mut ChromeState) {
        st.set_session_view(self.view());
    }

    fn view(&self) -> SessionView {
        SessionView {
            recorder: self.recorder.as_ref().map(|recorder| RecorderView {
                status: recorder.status_json(),
                recent_buffer: recorder.recent_buffer_enabled(),
                awaiting_review: recorder
                    .awaiting_review()
                    .into_iter()
                    .map(|(checkout, _)| checkout.id.as_str().to_string())
                    .collect(),
            }),
            replay: self.replay.as_ref().map(|replay| ReplayView {
                status: replay.status_json(),
                room: replay.room(),
                checks: replay.checks_json(),
            }),
        }
    }

    fn carry_out(&self, st: &mut ChromeState, intent: ServiceIntent) -> Result<(), String> {
        match intent {
            ServiceIntent::StartRecording { .. }
            | ServiceIntent::StopRecording { .. }
            | ServiceIntent::CaptureRecent
            | ServiceIntent::SetRecentBuffer { .. } => self.recorder.as_deref().map_or_else(
                || Err(NO_RECORDER.to_string()),
                |recorder| record(recorder, intent),
            ),
            ServiceIntent::RecordForRemark { .. }
            | ServiceIntent::StopForRemark { .. }
            | ServiceIntent::CaptureRecentForRemark => match self.recorder.as_deref() {
                Some(recorder) => record_for_remark(recorder, st, intent),
                None => Err(remark_refused(st, NO_RECORDER.to_string())),
            },
            ServiceIntent::ForgetSession { .. } | ServiceIntent::StopAndForget { .. } => {
                if let Some(recorder) = self.recorder.as_deref() {
                    forget(recorder, intent);
                }
                Ok(())
            }
            ServiceIntent::OpenReplay { .. }
            | ServiceIntent::CloseReplay
            | ServiceIntent::PlayReplay
            | ServiceIntent::PauseReplay
            | ServiceIntent::ReplayStepNext
            | ServiceIntent::ReplayStepPrevious
            | ServiceIntent::ReplaySeekStep(_)
            | ServiceIntent::ReplaySeekOffset(_)
            | ServiceIntent::ReplaySpeed(_) => self
                .replay
                .as_deref()
                .map_or_else(|| Err(NO_REPLAY.to_string()), |replay| play(replay, intent)),
        }
    }
}

/// What the recorder is asked outside a remark.
fn record(recorder: &Recorder, intent: ServiceIntent) -> Result<(), String> {
    match intent {
        ServiceIntent::StartRecording {
            rooms,
            apps,
            pixels,
            user,
        } => recorder
            .start(&rooms, apps, pixels, &user)
            .map_err(|error| error.to_string()),
        ServiceIntent::StopRecording { user } => recorder
            .stop(&user)
            .map(|_| ())
            .map_err(|error| error.to_string()),
        ServiceIntent::CaptureRecent => {
            recorder.capture_recent().map_err(|error| error.to_string())
        }
        ServiceIntent::SetRecentBuffer { enabled, user } => recorder
            .set_recent_buffer(enabled, &user)
            .map_err(|error| format!("setting not saved: {error}")),
        _ => Ok(()),
    }
}

/// Forget what a remark recorded without it leaving memory.
fn forget(recorder: &Recorder, intent: ServiceIntent) {
    match intent {
        ServiceIntent::ForgetSession { record } => drop(recorder.take_session(&record)),
        ServiceIntent::StopAndForget { user } => {
            if let Ok(checkout) = recorder.stop(&user) {
                drop(recorder.take_session(&checkout.id));
            }
        }
        _ => {}
    }
}

/// What the open remark asks of the recorder; its flow follows what the
/// recorder did, and a refusal becomes its notice.
fn record_for_remark(
    recorder: &Recorder,
    st: &mut ChromeState,
    intent: ServiceIntent,
) -> Result<(), String> {
    match intent {
        ServiceIntent::RecordForRemark {
            rooms,
            pixels,
            user,
        } => {
            recorder
                .start(&rooms, Vec::new(), Some(pixels), &user)
                .map_err(|error| remark_refused(st, error.to_string()))?;
            if st.remark.is_open() {
                st.remark.flow = Flow::Recording { rooms };
                st.remark.stage = Stage::Recording;
            } else if let Ok(checkout) = recorder.stop(&user) {
                // The remark it was for was discarded before it started.
                drop(recorder.take_session(&checkout.id));
            }
            Ok(())
        }
        ServiceIntent::StopForRemark { user } => {
            let checkout = recorder
                .stop(&user)
                .map_err(|error| remark_refused(st, error.to_string()))?;
            if !st.remark.is_open() {
                // The remark it was for is gone: nothing will attach it.
                drop(recorder.take_session(&checkout.id));
                return Ok(());
            }
            let summary = recorder
                .awaiting_review()
                .into_iter()
                .find(|(pending, _)| pending.id == checkout.id)
                .map(|(_, summary)| summary)
                .ok_or_else(|| {
                    remark_refused(
                        st,
                        "the recording was sealed but is no longer waiting".into(),
                    )
                })?;
            st.remark.flow = Flow::Attached {
                record: checkout.id.as_str().to_string(),
                summary,
            };
            st.remark.stage = Stage::Compose;
            Ok(())
        }
        ServiceIntent::CaptureRecentForRemark => {
            recorder
                .capture_recent()
                .map_err(|error| remark_refused(st, error.to_string()))?;
            if st.remark.is_open() && matches!(st.remark.flow, Flow::None) {
                st.remark.flow = Flow::Capturing;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// What the replay transport is asked.
fn play(replay: &ReplayController, intent: ServiceIntent) -> Result<(), String> {
    let refused = |error: crate::shell::replay::TransportError| error.to_string();
    match intent {
        ServiceIntent::OpenReplay {
            record,
            user,
            presentation,
        } => replay
            .open_record(&record, &user, presentation)
            .map(|_| ())
            .map_err(refused),
        ServiceIntent::CloseReplay => {
            if replay.close() {
                Ok(())
            } else {
                Err("nothing is being replayed".into())
            }
        }
        ServiceIntent::PlayReplay => replay.play().map_err(refused),
        ServiceIntent::PauseReplay => replay.pause().map_err(refused),
        ServiceIntent::ReplayStepNext => replay.step_next().map(|_| ()).map_err(refused),
        ServiceIntent::ReplayStepPrevious => replay.step_previous().map(|_| ()).map_err(refused),
        ServiceIntent::ReplaySeekStep(step) => replay.seek_step(step).map_err(refused),
        ServiceIntent::ReplaySeekOffset(at) => replay.seek_offset(at).map_err(refused),
        ServiceIntent::ReplaySpeed(speed) => replay.set_speed(speed).map(|_| ()).map_err(refused),
        _ => Ok(()),
    }
}

/// Tell the open remark why its recording was refused, and pass the reason
/// on to the invocation that asked.
fn remark_refused(st: &mut ChromeState, detail: String) -> String {
    if st.remark.is_open() {
        st.remark.notice = Some(Notice {
            key: "remark-recording-refused",
            detail: Some(detail.clone()),
        });
    }
    detail
}

/// Whether a shell action may queue [`ServiceIntent`]s, so its invocation
/// finishes only once the host carried them out.
pub fn may_queue_intents(action_id: &str) -> bool {
    ["feedback.", "replay.", "remark."]
        .iter()
        .any(|prefix| action_id.starts_with(prefix))
}
