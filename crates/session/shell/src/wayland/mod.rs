//! Wayland extension protocols the compositor implements outside of
//! smithay's `delegate_*!` wrappers.
//!
//! smithay 0.7 ships handlers for the core protocols (`wl_compositor`,
//! `xdg-shell`, `wl_shm`, `wl_seat`, `wl_output`, `zwp_linux_dmabuf_v1`,
//! …), but the staging + wp/* extensions we care about — starting with
//! `color-management-v1` — have no ready-made handler. This module
//! hosts our hand-rolled `wayland_server::{GlobalDispatch, Dispatch}`
//! implementations.

#[cfg(target_os = "linux")]
pub mod color_management;
pub mod edid;
