//! Linux screen capture: the ScreenCast portal asks the user for a monitor or
//! window and grants a PipeWire node, which a `pipewiresrc` reads. The
//! portal session lives in a task on the app runtime for as long as the
//! capture does.

use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::{Arc, Mutex};

use ashpd::desktop::screencast::{
    CursorMode, OpenPipeWireRemoteOptions, Screencast, SelectSourcesOptions, SourceType,
    StartCastOptions,
};
use ashpd::desktop::{CreateSessionOptions, PersistMode};

use crate::{CaptureError, CaptureState, Frame, ScreenRequest, VideoSource};

enum Grant {
    Waiting,
    Granted { fd: OwnedFd, node: u32 },
    Failed(CaptureError),
}

pub(crate) struct Screen {
    grant: Arc<Mutex<Grant>>,
    /// The PipeWire remote stays open while the pipeline reads from it.
    remote: Option<OwnedFd>,
    video: Option<Box<dyn VideoSource>>,
    failed: Option<CaptureError>,
    stop: Option<Box<dyn FnOnce() + Send>>,
}

impl Screen {
    pub(crate) fn request(request: ScreenRequest) -> Self {
        let grant = Arc::new(Mutex::new(Grant::Waiting));
        let answer = Arc::clone(&grant);
        let task = tilo_rt::app().spawn(async move {
            let set = |g: Grant| {
                if let Ok(mut slot) = answer.lock() {
                    *slot = g;
                }
            };
            let proxy = match Screencast::new().await {
                Ok(proxy) => proxy,
                Err(error) => {
                    set(Grant::Failed(CaptureError::Unsupported(
                        "the screen-capture portal is not available",
                    )));
                    drop(error);
                    return;
                }
            };
            let failed = |error: ashpd::Error| match error {
                ashpd::Error::Response(_) => Grant::Failed(CaptureError::Cancelled),
                other => Grant::Failed(CaptureError::Failed(other.to_string())),
            };
            let session = match proxy.create_session(CreateSessionOptions::default()).await {
                Ok(session) => session,
                Err(error) => return set(failed(error)),
            };
            let mut sources = ashpd::enumflags2::BitFlags::<SourceType>::empty();
            if request.monitors {
                sources |= SourceType::Monitor;
            }
            if request.windows {
                sources |= SourceType::Window;
            }
            let cursor = if request.cursor {
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
                return set(failed(error));
            }
            let response = match proxy
                .start(&session, None, StartCastOptions::default())
                .await
                .and_then(|request| request.response())
            {
                Ok(response) => response,
                Err(error) => return set(failed(error)),
            };
            let Some(node) = response.streams().first().map(|s| s.pipe_wire_node_id()) else {
                return set(Grant::Failed(CaptureError::Cancelled));
            };
            match proxy
                .open_pipe_wire_remote(&session, OpenPipeWireRemoteOptions::default())
                .await
            {
                Ok(fd) => set(Grant::Granted { fd, node }),
                Err(error) => return set(failed(error)),
            }
            // Holding the session keeps the cast running; dropping the
            // capture aborts this task, which ends it.
            std::future::pending::<()>().await;
            drop(session);
        });
        Self {
            grant,
            remote: None,
            video: None,
            failed: None,
            stop: Some(Box::new(move || task.abort())),
        }
    }

    /// Build the pipeline on the caller's thread once the portal granted a
    /// node.
    fn advance(&mut self) {
        if self.video.is_some() || self.failed.is_some() {
            return;
        }
        let Ok(mut grant) = self.grant.lock() else {
            return;
        };
        match std::mem::replace(&mut *grant, Grant::Waiting) {
            Grant::Waiting => {}
            Grant::Failed(error) => self.failed = Some(error),
            Grant::Granted { fd, node } => {
                match crate::gst_backend::pipewire_node(fd.as_raw_fd(), node) {
                    Ok(video) => {
                        self.video = Some(video);
                        self.remote = Some(fd);
                    }
                    Err(error) => self.failed = Some(error),
                }
            }
        }
    }
}

impl VideoSource for Screen {
    fn state(&mut self) -> CaptureState {
        self.advance();
        if let Some(error) = &self.failed {
            return CaptureState::Failed(error.clone());
        }
        self.video
            .as_mut()
            .map_or(CaptureState::Starting, |video| video.state())
    }

    fn take_latest(&mut self) -> Option<Frame> {
        self.advance();
        self.video.as_mut()?.take_latest()
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        self.video = None;
        self.remote = None;
        if let Some(stop) = self.stop.take() {
            stop();
        }
    }
}
