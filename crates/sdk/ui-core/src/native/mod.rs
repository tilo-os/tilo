//! The native desktop side of the toolkit, behind the default-on `native`
//! feature.
//!
//! The host-service backends — the desktop implementation of
//! [`crate::host::HostServices`] ([`services`]), the location and
//! screen-capture portals and MPRIS on the session bus — live in `tilo-host`
//! and the platform crates, and are re-exported here under their usual paths. Cold-start warmup is the toolkit's own. The winit event-loop
//! runner lives beside `App` (it drives `App`'s private state) and is compiled
//! only with this feature too; [`crate::App::run`] and [`crate::launch::run`]
//! are its entry points.
//!
//! Without the feature the crate is the portable toolkit — widgets, state,
//! layout, rendering and the headless host — and whoever hosts it installs
//! its own services with [`crate::host::install_default`].

#[cfg(target_os = "android")]
#[doc(inline)]
pub use tilo_host::native::android;
#[doc(inline)]
pub use tilo_host::native::{capture, mpris, services};
#[cfg(windows)]
#[doc(inline)]
pub use tilo_host::notify::windows_toasts as host_notifications;
#[cfg(target_os = "linux")]
pub(crate) mod gpu_backends;
pub mod warmup;
