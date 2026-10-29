//! A remote stream its source stops serving partway through, and picking it
//! up again where it stopped.

use super::MusicApp;
use ui::widget::toast::ToastKind;

/// How far short of its known length a stream may end and still count as
/// having played to the end: container durations and the catalogue's
/// rounded seconds disagree by a little, never by this much.
const STREAM_END_SLACK_MS: u64 = 5_000;

/// How far playback has to get past the last reconnect before a stream cut
/// short again is reconnected again. A source that keeps dying at one spot
/// then stops with its error instead of reconnecting forever.
const STREAM_RECOVERY_PROGRESS_MS: u64 = 5_000;

/// How a remote stream stopped before the user asked it to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StreamCut {
    /// The pipeline reported end of stream.
    Ended,
    /// The pipeline failed: the CDN refused a range, the connection dropped.
    Failed,
}

/// What to do about a remote stream that stopped at `position_ms`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StreamCutAction {
    /// The stream stopped on its own terms: the song is over, or it never
    /// started, which the start-failure path reports.
    Accept,
    /// Resolve a fresh URL and resume at this position.
    Resume(u64),
    /// It was cut short again at the spot of the last resume: report it.
    GiveUp,
}

/// What to do about a remote stream that stopped at `position_ms`.
///
/// A streaming CDN can stop serving a URL partway through: `YouTube` serves
/// the first segment of a URL whose client lacks a proof-of-origin token and
/// refuses the rest with 403, so a track stops about a minute in, and every
/// signed URL expires after some hours. The pipeline sees that as an error,
/// or as end of stream once the bytes it has run out. Neither is the end of
/// the song while `duration_ms` says more is left. A fresh URL resumes it.
/// Nothing played yet means the stream never started, which is a start
/// failure to report, and `last_resume_ms` bounds the retries to one per
/// spot.
pub(super) fn stream_cut_action(
    cut: StreamCut,
    position_ms: u64,
    duration_ms: u64,
    last_resume_ms: Option<u64>,
) -> StreamCutAction {
    if position_ms == 0 {
        return StreamCutAction::Accept;
    }
    if cut == StreamCut::Ended
        && (duration_ms == 0 || position_ms.saturating_add(STREAM_END_SLACK_MS) >= duration_ms)
    {
        return StreamCutAction::Accept;
    }
    if last_resume_ms
        .is_some_and(|last| position_ms < last.saturating_add(STREAM_RECOVERY_PROGRESS_MS))
    {
        return StreamCutAction::GiveUp;
    }
    StreamCutAction::Resume(position_ms)
}

impl MusicApp {
    /// Re-resolve the current remote stream and resume it where it stopped,
    /// when [`stream_cut_action`] says the source cut it short. Says so in a
    /// toast and the log, as it does when a stream is cut short again at the
    /// same spot, so a cut is never silent. Returns whether it resumed.
    pub(super) fn resume_cut_stream(&mut self, cut: StreamCut) -> bool {
        let Some(track_id) = self.pb.current.clone() else {
            return false;
        };
        let remote = self
            .audio
            .current_uri()
            .is_some_and(|uri| uri.starts_with("https://") || uri.starts_with("http://"));
        if !remote {
            return false;
        }
        let position = self
            .audio
            .position_ms()
            .unwrap_or(0)
            .max(self.playhead_ms.max(0.0) as u64);
        let duration = self
            .current_duration_ms()
            .max(self.audio.duration_ms().unwrap_or(0));
        let last = self
            .stream_recovery
            .as_ref()
            .filter(|(id, _)| *id == track_id)
            .map(|(_, at)| *at);
        let resume_at = match stream_cut_action(cut, position, duration, last) {
            StreamCutAction::Accept => return false,
            StreamCutAction::Resume(at) => at,
            StreamCutAction::GiveUp => {
                tracing::warn!(
                    target: "music::audio",
                    track_id,
                    position_ms = position,
                    duration_ms = duration,
                    ?cut,
                    "stream cut short again at the spot it was resumed from"
                );
                // A failure carries its own reason to the error path; an end
                // of stream has none, so it is said here.
                if cut == StreamCut::Ended {
                    let message = crate::i18n::t_args(
                        "music-stream-cut-short",
                        &[("position", ui::format::timecode(position / 1_000).into())],
                    );
                    self.playback_error = Some(message.clone());
                    self.notify(ToastKind::Error, message);
                }
                return false;
            }
        };
        let reason = self.audio.take_error();
        tracing::warn!(
            target: "music::audio",
            track_id,
            position_ms = resume_at,
            duration_ms = duration,
            ?cut,
            reason = reason.as_deref().unwrap_or("end of stream"),
            "stream cut short by its source; resolving a fresh URL to resume"
        );
        self.notify(
            ToastKind::Warning,
            crate::i18n::t_args(
                "music-stream-reconnecting",
                &[("position", ui::format::timecode(resume_at / 1_000).into())],
            ),
        );
        self.stream_recovery = Some((track_id.clone(), resume_at));
        self.playhead_ms = resume_at as f64;
        self.audio.stop();
        self.start_stream(&track_id);
        true
    }
}
