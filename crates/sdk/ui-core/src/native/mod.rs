//! The native desktop side of the toolkit, behind the default-on `native`
//! feature.
//!
//! Everything here talks to the operating system on the app's behalf: the
//! desktop implementation of [`crate::host::HostServices`] ([`services`]), the
//! Wayland clipboard, the XDG portal and Windows dialog backends, MPRIS on the
//! session bus, Windows toasts and cold-start warmup. The winit event-loop
//! runner lives beside `App` (it drives `App`'s private state) and is compiled
//! only with this feature too; [`crate::App::run`] and [`crate::launch::run`]
//! are its entry points.
//!
//! Without the feature the crate is the portable toolkit — widgets, state,
//! layout, rendering and the headless host — and whoever hosts it installs
//! its own services with [`crate::host::install_default`].

#[cfg(all(windows, feature = "notifications"))]
pub mod host_notifications;
pub mod mpris;
pub mod portal;
pub mod services;
pub mod warmup;
#[cfg(target_os = "linux")]
pub mod wayland_clipboard;
