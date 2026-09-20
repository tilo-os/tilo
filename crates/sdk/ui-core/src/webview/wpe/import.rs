//! DMA-BUF → wgpu texture import for the WebView's rendered pages.
//!
//! The mechanics live in [`crate::dmabuf_import`], shared with the video
//! decoder's zero-copy path — both receive a GPU buffer as a dmabuf and want
//! it as a `wgpu::Texture` without a copy, and the Vulkan external-memory
//! dance is identical. This module is the adapter: it turns WebKit's
//! [`DmabufFrame`] into the shared descriptor and reports failures in the
//! terms this call site cares about.
//!
//! WebKit exports a single-plane RGBA-family buffer. Whether it is linear
//! depends on the driver: `AMD_DEBUG=nodcc,notiling` forces linear, which is
//! what makes the zero-copy path reachable, and without it the buffer comes
//! back tiled and the caller falls back to the SHM path.

use std::os::fd::AsRawFd;

use vello::wgpu;

use crate::dmabuf_import::{self, DmabufRef, ImportError, PlaneRef};

use super::egl::DmabufFrame;

/// Import a WebKit dmabuf frame as a sampleable texture.
///
/// The returned texture owns the imported `VkImage` + `VkDeviceMemory` (and
/// the duplicated fd) and frees them when dropped. `None` — with a warning
/// naming the reason — on a layout that cannot be imported or any Vulkan
/// failure, so the caller keeps using the SHM path.
pub fn import_dmabuf(device: &wgpu::Device, frame: &DmabufFrame) -> Option<wgpu::Texture> {
    let planes: Vec<PlaneRef<'_>> = frame
        .planes
        .iter()
        .map(|p| PlaneRef {
            // Safety: the fd is owned by `frame`, which outlives this call;
            // the importer duplicates it before handing it to Vulkan.
            fd: unsafe { std::os::fd::BorrowedFd::borrow_raw(p.fd.as_raw_fd()) },
            offset: p.offset,
            stride: p.stride,
        })
        .collect();

    let desc = DmabufRef {
        planes: &planes,
        fourcc: frame.fourcc,
        modifier: frame.modifier,
        width: frame.width,
        height: frame.height,
        // A rendered page is RGB already; no YUV conversion applies.
        colorimetry: media::dmabuf::Colorimetry::RGB,
    };

    match dmabuf_import::import(device, &desc) {
        Ok(mut imported) => {
            // A page is always a single packed RGBA-family plane; a multi-plane
            // result would mean the format table changed under this call site.
            debug_assert_eq!(imported.layout, dmabuf_import::PlaneLayout::Packed);
            imported.planes.pop()
        }
        Err(ImportError::UnsupportedModifier { modifier }) => {
            tracing::warn!(
                "webview dmabuf import: buffer is tiled (modifier {modifier:#x}); \
                 set AMD_DEBUG=nodcc,notiling for the zero-copy path. Using SHM."
            );
            None
        }
        Err(e) => {
            tracing::warn!("webview dmabuf import failed ({e}); using SHM");
            None
        }
    }
}
