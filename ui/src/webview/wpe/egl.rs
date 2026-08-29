//! EGL / GBM setup for the DMA-BUF zero-copy export path.
//!
//! WPE's default SHM export copies each frame GPU→CPU (a readback) and we
//! then re-upload it CPU→GPU — wasteful on real GPU hardware. The EGL path
//! instead has WebKit render into a GPU buffer and hand us an
//! `EGLImageKHR`, which we export to a **dmabuf fd** (`eglExportDMABUFImage
//! MESA`). That fd is imported directly into a wgpu texture on the render
//! thread — no copy, no swizzle, no readback.
//!
//! This module owns the engine-thread EGL context (a surfaceless context
//! on a chosen DRM render node via gbm) and the per-frame image→dmabuf
//! export. It is only constructed when `QOS_WEBVIEW_DMABUF=1`.

use std::os::fd::AsRawFd;

use super::ffi;

pub use crate::webview::{DmabufFrame, DmabufPlane};

/// Engine-thread EGL state for the export path. Holds the render-node file
/// (keeps the fd alive), the gbm device, and the EGL display/context, plus
/// the two MESA export entry points (loaded via `eglGetProcAddress`).
pub struct EglExport {
    _node: std::fs::File,
    pub display: ffi::EGLDisplay,
    query_fn: ffi::PFNEglExportDmabufImageQueryMesa,
    export_fn: ffi::PFNEglExportDmabufImageMesa,
}

// The context is created on and only ever touched from the engine thread.
unsafe impl Send for EglExport {}

impl EglExport {
    /// Open a render node, bring up a surfaceless GLES context on it, and
    /// load the dmabuf-export extension. Returns `None` (with a warning)
    /// on any failure so the caller can fall back to the SHM path.
    pub fn setup() -> Option<Self> {
        // Default to the first render node; a hybrid machine may need the
        // node with a working mesa driver selected via QOS_WEBVIEW_DRM_NODE,
        // since a node without one can't back an EGL display.
        let node_path = std::env::var("QOS_WEBVIEW_DRM_NODE")
            .unwrap_or_else(|_| "/dev/dri/renderD128".to_string());
        let node = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&node_path)
            .map_err(|e| tracing::warn!("dmabuf: open {node_path} failed: {e}"))
            .ok()?;

