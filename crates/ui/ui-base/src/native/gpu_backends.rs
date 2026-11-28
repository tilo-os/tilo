//! Which wgpu backends a native process asks for when `WGPU_BACKEND` names none and its platform has no
//! fixed choice (`crate::gpu::RenderContext::new`: Windows asks for DX12, Android for Vulkan). wgpu
//! initialises every backend it is asked for as the instance is made, and for OpenGL that means EGL, a
//! Mesa driver and a GL context: on lavapipe +25 ms, +17 threads and +28 MB per process, in every app,
//! on the thread whose device the first frame waits for. That cannot be the case when - the launcher
//! pinned the Vulkan driver of the compositor's render node (`VK_DRIVER_FILES`/`VK_ICD_FILENAMES`,
//! `launcher::gpu`): the GPU has one; - there is no DRM render node at all: on Linux both backends
//! could only offer a software rasteriser, and lavapipe is preferred over llvmpipe's OpenGL; on macOS
//! OpenGL needs an ANGLE nobody ships.
//!
//! The process that drives the display itself ([`beside_the_display`]) never asks for OpenGL up front
//! either: it makes its instance before its compositor opens the display's device through logind, and
//! Mesa's EGL, finding a render node it cannot draw on (a VM's virtio-gpu without 3D), falls back to
//! its software KMS driver, which opens the primary node. The first process to open a primary node
//! that has no DRM master becomes its master, so logind's `SET_MASTER` for the compositor fails and
//! the desktop cannot start.

use std::path::Path;

use crate::gpu::BackendChoice;

/// This process's backends, from its environment and its devices.
pub(crate) fn default_choice() -> BackendChoice {
    backends_for(vulkan_driver_pinned(), render_node_present())
}

/// [`default_choice`] for a process whose Vulkan loader was (`vulkan_pinned`) or was not told which
/// driver to load, and that does or does not see a render node.
pub(crate) fn backends_for(vulkan_pinned: bool, render_node: bool) -> BackendChoice {
    if vulkan_pinned || !render_node {
        WITHOUT_OPENGL
    } else {
        BackendChoice {
            primary: wgpu::Backends::all(),
            fallback: None,
        }
    }
}

/// The backends of the process whose compositor drives the display, made as it starts, before the
/// compositor opened the display's device: OpenGL only once no other backend offers an adapter, which
/// is asked when the first frame is drawn, after the compositor became the device's master. Whatever
/// its GPU, this process's frames go to its compositor in dma-bufs a Vulkan device drew
/// (`tilo_ui_render::dmabuf_import`): an OpenGL adapter would draw them on the CPU just the same.
pub(crate) const fn beside_the_display() -> BackendChoice {
    WITHOUT_OPENGL
}

/// Every backend but OpenGL, and OpenGL when none of them offers an adapter.
const WITHOUT_OPENGL: BackendChoice = BackendChoice {
    primary: wgpu::Backends::all().difference(wgpu::Backends::GL),
    fallback: Some(wgpu::Backends::GL),
};

/// Whether this process's Vulkan loader was told which driver to load: the launcher's GPU pin, or a
/// person's own choice. Whether `WGPU_ADAPTER_NAME` names the adapter wgpu must pick
/// (`wgpu::util::initialize_adapter_from_env_or_default`).
pub(crate) fn adapter_named() -> bool {
    std::env::var_os("WGPU_ADAPTER_NAME").is_some()
}

fn vulkan_driver_pinned() -> bool {
    ["VK_DRIVER_FILES", "VK_ICD_FILENAMES"]
        .into_iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

/// Whether any DRM render node is visible: inside a sandbox, only the GPU's the launcher bound.
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
        assert_eq!(backends_for(true, true), without_opengl);
        assert_eq!(backends_for(true, false), without_opengl);
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

    #[test]
    fn the_process_that_drives_the_display_asks_for_opengl_only_when_nothing_else_draws() {
        // What the desktop asks for as it starts, on a VM whose virtio-gpu has a render node and no
        // 3D: an OpenGL instance there opens the primary node before the compositor can.
        let choice = beside_the_display();
        assert!(
            !choice.primary.contains(Backends::GL),
            "OpenGL is not initialised before the compositor holds the display's device"
        );
        assert!(choice.primary.contains(Backends::VULKAN));
        assert_eq!(choice.fallback, Some(Backends::GL));
    }
}
