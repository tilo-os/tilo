//! The host side of first use: everything setup does that the setup app
//! (`tilo-onboarding`) only asks for. The identity ceremony and its storage,
//! first-boot provisioning, pairing with a nearby device, a setup prepared
//! on another computer and the transfers it carries over (a portable home,
//! its applications, browser data, the files of the system Tilo replaced),
//! and the custody of every secret those steps take.
//!
//! The session that runs setup lends what only it has through
//! [`host::SessionHost`].

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod firstboot;
pub mod host;
pub mod identity;
pub mod identity_provisioning;
pub mod nearby;

#[cfg(target_os = "linux")]
pub mod browser;
#[cfg(target_os = "linux")]
pub mod continuation;
#[cfg(target_os = "linux")]
pub mod migration;
#[cfg(target_os = "linux")]
pub mod peer;
#[cfg(target_os = "linux")]
pub mod portable_applications;
#[cfg(target_os = "linux")]
pub mod portable_home;
#[cfg(target_os = "linux")]
pub mod prepared;
#[cfg(target_os = "linux")]
pub mod retained;

#[cfg(not(target_os = "linux"))]
mod elsewhere;
#[cfg(not(target_os = "linux"))]
pub use elsewhere::{browser, migration, portable_applications, portable_home, retained};