        unsafe {
            let gbm = ffi::gbm_create_device(node.as_raw_fd());
            if gbm.is_null() {
                tracing::warn!("dmabuf: gbm_create_device failed on {node_path}");
                return None;
            }
            let display = ffi::eglGetPlatformDisplay(
                ffi::EGL_PLATFORM_GBM_KHR,
                gbm as *mut std::ffi::c_void,
                std::ptr::null(),
            );
            if display.is_null() {
                tracing::warn!("dmabuf: eglGetPlatformDisplay failed");
                return None;
            }
            let (mut major, mut minor) = (0, 0);
            if ffi::eglInitialize(display, &mut major, &mut minor) != ffi::EGL_TRUE {
                tracing::warn!("dmabuf: eglInitialize failed (0x{:x})", ffi::eglGetError());
                return None;
            }
            if ffi::eglBindAPI(ffi::EGL_OPENGL_ES_API) != ffi::EGL_TRUE {
                tracing::warn!("dmabuf: eglBindAPI failed");
                return None;
            }

            let cfg_attribs: [ffi::EGLint; 13] = [
                ffi::EGL_SURFACE_TYPE,
                ffi::EGL_WINDOW_BIT,
                ffi::EGL_RENDERABLE_TYPE,
                ffi::EGL_OPENGL_ES2_BIT,
                ffi::EGL_RED_SIZE,
                8,
                ffi::EGL_GREEN_SIZE,
                8,
                ffi::EGL_BLUE_SIZE,
                8,
                ffi::EGL_ALPHA_SIZE,
                8,
                ffi::EGL_NONE,
            ];
            let mut config: ffi::EGLConfig = std::ptr::null_mut();
            let mut num_config: ffi::EGLint = 0;
            if ffi::eglChooseConfig(
                display,
                cfg_attribs.as_ptr(),
                &mut config,
                1,
                &mut num_config,
            ) != ffi::EGL_TRUE
                || num_config < 1
            {
                tracing::warn!("dmabuf: eglChooseConfig found no config");
                return None;
            }
            let ctx_attribs: [ffi::EGLint; 3] = [ffi::EGL_CONTEXT_CLIENT_VERSION, 2, ffi::EGL_NONE];
            let context =
                ffi::eglCreateContext(display, config, std::ptr::null_mut(), ctx_attribs.as_ptr());
            if context.is_null() {
                tracing::warn!(
                    "dmabuf: eglCreateContext failed (0x{:x})",
                    ffi::eglGetError()
                );
                return None;
            }
            // Surfaceless — WebKit renders offscreen; we never present here.
            if ffi::eglMakeCurrent(display, std::ptr::null_mut(), std::ptr::null_mut(), context)
                != ffi::EGL_TRUE
            {
                tracing::warn!("dmabuf: eglMakeCurrent (surfaceless) failed");
                return None;
            }

            let query_fn = load_proc("eglExportDMABUFImageQueryMESA")?;
            let export_fn = load_proc("eglExportDMABUFImageMESA")?;
            let query_fn: ffi::PFNEglExportDmabufImageQueryMesa = std::mem::transmute(query_fn);
            let export_fn: ffi::PFNEglExportDmabufImageMesa = std::mem::transmute(export_fn);

            tracing::info!("dmabuf: EGL {major}.{minor} ready on {node_path}");
            Some(Self {
                _node: node,
                display,
                query_fn,
                export_fn,
            })
        }
    }

    /// Export a WebKit `EGLImageKHR` to a single-plane dmabuf. Returns
    /// `None` if the image isn't single-plane or the export fails.
    pub fn export(&self, image: ffi::EGLImageKHR, width: u32, height: u32) -> Option<DmabufFrame> {
        unsafe {
            let (mut fourcc, mut num_planes, mut modifier) = (0i32, 0i32, 0u64);
            if (self.query_fn)(
                self.display,
                image,
                &mut fourcc,
                &mut num_planes,
                &mut modifier,
            ) != ffi::EGL_TRUE
            {
                tracing::warn!("dmabuf: export query failed (0x{:x})", ffi::eglGetError());
                return None;
            }
            if num_planes < 1 || num_planes > 4 {
                tracing::warn!("dmabuf: implausible plane count {num_planes}");
                return None;
            }
            let mut fds = [-1i32; 4];
            let mut strides = [0i32; 4];
            let mut offsets = [0i32; 4];
            if (self.export_fn)(
                self.display,
                image,
                fds.as_mut_ptr(),
                strides.as_mut_ptr(),
                offsets.as_mut_ptr(),
            ) != ffi::EGL_TRUE
                || fds[0] < 0
            {
                tracing::warn!("dmabuf: export failed (0x{:x})", ffi::eglGetError());
                return None;
            }
            let planes = (0..num_planes as usize)
                .map(|i| {
                    Some(DmabufPlane {
                        fd: std::os::fd::OwnedFd::from_raw_fd_checked(fds[i])?,
                        stride: strides[i] as u32,
                        offset: offsets[i] as u32,
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            Some(DmabufFrame {
                planes,
                fourcc: fourcc as u32,
                modifier,
                width,
                height,
            })
        }
    }
}

/// Load an EGL extension entry point by name.
fn load_proc(name: &str) -> Option<*mut std::ffi::c_void> {
    let c = std::ffi::CString::new(name).ok()?;
    let p = unsafe { ffi::eglGetProcAddress(c.as_ptr()) };
    if p.is_null() {
        tracing::warn!("dmabuf: {name} not available");
        return None;
    }
    Some(p)
}

/// A four-char code as a readable string, for logging.
pub fn fourcc_str(fourcc: u32) -> String {
    let b = fourcc.to_le_bytes();
    b.iter().map(|&c| c as char).collect()
}

trait OwnedFdExt: Sized {
    fn from_raw_fd_checked(fd: i32) -> Option<Self>;
}
impl OwnedFdExt for std::os::fd::OwnedFd {
    fn from_raw_fd_checked(fd: i32) -> Option<Self> {
        use std::os::fd::FromRawFd;
        (fd >= 0).then(|| unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
    }
}
