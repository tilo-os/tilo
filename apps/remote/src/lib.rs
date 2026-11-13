//! Remote: SSH terminals, VNC/RDP desktops and Tilo-to-Tilo relay sessions. The saved
//! connections are the [`model::Remote`] record, so a machine list edited on one device
//! converges everywhere; the screen, the draft and the live session are per-peer state. A
//! Tilo device is a connection kind of its own, served by the session broker's
//! authenticated route to that exact device.

// An app runs as its own sandboxed process: a crash costs one app, not the desktop,
// so wrapping arithmetic and casts are allowed here. The panic lints stay denied.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "app-local arithmetic; see the note above"
)]
pub mod actions;
pub mod app;
pub mod model;
pub mod resource;
pub mod rfb;
#[cfg(feature = "samples")]
mod sample;

pub mod session;
pub mod ssh;
mod view;
pub mod vnc;
pub mod vt;

pub use resource::{Remote, RemoteShell};
