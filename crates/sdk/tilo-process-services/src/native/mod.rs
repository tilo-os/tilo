//! The native desktop backends for [`crate::process::ProcessServices`], behind the
//! `native` feature.
//!
//! Everything here talks to the operating system on the app's behalf: the
//! desktop implementation of [`crate::process::ProcessServices`] ([`services`]), the
//! XDG portal and Windows dialog backends, MPRIS on the
//! session bus and Windows toasts.
//!
//! Without the feature the crate is the process vocabulary only, and whoever
//! processes the toolkit installs its own services with
//! [`crate::process::install_default`].

#[cfg(target_os = "android")]
pub mod android;
pub mod capture;
pub mod mpris;
pub mod services;
