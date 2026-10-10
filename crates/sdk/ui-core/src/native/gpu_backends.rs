//! Which wgpu backends a native process asks for when `WGPU_BACKEND` names
//! none and its platform has no fixed choice (`crate::gpu::RenderContext::new`:
//! Windows asks for DX12, Android for Vulkan).
//!
//! wgpu initialises every backend it is asked for as the instance is made,
//! and for OpenGL that means EGL, a Mesa driver and a GL context: on lavapipe
//! +25 ms, +17 threads and +28 MB per process, in every app, on the thread
//! whose device the first frame waits for (TIL-775). The adapter is Vulkan's
//! whenever both backends offer one of the same kind, so OpenGL only wins when
//! a GPU has an OpenGL driver and no Vulkan one. That cannot be the case when
//!
//! - the launcher pinned the Vulkan driver of the compositor's render node
//!   (`VK_DRIVER_FILES`/`VK_ICD_FILENAMES`, `launcher::gpu`): the GPU has one;
//! - there is no DRM render node at all: on Linux both backends could only
//!   offer a software rasteriser, and lavapipe is preferred over llvmpipe's
//!   OpenGL; on macOS OpenGL needs an ANGLE nobody ships.
//!
//! Then every backend but OpenGL is asked, and OpenGL only if those offer no
//! adapter. Otherwise — a GPU whose Vulkan driver nobody matched — every
//! backend is asked, as before, so its OpenGL driver can still beat lavapipe.

use std::path::Path;

use crate::gpu::BackendChoice;

/// This process's backends, from its environment and its devices.
pub(crate) fn default_choice() -> BackendChoice {
    backends_for(vulkan_driver_pinned(), render_node_present())
}

/// [`default_choice`] for a process whose Vulkan loader was (`vulkan_pinned`)
/// or was not told which driver to load, and that does or does not see a
/// render node.
pub(crate) fn backends_for(vulkan_pinned: bool, render_node: bool) -> BackendChoice {
    if vulkan_pinned || !render_node {
        BackendChoice {
            primary: wgpu::Backends::all().difference(wgpu::Backends::GL),
            fallback: Some(wgpu::Backends::GL),
        }
    } else {
        BackendChoice {
            primary: wgpu::Backends::all(),
            fallback: None,
        }
    }
}

/// Whether this process's Vulkan loader was told which driver to load: the
/// launcher's GPU pin, or a person's own choice.
fn vulkan_driver_pinned() -> bool {
    ["VK_DRIVER_FILES", "VK_ICD_FILENAMES"]
        .into_iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

/// Whether any DRM render node is visible: inside a sandbox, only the GPU's
/// the launcher bound.
fn render_node_present() -> bool {
    tilo_files::file_at(Path::new("/dev/dri"))
        .and_then(|(dev, dri)| tilo_files::blocking::list(&dev, &dri))
        .is_ok_and(|entries| {
            entries
                .iter()
                .any(|entry| entry.name.starts_with("renderD"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wgpu::Backends;

    #[test]
    fn opengl_is_only_asked_for_where_a_gpu_may_lack_a_vulkan_driver() {
        let without_opengl = BackendChoice {
            primary: Backends::all().difference(Backends::GL),
            fallback: Some(Backends::GL),
        };
        assert!(without_opengl.primary.contains(Backends::VULKAN));
        // The launcher pinned the render node's Vulkan driver.
        assert_eq!(backends_for(true, true), without_opengl);
        assert_eq!(backends_for(true, false), without_opengl);
        // No GPU at all: lavapipe beats llvmpipe's OpenGL either way.
        assert_eq!(backends_for(false, false), without_opengl);
        // A GPU whose Vulkan driver nobody matched may only have OpenGL.
        assert_eq!(
            backends_for(false, true),
            BackendChoice {
                primary: Backends::all(),
                fallback: None,
            }
        );
    }
}
