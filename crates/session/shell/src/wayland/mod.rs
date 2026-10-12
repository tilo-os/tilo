//! Wayland extension protocols the compositor implements outside of
//! smithay's `delegate_*!` wrappers.
//!
//! smithay 0.7 ships handlers for the core protocols (`wl_compositor`,
//! `xdg-shell`, `wl_shm`, `wl_seat`, `wl_output`, `zwp_linux_dmabuf_v1`,
//! …), but the staging + wp/* extensions we care about — starting with
//! `color-management-v1` — have no ready-made handler. This module
//! hosts our hand-rolled `wayland_server::{GlobalDispatch, Dispatch}`
//! implementations.

#[cfg(any(test, target_os = "linux"))]
#[cfg_attr(
    not(target_os = "linux"),
    expect(
        dead_code,
        reason = "only the Linux Wayland dispatcher reads this color state; other hosts build it for its tests"
    )
)]
pub mod color_management;
pub mod edid;
