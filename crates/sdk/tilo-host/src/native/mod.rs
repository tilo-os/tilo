//! The native desktop backends for [`crate::host::HostServices`], behind the
//! `native` feature.
//!
//! Everything here talks to the operating system on the app's behalf: the
//! desktop implementation of [`crate::host::HostServices`] ([`services`]), the
//! Wayland clipboard, the XDG portal and Windows dialog backends, MPRIS on the
//! session bus and Windows toasts. On Android [`services`] resolves to
//! [`android`], which reaches the framework through the activity instead.
//!
//! Without the feature the crate is the host vocabulary only, and whoever
//! hosts the toolkit installs its own services with
//! [`crate::host::install_default`].

#[cfg(target_os = "android")]
pub mod android;
pub mod capture;
#[cfg(all(windows, feature = "notifications"))]
pub mod host_notifications;
pub mod location;
pub mod mpris;
#[cfg(not(target_os = "android"))]
pub mod portal;
pub mod services;
#[cfg(target_os = "linux")]
pub mod wayland_clipboard;
