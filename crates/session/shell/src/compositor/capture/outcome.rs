//! Telling the person what a capture did: what the loop does with an
//! outcome that needs nothing but the frame (its chrome and notifications).
//! The outcomes that need the loop's own services (the device fabric, the
//! floating-surface service) are handled where those live.

use super::CaptureOutcome;
use crate::compositor::frame::FrameBuilder;

/// Tell the person what `outcome` came to, or note it in the log where the
/// shell's own chrome already shows it.
pub fn tell_outcome(frame_builder: &mut FrameBuilder, outcome: CaptureOutcome) {
    match outcome {
        // The loop answers these with its own services.
        CaptureOutcome::Saved(_)
        | CaptureOutcome::ScreenshotSendRequested { .. }
        | CaptureOutcome::FloatingClosed { .. } => {}
        CaptureOutcome::ScreenshotSent(sent) => {
            notify_screenshot_sent(frame_builder, sent);
        }
        CaptureOutcome::Copied { width, height } => {
            tracing::info!(width, height, "screenshot copied");
        }
        CaptureOutcome::Floated { id } => {
            tracing::debug!(%id, "live capture mirror created");
        }
        CaptureOutcome::Pinned { id } => {
            tracing::debug!(%id, "screenshot pinned as a frozen reference");
        }
        CaptureOutcome::RecordingStarted(path) => {
            // The compositor-owned pill supplies immediate,
            // spatial confirmation and then settles into the
            // top bar. A toast here would duplicate it and
            // unnecessarily cover application content.
            tracing::debug!(path = %path.display(), "recording started");
        }
        CaptureOutcome::RecordingPaused { path, paused } => {
            tracing::debug!(
                path = %path.display(),
                paused,
                "recording state changed"
            );
        }
        CaptureOutcome::RecordingAudioLost { path, reason } => {
            frame_builder.with_chrome(|chrome| {
                chrome.push_notification(
                    crate::shell::i18n::t("capture-recording-audio-lost"),
                    crate::shell::i18n::t_args(
                        "capture-recording-audio-lost-body",
                        &[
                            ("path", path.display().to_string().into()),
                            ("reason", reason.into()),
                        ],
                    ),
                );
            });
        }
        CaptureOutcome::RecordingStopped { path, frames } => {
            // The recording pill morphs in-place into a
            // clickable saved receipt. Avoid a second surface
            // competing for attention in the top-right corner.
            tracing::info!(path = %path.display(), frames, "recording saved");
        }
        CaptureOutcome::RecordingOpened(path) => {
            tracing::debug!(path = %path.display(), "recording opened");
        }
        CaptureOutcome::RecordingRevealed(path) => {
            tracing::debug!(path = %path.display(), "recording shown in Files");
        }
        CaptureOutcome::ReplayStarted => {
            tracing::info!("recent capture enabled");
        }
        CaptureOutcome::ReplaySaved { path, seconds } => {
            tracing::info!(path = %path.display(), seconds, "recent capture saved");
        }
        CaptureOutcome::ReplayStopped => {
            tracing::info!("recent capture disabled");
        }
        CaptureOutcome::ReplayAudioLost(reason) => {
            frame_builder.with_chrome(|chrome| {
                chrome.push_warning_notification(
                    crate::shell::i18n::t("capture-recent-audio-lost"),
                    reason,
                );
            });
        }
        CaptureOutcome::ScreenshotOpened(path) => {
            tracing::debug!(path = %path.display(), "screenshot opened");
        }
        CaptureOutcome::ScreenshotMarkupOpened(path) => {
            tracing::debug!(path = %path.display(), "screenshot markup opened");
        }
        CaptureOutcome::ScreenshotMarkupClosed => {
            tracing::debug!("screenshot markup closed");
        }
        CaptureOutcome::ScreenshotRevealed(path) => {
            tracing::debug!(path = %path.display(), "screenshot shown in Files");
        }
        CaptureOutcome::PortalScreenshotSaved(path) => {
            tracing::warn!(path = %path.display(), "portal screenshot completed after its request expired");
        }
        CaptureOutcome::PortalScreenshotCancelled => {
            tracing::debug!("portal screenshot selector cancelled after its request expired");
        }
        CaptureOutcome::Uncapturable(source) => {
            tracing::warn!(?source, "capture found nothing to read");
            frame_builder.with_chrome(|chrome| {
                chrome.push_error_notification(
                    crate::shell::i18n::t("capture-failed"),
                    crate::shell::i18n::t(match source {
                        ui::widget::SourceMode::Window => "capture-uncapturable-window",
                        ui::widget::SourceMode::Region => "capture-uncapturable-selection",
                        ui::widget::SourceMode::Strip => "capture-uncapturable-strip",
                        ui::widget::SourceMode::Display => "capture-uncapturable-screen",
                    }),
                );
            });
        }
        CaptureOutcome::Failed(error) => {
            // The reason is for the log; the person is told
            // the outcome and what to try.
            tracing::warn!(%error, "capture failed");
            frame_builder.with_chrome(|chrome| {
                chrome.push_error_notification(
                    crate::shell::i18n::t("capture-failed"),
                    crate::shell::i18n::t("capture-failed-body"),
                );
            });
        }
    }
}

/// Tell the person where "Send…" put their screenshot, or why it did not.
fn notify_screenshot_sent(
    frame_builder: &mut FrameBuilder,
    sent: Result<crate::shell::screenshot_send::SendReceipt, String>,
) {
    match sent {
        Ok(receipt) => frame_builder.with_chrome(|chrome| {
            chrome.push_notification(
                crate::shell::i18n::t("capture-screenshot-sent"),
                receipt.message(),
            );
        }),
        Err(error) => frame_builder.with_chrome(|chrome| {
            chrome.push_error_notification(
                crate::shell::i18n::t("capture-screenshot-not-sent"),
                error,
            );
        }),
    }
}
