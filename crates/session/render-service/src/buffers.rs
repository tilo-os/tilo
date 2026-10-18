//! The buffers a surface's frames are delivered in.
//!
//! On a GPU they are linear dma-bufs from GBM on the compositor's render
//! node, imported as copy targets: the app attaches them with
//! `zwp_linux_dmabuf_v1`, the compositor imports them like any client's and
//! may scan them out directly. Without a GPU they are shared memory the app
//! attaches with `wl_shm`. Either way a surface has [`BUFFERS`] of them, so
//! one can be on screen, one waiting for the next refresh and one being
//! drawn.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use tilo_render_protocol::control::{BufferKind, DmabufFormat, Plane};
use tilo_render_protocol::shm::SharedMemory;

/// Buffers per surface.
pub const BUFFERS: usize = 3;

pub const DRM_FORMAT_ABGR8888: u32 = u32::from_le_bytes(*b"AB24");
pub const DRM_FORMAT_XBGR8888: u32 = u32::from_le_bytes(*b"XB24");
pub const DRM_FORMAT_MOD_LINEAR: u64 = 0;
/// `wl_shm` formats.
pub const WL_SHM_ARGB8888: u32 = 0;
pub const WL_SHM_XRGB8888: u32 = 1;

/// Where one buffer's pixels are.
pub enum Memory {
    #[cfg(target_os = "linux")]
    Dmabuf(Box<Dmabuf>),
    Shm(SharedMemory),
}

/// A dma-buf from GBM and its import.
#[cfg(target_os = "linux")]
pub struct Dmabuf {
    /// The import aliasing the allocation; dropped before it.
    pub target: ui::dmabuf_import::ImportedRenderTarget,
    pub fd: OwnedFd,
    _bo: gbm::BufferObject<()>,
}

/// One buffer and what it holds.
pub struct Buffer {
    pub memory: Memory,
    /// The app holds it (attached, or not yet released by the compositor).
    pub busy: bool,
    /// The surface frame index whose pixels it holds.
    pub holds: Option<u64>,
}

impl Buffer {
    pub fn fd(&self) -> BorrowedFd<'_> {
        match &self.memory {
            #[cfg(target_os = "linux")]
            Memory::Dmabuf(dmabuf) => dmabuf.fd.as_fd(),
            Memory::Shm(memory) => memory.fd(),
        }
    }
}

/// A surface's buffers of one size.
pub struct BufferSet {
    pub generation: u32,
    pub width: u32,
    pub height: u32,
    pub kind: BufferKind,
    pub planes: Vec<Plane>,
    pub buffers: Vec<Buffer>,
}

/// Why buffers could not be made.
#[derive(Debug, thiserror::Error)]
pub enum BufferError {
    #[error("the app's compositor takes none of the service's buffer formats")]
    NoFormat,
    #[error("buffer allocation failed: {0}")]
    Allocate(String),
}

/// The linear format a surface gets: alpha unless it is opaque, among those
/// the app said its compositor takes (all of them when it could not say).
pub fn dmabuf_format(opaque: bool, accepted: &[DmabufFormat]) -> Option<DmabufFormat> {
    let preferred = if opaque {
        [DRM_FORMAT_XBGR8888, DRM_FORMAT_ABGR8888]
    } else {
        [DRM_FORMAT_ABGR8888, DRM_FORMAT_ABGR8888]
    };
    preferred
        .into_iter()
        .map(|fourcc| DmabufFormat {
            fourcc,
            modifier: DRM_FORMAT_MOD_LINEAR,
        })
        .find(|format| accepted.is_empty() || accepted.contains(format))
}

impl BufferSet {
    /// [`BUFFERS`] dma-bufs of `width` × `height` in `format`, imported on
    /// `device`.
    ///
    /// # Errors
    ///
    /// When GBM or the import fails.
    #[cfg(target_os = "linux")]
    pub fn dmabufs(
        (device, allocator): (&vello::wgpu::Device, &gbm::Device<std::fs::File>),
        (width, height): (u32, u32),
        format: DmabufFormat,
        generation: u32,
    ) -> Result<Self, BufferError> {
        let gbm_format = if format.fourcc == DRM_FORMAT_XBGR8888 {
            gbm::Format::Xbgr8888
        } else {
            gbm::Format::Abgr8888
        };
        let mut buffers = Vec::with_capacity(BUFFERS);
        let mut planes = Vec::with_capacity(BUFFERS);
        for _ in 0..BUFFERS {
            let bo = allocator
                .create_buffer_object_with_modifiers2::<()>(
                    width,
                    height,
                    gbm_format,
                    [gbm::Modifier::Linear].into_iter(),
                    gbm::BufferObjectFlags::RENDERING,
                )
                .map_err(|error| BufferError::Allocate(error.to_string()))?;
            let fd = bo
                .fd_for_plane(0)
                .map_err(|error| BufferError::Allocate(error.to_string()))?;
            let plane = Plane {
                offset: bo.offset(0),
                stride: bo.stride_for_plane(0),
            };
            let target = ui::dmabuf_import::import_copy_target(
                device,
                &ui::dmabuf_import::DmabufRef {
                    planes: &[ui::dmabuf_import::PlaneRef {
                        fd: fd.as_fd(),
                        offset: plane.offset,
                        stride: plane.stride,
                    }],
                    fourcc: format.fourcc,
                    modifier: DRM_FORMAT_MOD_LINEAR,
                    width,
                    height,
                    colorimetry: ui::dmabuf_import::Colorimetry::default(),
                },
            )
            .map_err(|error| BufferError::Allocate(error.to_string()))?;
            planes.push(plane);
            buffers.push(Buffer {
                memory: Memory::Dmabuf(Box::new(Dmabuf {
                    target,
                    fd,
                    _bo: bo,
                })),
                busy: false,
                holds: None,
            });
        }
        Ok(Self {
            generation,
            width,
            height,
            kind: BufferKind::Dmabuf(format),
            planes,
            buffers,
        })
    }

    /// [`BUFFERS`] shared-memory buffers of `width` × `height`.
    ///
    /// # Errors
    ///
    /// When the memfds cannot be made.
    pub fn shm(
        (width, height): (u32, u32),
        opaque: bool,
        generation: u32,
    ) -> Result<Self, BufferError> {
        let stride = width
            .checked_mul(4)
            .ok_or_else(|| BufferError::Allocate("row overflows".to_owned()))?;
        let length = usize::try_from(u64::from(stride).saturating_mul(u64::from(height)))
            .map_err(|_| BufferError::Allocate("buffer overflows".to_owned()))?;
        let mut buffers = Vec::with_capacity(BUFFERS);
        for _ in 0..BUFFERS {
            let memory = SharedMemory::create("tilo-render-buffer", length)
                .map_err(|error| BufferError::Allocate(error.to_string()))?;
            buffers.push(Buffer {
                memory: Memory::Shm(memory),
                busy: false,
                holds: None,
            });
        }
        Ok(Self {
            generation,
            width,
            height,
            kind: BufferKind::Shm {
                format: if opaque {
                    WL_SHM_XRGB8888
                } else {
                    WL_SHM_ARGB8888
                },
            },
            planes: vec![Plane { offset: 0, stride }; BUFFERS],
            buffers,
        })
    }

    /// A buffer the app does not hold, preferring the one holding the newest
    /// frame (least to copy).
    pub fn free(&self) -> Option<usize> {
        self.buffers
            .iter()
            .enumerate()
            .filter(|(_, buffer)| !buffer.busy)
            .max_by_key(|(_, buffer)| buffer.holds.map_or(0, |held| held.saturating_add(1)))
            .map(|(index, _)| index)
    }
}
