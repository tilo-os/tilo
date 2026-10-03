//! Screen capture via the **XDG Desktop Portal** (`org.freedesktop.portal.ScreenCast`,
//! through the [`ashpd`] crate) — the native
//! [`crate::host::ScreenCaptureProvider`], with the `portal` feature on Unix.
//!
//! The portal lets the user pick a monitor or window, starts the cast and
//! hands back the first stream's `PipeWire` node, which the app then opens
//! (for example with `pipewiresrc`). The handshake runs on the app runtime
//! ([`tilo_rt::app`]), bounded so an unresponsive portal or a picker nobody
//! answers cannot wedge the request forever; the outcome wakes the event loop.

use crate::capture::{ScreenCaptureChannel, ScreenCaptureOutcome, ScreenCaptureRequest};

/// The native screen-capture provider.
pub struct DesktopScreenCapture;

impl crate::host::ScreenCaptureProvider for DesktopScreenCapture {
    fn request(&self, request: ScreenCaptureRequest) -> ScreenCaptureChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        #[cfg(all(feature = "portal", target_os = "linux"))]
        {
            let wake = crate::wake::WakeHandle::new();
            drop(
                tilo_rt::app().spawn_then(bounded_capture(request), move |outcome| {
                    // The UI may have moved on (window closed); ignore a dead channel.
                    if tx.send(outcome).is_ok() {
                        wake.wake();
                    }
                }),
            );
        }
        // Without the portal (the `portal` feature, on Unix) there is nothing
        // to capture through; answer at once.
        #[cfg(not(all(feature = "portal", target_os = "linux")))]
        {
            let _ = request;
            let _ = tx.send(ScreenCaptureOutcome::Unavailable(
                "screen capture needs the real-desktop build (`--features portal`)".to_string(),
            ));
        }
        rx
    }
}

#[cfg(all(feature = "portal", target_os = "linux"))]
async fn bounded_capture(request: ScreenCaptureRequest) -> ScreenCaptureOutcome {
    match tokio::time::timeout(std::time::Duration::from_secs(120), capture(request)).await {
        Ok(outcome) => outcome,
        Err(_) => ScreenCaptureOutcome::Unavailable(
            "timed out waiting for a screen-capture source".to_string(),
        ),
    }
}

#[cfg(all(feature = "portal", target_os = "linux"))]
async fn capture(request: ScreenCaptureRequest) -> ScreenCaptureOutcome {
    use crate::capture::{CaptureStream, ScreenCaptureGrant};
    use ashpd::desktop::screencast::{
        CursorMode, Screencast, SelectSourcesOptions, SourceType, StartCastOptions,
    };
    use ashpd::desktop::{CreateSessionOptions, PersistMode};

    let failed = |error: ashpd::Error| match error {
        ashpd::Error::Response(_) => ScreenCaptureOutcome::Cancelled,
        other => ScreenCaptureOutcome::Unavailable(other.to_string()),
    };
    let proxy = match Screencast::new().await {
        Ok(proxy) => proxy,
        Err(error) => {
            return ScreenCaptureOutcome::Unavailable(format!(
                "screen-capture portal unavailable: {error}"
            ));
        }
    };
    let session = match proxy.create_session(CreateSessionOptions::default()).await {
        Ok(session) => session,
        Err(error) => return failed(error),
    };
    let mut sources = ashpd::enumflags2::BitFlags::<SourceType>::empty();
    if request.monitors {
        sources |= SourceType::Monitor;
    }
    if request.windows {
        sources |= SourceType::Window;
    }
    let cursor = if request.embed_cursor {
        CursorMode::Embedded
    } else {
        CursorMode::Hidden
    };
    if let Err(error) = proxy
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_cursor_mode(cursor)
                .set_sources(sources)
                .set_multiple(false)
                .set_persist_mode(PersistMode::DoNot),
        )
        .await
    {
        return failed(error);
    }
    // No parent window handle from this surface: pass no identifier, letting
    // the portal parent the chooser.
    let response = match proxy
        .start(&session, None, StartCastOptions::default())
        .await
        .and_then(|request| request.response())
    {
        Ok(response) => response,
        Err(error) => return failed(error),
    };
    match response.streams().first() {
        Some(stream) => ScreenCaptureOutcome::Granted(ScreenCaptureGrant {
            stream: CaptureStream::PipeWire {
                node_id: stream.pipe_wire_node_id(),
            },
            size: stream.size().and_then(|(width, height)| {
                Some((u32::try_from(width).ok()?, u32::try_from(height).ok()?))
            }),
        }),
        None => ScreenCaptureOutcome::Cancelled,
    }
}
