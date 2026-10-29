//! `ext_image_copy_capture_v1` with `ext_output_image_capture_source_v1`:
//! a session per captured output that announces the buffers it takes, then
//! one frame at a time, the first at once and each later one once the
//! output changed.
//!
//! Cursor sessions (the pointer as its own image) are not offered: their
//! capture sessions stop at once, and clients draw the pointer into frames
//! instead (`paint_cursors`).

use std::sync::Mutex;
use std::time::Duration;

use smithay::output::Output;
use smithay::reexports::wayland_protocols::ext::image_capture_source::v1::server::{
    ext_image_capture_source_v1::{self, ExtImageCaptureSourceV1},
    ext_output_image_capture_source_manager_v1::{self, ExtOutputImageCaptureSourceManagerV1},
};
use smithay::reexports::wayland_protocols::ext::image_copy_capture::v1::server::{
    ext_image_copy_capture_cursor_session_v1::{self, ExtImageCopyCaptureCursorSessionV1},
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1, FailureReason},
    ext_image_copy_capture_manager_v1::{self, ExtImageCopyCaptureManagerV1, Options},
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};
use smithay::reexports::wayland_server::backend::{ClientId, ObjectId};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource as _, WEnum,
};
use smithay::utils::{Buffer, Physical, Rectangle, Size, Transform};

use super::{
    DmabufTargets, Failure, Frame as Copy, Reply, SHM_FORMATS, Watcher, buffer_size,
    region_on_output,
};
use crate::state::{ClientState, State};

pub(super) fn create_globals(display: &DisplayHandle) {
    display.create_global::<State, ExtImageCopyCaptureManagerV1, ()>(1, ());
    display.create_global::<State, ExtOutputImageCaptureSourceManagerV1, ()>(1, ());
}

fn trusted(client: &Client) -> bool {
    client
        .get_data::<ClientState>()
        .is_some_and(ClientState::trusted)
}

/// What a capture source shows: an output, by name; `None` when the client
/// named an output that is not ours.
pub struct SourceData {
    output: Option<String>,
}

/// A session's source and what it last told its client.
pub struct SessionData {
    output: Option<String>,
    cursor: bool,
    state: Mutex<SessionState>,
}

#[derive(Default)]
struct SessionState {
    /// A frame object of this session exists.
    frame: bool,
    /// The buffer size last announced.
    size: Option<Size<i32, Physical>>,
    stopped: bool,
}

/// A frame's session, buffer and whether it was captured.
pub struct FrameData {
    session: ExtImageCopyCaptureSessionV1,
    state: Mutex<FrameState>,
}

#[derive(Default)]
struct FrameState {
    buffer: Option<WlBuffer>,
    captured: bool,
}

/// A live session, for constraint updates and stops.
pub(crate) struct Session(ExtImageCopyCaptureSessionV1);

impl Session {
    pub(super) fn alive(&self) -> bool {
        self.0.is_alive()
    }

    pub(super) fn id(&self) -> ObjectId {
        self.0.id()
    }
}

/// An image-copy frame waiting for its copy.
pub(crate) struct Frame(ExtImageCopyCaptureFrameV1);

impl Frame {
    pub(super) fn alive(&self) -> bool {
        self.0.is_alive()
    }

    pub(super) fn ready(
        &self,
        damage: &[Rectangle<i32, Buffer>],
        transform: Transform,
        time: Duration,
    ) {
        let frame = &self.0;
        frame.transform(transform.into());
        for rect in damage {
            frame.damage(rect.loc.x, rect.loc.y, rect.size.w, rect.size.h);
        }
        let seconds = time.as_secs();
        frame.presentation_time(
            u32::try_from(seconds >> 32).unwrap_or(0),
            u32::try_from(seconds & u64::from(u32::MAX)).unwrap_or(0),
            time.subsec_nanos(),
        );
        frame.ready();
    }

    pub(super) fn failed(&self, why: Failure) {
        self.0.failed(match why {
            Failure::Constraints => FailureReason::BufferConstraints,
            Failure::Stopped => FailureReason::Stopped,
            Failure::Unknown => FailureReason::Unknown,
        });
    }
}

