//! Screen capture via the **XDG Desktop Portal** (`org.freedesktop.portal.ScreenCast`,
//! through the [`ashpd`] crate) — the native
//! [`crate::host::ScreenCaptureProvider`], with the `portal` feature on Unix.
//!
//! The portal lets the user pick a monitor or window, starts the cast and
//! hands back the first stream's `PipeWire` node, which the app then opens
//! (for example with `pipewiresrc`). The handshake runs on a worker thread with
//! a short-lived Tokio runtime, bounded so an unresponsive portal or a picker
//! nobody answers cannot wedge the request forever.

use crate::capture::{ScreenCaptureChannel, ScreenCaptureOutcome, ScreenCaptureRequest};

/// The native screen-capture provider.
pub struct DesktopScreenCapture;

impl crate::host::ScreenCaptureProvider for DesktopScreenCapture {
    fn request(&self, request: ScreenCaptureRequest) -> ScreenCaptureChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let fallback = tx.clone();
        let spawned = std::thread::Builder::new()
            .name("tilo-screen-capture".into())
            .spawn(move || {
                let _ = tx.send(capture_blocking(request));
            })
            .is_ok();
        if !spawned {
            let _ = fallback.send(ScreenCaptureOutcome::Unavailable(
                "failed to spawn the screen-capture worker thread".to_string(),
            ));
        }
        rx
    }
}

#[cfg(all(feature = "portal", unix))]
fn capture_blocking(request: ScreenCaptureRequest) -> ScreenCaptureOutcome {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            return ScreenCaptureOutcome::Unavailable(format!("screen-capture runtime: {error}"));
        }
    };
    runtime.block_on(async {
        match tokio::time::timeout(std::time::Duration::from_secs(120), capture(request)).await {
            Ok(outcome) => outcome,
            Err(_) => ScreenCaptureOutcome::Unavailable(
                "timed out waiting for a screen-capture source".to_string(),
            ),
        }
    })
}

#[cfg(all(feature = "portal", unix))]
async fn capture(request: ScreenCaptureRequest) -> ScreenCaptureOutcome {
    use crate::capture::{CaptureStream, ScreenCaptureGrant};
    use ashpd::WindowIdentifier;
    use ashpd::desktop::PersistMode;
    use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};

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
    let session = match proxy.create_session().await {
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
        .select_sources(&session, cursor, sources, false, None, PersistMode::DoNot)
        .await
    {
        return failed(error);
    }
    // No parent window handle from this surface: pass the default (empty)
    // identifier, letting the portal parent the chooser.
    let response = match proxy
        .start(&session, &WindowIdentifier::default())
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

/// Without the portal (the `portal` feature, on Unix) there is nothing to
/// capture through.
#[cfg(not(all(feature = "portal", unix)))]
fn capture_blocking(_request: ScreenCaptureRequest) -> ScreenCaptureOutcome {
    ScreenCaptureOutcome::Unavailable(
        "screen capture needs the real-desktop build (`--features portal`)".to_string(),
    )
}
