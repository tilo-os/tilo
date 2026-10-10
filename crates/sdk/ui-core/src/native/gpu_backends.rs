//! Which wgpu backends a Linux process asks for when `WGPU_BACKEND` names
//! none (`crate::gpu::RenderContext::new`).
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
//! - there is no render node at all: both backends could only offer a
//!   software rasteriser, and lavapipe is preferred over llvmpipe's OpenGL.
//!
//! Then Vulkan is asked alone, and OpenGL only if Vulkan offers no adapter.
//! Otherwise — a GPU whose Vulkan driver nobody matched — every backend is
//! asked, as before, so its OpenGL driver can still beat lavapipe.

use crate::gpu::BackendChoice;

/// This process's backends, from its environment and its devices.
pub(crate) fn linux_default() -> BackendChoice {
    linux_backends(vulkan_driver_pinned(), render_node_present())
}

/// [`linux_default`] for a process whose Vulkan loader was (`vulkan_pinned`)
/// or was not told which driver to load, and that does or does not see a
/// render node.
pub(crate) fn linux_backends(vulkan_pinned: bool, render_node: bool) -> BackendChoice {
    if vulkan_pinned || !render_node {
        BackendChoice {
            primary: wgpu::Backends::VULKAN,
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
    std::fs::read_dir("/dev/dri").is_ok_and(|entries| {
        entries
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().starts_with("renderD"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wgpu::Backends;

    #[test]
    fn linux_asks_for_vulkan_alone_unless_a_gpu_may_lack_a_vulkan_driver() {
        let vulkan_first = BackendChoice {
            primary: Backends::VULKAN,
            fallback: Some(Backends::GL),
        };
        // The launcher pinned the render node's Vulkan driver.
        assert_eq!(linux_backends(true, true), vulkan_first);
        assert_eq!(linux_backends(true, false), vulkan_first);
        // No GPU at all: lavapipe beats llvmpipe's OpenGL either way.
        assert_eq!(linux_backends(false, false), vulkan_first);
        // A GPU whose Vulkan driver nobody matched may only have OpenGL.
        assert_eq!(
            linux_backends(false, true),
            BackendChoice {
                primary: Backends::all(),
                fallback: None,
            }
        );
    }
}
