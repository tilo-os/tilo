//! Buffers another process drew, attached to a window's own surface (Tilo).
//!
//! A Tilo app's frames are drawn by its room's render service into dma-bufs
//! (or shared memory without a GPU); the app attaches them to its window's
//! `wl_surface` like any client buffer. This binds `zwp_linux_dmabuf_v1`
//! (version 3, for its format and modifier events) and keeps the formats the
//! compositor takes; a buffer is a `wl_buffer` whose release event calls the
//! owner back.
use std::os::fd::BorrowedFd;
use std::sync::{Arc, Mutex};

use sctk::reexports::client::globals::{BindError, GlobalList};
use sctk::reexports::client::protocol::wl_buffer::{self, WlBuffer};
use sctk::reexports::client::protocol::wl_shm::{self, WlShm};
use sctk::reexports::client::protocol::wl_shm_pool::{self, WlShmPool};
use sctk::reexports::client::{delegate_dispatch, Connection, Dispatch, QueueHandle};
use sctk::reexports::protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1, zwp_linux_dmabuf_v1,
};

use crate::platform_impl::wayland::state::WinitState;

/// The globals client buffers are made with.
pub struct ClientBuffers {
    dmabuf: Option<zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1>,
    shm: WlShm,
    formats: Arc<Mutex<Vec<(u32, u64)>>>,
    queue: QueueHandle<WinitState>,
}

/// Calls its owner back when the compositor releases the buffer.
pub struct ReleaseHandler(Box<dyn Fn() + Send + Sync>);

/// One buffer: destroyed when dropped.
pub struct ClientBuffer {
    buffer: WlBuffer,
}

impl ClientBuffer {
    pub fn wl_buffer(&self) -> &WlBuffer {
        &self.buffer
    }
}

impl Drop for ClientBuffer {
    fn drop(&mut self) {
        self.buffer.destroy();
    }
}

/// Where a buffer's single plane is.
pub enum ClientBufferSource<'a> {
    Dmabuf {
        fd: BorrowedFd<'a>,
        width: i32,
        height: i32,
        fourcc: u32,
        modifier: u64,
        offset: u32,
        stride: u32,
    },
    Shm {
        fd: BorrowedFd<'a>,
        width: i32,
        height: i32,
        /// A `wl_shm` format code.
        format: u32,
        offset: i32,
        stride: i32,
    },
}

impl ClientBuffers {
    pub fn bind(
        globals: &GlobalList,
        shm: &WlShm,
        queue: &QueueHandle<WinitState>,
    ) -> Result<Self, BindError> {
        let formats = Arc::new(Mutex::new(Vec::new()));
        let dmabuf = globals.bind(queue, 3..=3, formats.clone()).ok();
        Ok(Self {
            dmabuf,
            shm: shm.clone(),
            formats,
            queue: queue.clone(),
        })
    }

    /// The (fourcc, modifier) pairs the compositor takes as dma-bufs.
    pub fn formats(&self) -> Vec<(u32, u64)> {
        self.formats.lock().unwrap().clone()
    }

    /// A buffer of `source`, calling `on_release` whenever the compositor
    /// releases it. `None` when the compositor does not take that kind or
    /// format of buffer, so asking can never be a protocol error.
    pub fn create(
        &self,
        source: ClientBufferSource<'_>,
        on_release: Box<dyn Fn() + Send + Sync>,
    ) -> Option<ClientBuffer> {
        let handler = ReleaseHandler(on_release);
        match source {
            ClientBufferSource::Dmabuf {
                fd,
                width,
                height,
                fourcc,
                modifier,
                offset,
                stride,
            } => {
                let dmabuf = self.dmabuf.as_ref()?;
                if !self.formats().contains(&(fourcc, modifier)) || width <= 0 || height <= 0 {
                    return None;
                }
                let params = dmabuf.create_params(&self.queue, ());
                params.add(
                    fd,
                    0,
                    offset,
                    stride,
                    (modifier >> 32) as u32,
                    (modifier & 0xffff_ffff) as u32,
                );
                let buffer = params.create_immed(
                    width,
                    height,
                    fourcc,
                    zwp_linux_buffer_params_v1::Flags::empty(),
                    &self.queue,
                    handler,
                );
                params.destroy();
                Some(ClientBuffer { buffer })
            }
            ClientBufferSource::Shm {
                fd,
                width,
                height,
                format,
                offset,
                stride,
            } => {
                let format = wl_shm::Format::try_from(format).ok()?;
                if !matches!(format, wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888)
                    || width <= 0
                    || height <= 0
                {
                    return None;
                }
                let size = stride.checked_mul(height)?.checked_add(offset)?;
                let pool = self.shm.create_pool(fd, size, &self.queue, ());
                let buffer =
                    pool.create_buffer(offset, width, height, stride, format, &self.queue, handler);
                pool.destroy();
                Some(ClientBuffer { buffer })
            }
        }
    }
}

impl Dispatch<zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1, Arc<Mutex<Vec<(u32, u64)>>>, WinitState>
    for ClientBuffers
{
    fn event(
        _: &mut WinitState,
        _: &zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
        event: zwp_linux_dmabuf_v1::Event,
        formats: &Arc<Mutex<Vec<(u32, u64)>>>,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
        if let zwp_linux_dmabuf_v1::Event::Modifier {
            format,
            modifier_hi,
            modifier_lo,
        } = event
        {
            let modifier = (u64::from(modifier_hi) << 32) | u64::from(modifier_lo);
            let mut formats = formats.lock().unwrap();
            if !formats.contains(&(format, modifier)) {
                formats.push((format, modifier));
            }
        }
    }
}

impl Dispatch<zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1, (), WinitState>
    for ClientBuffers
{
    fn event(
        _: &mut WinitState,
        _: &zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
        _: zwp_linux_buffer_params_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
        // `create_immed` only: a failure is a protocol error, never an event.
    }

    sctk::reexports::client::event_created_child!(WinitState, zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1, [
        zwp_linux_buffer_params_v1::EVT_CREATED_OPCODE => (WlBuffer, ReleaseHandler(Box::new(|| {})))
    ]);
}

impl Dispatch<WlShmPool, (), WinitState> for ClientBuffers {
    fn event(
        _: &mut WinitState,
        _: &WlShmPool,
        _: wl_shm_pool::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
    }
}

impl Dispatch<WlBuffer, ReleaseHandler, WinitState> for ClientBuffers {
    fn event(
        _: &mut WinitState,
        _: &WlBuffer,
        event: wl_buffer::Event,
        handler: &ReleaseHandler,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
        if let wl_buffer::Event::Release = event {
            (handler.0)();
        }
    }
}

delegate_dispatch!(WinitState: [zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1: Arc<Mutex<Vec<(u32, u64)>>>] => ClientBuffers);
delegate_dispatch!(WinitState: [zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1: ()] => ClientBuffers);
delegate_dispatch!(WinitState: [WlShmPool: ()] => ClientBuffers);
delegate_dispatch!(WinitState: [WlBuffer: ReleaseHandler] => ClientBuffers);
