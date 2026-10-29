//! `zwlr_screencopy_manager_v1` (version 3): a frame per copy, of a whole
//! output or a region of it, into shared memory or a dma-buf, with or
//! without the pointer, at once (`copy`) or once something changed
//! (`copy_with_damage`).

use std::sync::Mutex;
use std::time::Duration;

use smithay::output::Output;
use smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::{self, ZwlrScreencopyManagerV1},
};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource as _,
};
use smithay::utils::{Buffer, Logical, Physical, Rectangle};

use super::{
    Frame as Copy, Reply, Watcher, buffer_size, check_dmabuf, check_shm, region_on_output,
};
use crate::state::{ClientState, State};

/// The protocol version offered.
const VERSION: u32 = 3;

pub(super) fn create_global(display: &DisplayHandle) {
    display.create_global::<State, ZwlrScreencopyManagerV1, ()>(VERSION, ());
}

/// A screencopy frame waiting for its copy.
pub(crate) struct Frame(ZwlrScreencopyFrameV1);

impl Frame {
    pub(super) fn alive(&self) -> bool {
        self.0.is_alive()
    }

    pub(super) fn ready(&self, damage: &[Rectangle<i32, Buffer>], time: Duration) {
        let frame = &self.0;
        frame.flags(zwlr_screencopy_frame_v1::Flags::empty());
        if frame
            .data::<FrameData>()
            .is_some_and(FrameData::with_damage)
        {
            for rect in damage {
                frame.damage(
                    u32::try_from(rect.loc.x).unwrap_or(0),
                    u32::try_from(rect.loc.y).unwrap_or(0),
                    u32::try_from(rect.size.w).unwrap_or(0),
                    u32::try_from(rect.size.h).unwrap_or(0),
                );
            }
        }
        let seconds = time.as_secs();
        frame.ready(
            u32::try_from(seconds >> 32).unwrap_or(0),
            u32::try_from(seconds & u64::from(u32::MAX)).unwrap_or(0),
            time.subsec_nanos(),
        );
    }

    pub(super) fn failed(&self) {
        self.0.failed();
    }
}

/// What a frame copies, and whether it was used.
pub struct FrameData {
    /// The output and the region in its physical pixels; `None` when the
    /// output is not ours or the region misses it (the frame failed).
    target: Option<(String, Rectangle<i32, Physical>)>,
    cursor: bool,
    /// Set once a copy was asked for, with whether it waits for damage.
    used: Mutex<Option<bool>>,
}

impl FrameData {
    fn with_damage(&self) -> bool {
        self.used
            .lock()
            .ok()
            .and_then(|used| *used)
            .unwrap_or(false)
    }
}

impl GlobalDispatch<ZwlrScreencopyManagerV1, ()> for State {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrScreencopyManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }

    fn can_view(client: Client, _data: &()) -> bool {
        client
            .get_data::<ClientState>()
            .is_some_and(ClientState::trusted)
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let (frame, cursor, output, region) = match request {
            zwlr_screencopy_manager_v1::Request::CaptureOutput {
                frame,
                overlay_cursor,
                output,
            } => (frame, overlay_cursor != 0, output, None),
            zwlr_screencopy_manager_v1::Request::CaptureOutputRegion {
                frame,
                overlay_cursor,
                output,
                x,
                y,
                width,
                height,
            } => (
                frame,
                overlay_cursor != 0,
                output,
                Some(Rectangle::<i32, Logical>::new(
                    (x, y).into(),
                    (width, height).into(),
                )),
            ),
            _ => return,
        };
        let target = frame_target(state, &output, region);
        let frame = data_init.init(
            frame,
            FrameData {
                target: target.clone(),
                cursor,
                used: Mutex::new(None),
            },
        );
        let Some((name, region)) = target else {
            frame.failed();
            return;
        };
        let transform = state
            .shown_size(&name)
            .map_or(smithay::utils::Transform::Normal, |(output, _)| {
                output.current_transform()
            });
        let size = buffer_size(region.size, transform);
        let (Ok(width), Ok(height)) = (u32::try_from(size.w), u32::try_from(size.h)) else {
            frame.failed();
            return;
        };
        frame.buffer(
            smithay::reexports::wayland_server::protocol::wl_shm::Format::Xrgb8888,
            width,
            height,
            width.saturating_mul(4),
        );
        if frame.version() >= 3 {
            if let Some(targets) = &state.capture.dmabuf
                && let Some(format) = targets.formats.first()
            {
                frame.linux_dmabuf(format.code as u32, width, height);
            }
            frame.buffer_done();
        }
    }
}

/// The output `output` names and the part of it a frame copies.
fn frame_target(
    state: &State,
    output: &WlOutput,
    region: Option<Rectangle<i32, Logical>>,
) -> Option<(String, Rectangle<i32, Physical>)> {
    let name = Output::from_resource(output)?.name();
    let (output, shown) = state.shown_size(&name)?;
    let scale = output.current_scale().fractional_scale();
    let region = region_on_output(region, scale, shown)?;
    Some((name, region))
}

impl Dispatch<ZwlrScreencopyFrameV1, FrameData> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        frame: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        data: &FrameData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let (buffer, with_damage) = match request {
            zwlr_screencopy_frame_v1::Request::Copy { buffer } => (buffer, false),
            zwlr_screencopy_frame_v1::Request::CopyWithDamage { buffer } => (buffer, true),
            _ => return,
        };
        {
            let Ok(mut used) = data.used.lock() else {
                return;
            };
            if used.is_some() {
                frame.post_error(
                    zwlr_screencopy_frame_v1::Error::AlreadyUsed,
                    "the frame was already copied",
                );
                return;
            }
            *used = Some(with_damage);
        }
        let Some((name, region)) = data.target.clone() else {
            frame.failed();
            return;
        };
        let Some((output, _)) = state.shown_size(&name) else {
            frame.failed();
            return;
        };
        let size = buffer_size(region.size, output.current_transform());
        if let Err(why) = check_buffer(state, &buffer, size) {
            frame.post_error(zwlr_screencopy_frame_v1::Error::InvalidBuffer, why);
            return;
        }
        state.capture.queue(Copy {
            reply: Reply::Screencopy(Frame(frame.clone())),
            output: name.clone(),
            region,
            buffer,
            cursor: data.cursor,
            wait_for_damage: with_damage,
            watcher: Watcher::Client(client.id(), name),
        });
    }
}

/// Whether `buffer` can take a frame of `size`.
fn check_buffer(
    state: &State,
    buffer: &WlBuffer,
    size: smithay::utils::Size<i32, Physical>,
) -> Result<(), String> {
    if let Ok(dmabuf) = smithay::wayland::dmabuf::get_dmabuf(buffer) {
        use smithay::backend::allocator::Buffer as _;
        let dmabuf_size = dmabuf.size();
        return check_dmabuf(
            (dmabuf_size.w, dmabuf_size.h),
            dmabuf.format(),
            size,
            state.capture.dmabuf.as_ref(),
        );
    }
    smithay::wayland::shm::with_buffer_contents(buffer, |_, len, data| {
        check_shm(data.into(), len, size)
    })
    .map_err(|error| format!("{error:?}"))?
}
