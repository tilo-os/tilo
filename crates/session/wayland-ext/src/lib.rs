//! Wayland extension protocols the compositor implements outside of
//! smithay's `delegate_*!` wrappers.
//!
//! smithay 0.7 ships handlers for the core protocols (`wl_compositor`,
//! `xdg-shell`, `wl_shm`, `wl_seat`, `wl_output`, `zwp_linux_dmabuf_v1`,
//! …), but the staging + wp/* extensions we care about — starting with
//! `color-management-v1` — have no ready-made handler. This crate
//! hosts our hand-rolled `wayland_server::{GlobalDispatch, Dispatch}`
//! implementations, and the EDID parsing that tells them what a display
//! can show. It sits below the compositor so the compositor's own crate
//! does not rebuild it, and its tests run on their own.

// Only the Linux Wayland dispatcher reads this color state; other hosts
// build it for its tests.
#[cfg(any(test, target_os = "linux"))]
pub mod color_management;
pub mod edid;
