//! The native desktop side of the toolkit, behind the default-on `native` feature. The host-service
//! backends — the desktop implementation of [`crate::host::HostServices`] ([`services`]), the location
//! and screen-capture portals and MPRIS on the session bus — live in `tilo-host` and the platform
//! crates, and are re-exported here under their usual paths.

#[cfg(target_os = "android")]
#[doc(inline)]
pub use tilo_host::native::android;
#[doc(inline)]
pub use tilo_host::native::{capture, mpris, services};
#[cfg(windows)]
#[doc(inline)]
pub use tilo_host::notify::windows_toasts as host_notifications;
pub(crate) mod gpu_backends;
pub(crate) mod standalone_instance;
pub mod warmup;