/// Tell a session's client the buffers it takes for frames of `size`.
fn announce(
    session: &ExtImageCopyCaptureSessionV1,
    size: Size<i32, Physical>,
    dmabuf: Option<&DmabufTargets>,
) {
    session.buffer_size(
        u32::try_from(size.w).unwrap_or(0),
        u32::try_from(size.h).unwrap_or(0),
    );
    for format in SHM_FORMATS {
        session.shm_format(format);
    }
    if let Some(targets) = dmabuf {
        session.dmabuf_device(targets.device.to_ne_bytes().to_vec());
        let mut codes: Vec<_> = targets.formats.iter().map(|format| format.code).collect();
        codes.dedup();
        for code in codes {
            let modifiers: Vec<u8> = targets
                .formats
                .iter()
                .filter(|format| format.code == code)
                .flat_map(|format| u64::from(format.modifier).to_ne_bytes())
                .collect();
            session.dmabuf_format(code as u32, modifiers);
        }
    }
    session.done();
}

/// The buffer size frames of `output` take, when it is shown.
fn frame_size(
    state: &State,
    output: &str,
) -> Option<(Rectangle<i32, Physical>, Size<i32, Physical>)> {
    let (output, shown) = state.shown_size(output)?;
    let region = region_on_output(None, 1.0, shown)?;
    Some((region, buffer_size(region.size, output.current_transform())))
}

/// Stop sessions whose output is gone and tell the others when the size of
/// their frames changed.
pub(super) fn refresh_sessions(state: &mut State) {
    state.capture.sessions.retain(Session::alive);
    for session in &state.capture.sessions {
        let Some(data) = session.0.data::<SessionData>() else {
            continue;
        };
        let Ok(mut session_state) = data.state.lock() else {
            continue;
        };
        if session_state.stopped {
            continue;
        }
        let size = data
            .output
            .as_deref()
            .and_then(|output| frame_size(state, output))
            .map(|(_, size)| size);
        match size {
            None => {
                session_state.stopped = true;
                session.0.stopped();
            }
            Some(size) if session_state.size != Some(size) => {
                session_state.size = Some(size);
                announce(&session.0, size, state.capture.dmabuf.as_ref());
            }
            Some(_) => {}
        }
    }
}

impl GlobalDispatch<ExtOutputImageCaptureSourceManagerV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ExtOutputImageCaptureSourceManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, _data: &()) -> bool {
        trusted(&client)
    }
}

impl Dispatch<ExtOutputImageCaptureSourceManagerV1, ()> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _manager: &ExtOutputImageCaptureSourceManagerV1,
        request: ext_output_image_capture_source_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let ext_output_image_capture_source_manager_v1::Request::CreateSource {
            source,
            output,
        } = request
        {
            let output = Output::from_resource(&output).map(|output| output.name());
            data_init.init(source, SourceData { output });
        }
    }
}

impl Dispatch<ExtImageCaptureSourceV1, SourceData> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _source: &ExtImageCaptureSourceV1,
        _request: ext_image_capture_source_v1::Request,
        _data: &SourceData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}

impl GlobalDispatch<ExtImageCopyCaptureManagerV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ExtImageCopyCaptureManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, _data: &()) -> bool {
        trusted(&client)
    }
}

impl Dispatch<ExtImageCopyCaptureManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        manager: &ExtImageCopyCaptureManagerV1,
        request: ext_image_copy_capture_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ext_image_copy_capture_manager_v1::Request::CreateSession {
                session,
                source,
                options,
            } => {
                let cursor = match options {
                    WEnum::Value(options) => options.contains(Options::PaintCursors),
                    WEnum::Unknown(_) => {
                        manager.post_error(
                            ext_image_copy_capture_manager_v1::Error::InvalidOption,
                            "unknown capture options",
                        );
                        return;
                    }
                };
                let output = source
                    .data::<SourceData>()
                    .and_then(|data| data.output.clone());
                let session = data_init.init(
                    session,
                    SessionData {
                        output,
                        cursor,
                        state: Mutex::new(SessionState::default()),
                    },
                );
                state.capture.sessions.push(Session(session));
                refresh_sessions(state);
            }
            ext_image_copy_capture_manager_v1::Request::CreatePointerCursorSession {
                session,
                ..
            } => {
                data_init.init(session, Mutex::new(false));
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureCursorSessionV1, Mutex<bool>> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        cursor_session: &ExtImageCopyCaptureCursorSessionV1,
        request: ext_image_copy_capture_cursor_session_v1::Request,
        taken: &Mutex<bool>,
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let ext_image_copy_capture_cursor_session_v1::Request::GetCaptureSession { session } =
            request
        {
            let Ok(mut taken) = taken.lock() else {
                return;
            };
            if *taken {
                cursor_session.post_error(
                    ext_image_copy_capture_cursor_session_v1::Error::DuplicateSession,
                    "the capture session was already taken",
                );
                return;
            }
            *taken = true;
            let session = data_init.init(
                session,
                SessionData {
                    output: None,
                    cursor: false,
                    state: Mutex::new(SessionState {
                        stopped: true,
                        ..SessionState::default()
                    }),
                },
            );
            session.stopped();
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, SessionData> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        session: &ExtImageCopyCaptureSessionV1,
        request: ext_image_copy_capture_session_v1::Request,
        data: &SessionData,
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let ext_image_copy_capture_session_v1::Request::CreateFrame { frame } = request {
            let Ok(mut session_state) = data.state.lock() else {
                return;
            };
            if session_state.frame {
                session.post_error(
                    ext_image_copy_capture_session_v1::Error::DuplicateFrame,
                    "the session's previous frame still exists",
                );
                return;
            }
            session_state.frame = true;
            data_init.init(
                frame,
                FrameData {
                    session: session.clone(),
                    state: Mutex::new(FrameState::default()),
                },
            );
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, FrameData> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        frame: &ExtImageCopyCaptureFrameV1,
        request: ext_image_copy_capture_frame_v1::Request,
        data: &FrameData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let Ok(mut frame_state) = data.state.lock() else {
            return;
        };
        let already = |frame: &ExtImageCopyCaptureFrameV1| {
            frame.post_error(
                ext_image_copy_capture_frame_v1::Error::AlreadyCaptured,
                "the frame was already captured",
            );
        };
        match request {
            ext_image_copy_capture_frame_v1::Request::AttachBuffer { buffer } => {
                if frame_state.captured {
                    already(frame);
                    return;
                }
                frame_state.buffer = Some(buffer);
            }
            ext_image_copy_capture_frame_v1::Request::DamageBuffer {
                x,
                y,
                width,
                height,
            } => {
                if frame_state.captured {
                    already(frame);
                } else if x < 0 || y < 0 || width <= 0 || height <= 0 {
                    frame.post_error(
                        ext_image_copy_capture_frame_v1::Error::InvalidBufferDamage,
                        "damage outside the buffer",
                    );
                }
                // Every copy redraws the whole buffer, so the client's
                // damage needs no tracking.
            }
            ext_image_copy_capture_frame_v1::Request::Capture => {
                if frame_state.captured {
                    already(frame);
                    return;
                }
                let Some(buffer) = frame_state.buffer.clone() else {
                    frame.post_error(
                        ext_image_copy_capture_frame_v1::Error::NoBuffer,
                        "capture without a buffer",
                    );
                    return;
                };
                frame_state.captured = true;
                drop(frame_state);
                capture(state, frame, data, buffer);
            }
            _ => {}
        }
    }

    fn destroyed(
        _state: &mut Self,
        _client: ClientId,
        _frame: &ExtImageCopyCaptureFrameV1,
        data: &FrameData,
    ) {
        if let Some(session) = data.session.data::<SessionData>()
            && let Ok(mut session_state) = session.state.lock()
        {
            session_state.frame = false;
        }
    }
}

/// Queue a captured frame, or fail it when its session stopped.
fn capture(
    state: &mut State,
    frame: &ExtImageCopyCaptureFrameV1,
    data: &FrameData,
    buffer: WlBuffer,
) {
    let reply = Frame(frame.clone());
    let Some(session) = data.session.data::<SessionData>() else {
        reply.failed(Failure::Stopped);
        return;
    };
    let stopped = session.state.lock().map_or(true, |state| state.stopped);
    let target = session
        .output
        .as_deref()
        .and_then(|output| Some((output, frame_size(state, output)?)));
    let (false, Some((output, (region, _)))) = (stopped, target) else {
        reply.failed(Failure::Stopped);
        return;
    };
    state.capture.queue(Copy {
        reply: Reply::ImageCopy(reply),
        output: output.to_owned(),
        region,
        buffer,
        cursor: session.cursor,
        wait_for_damage: true,
        watcher: Watcher::Session(data.session.id()),
    });
}
