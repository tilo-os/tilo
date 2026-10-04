//! Remote — a Tilo native app for reaching other machines: SSH terminals,
//! VNC/RDP remote desktops, and Tilo-to-Tilo relay sessions.
//!
//! The app is built on the CRDT-native `ui` toolkit. Its durable state —
//! the list of saved connections — lives in the shared Automerge document
//! ([`doc`]), so a machine list edited on one device converges on every
//! device the document is shared with. View state (which screen you're
//! on, the in-progress add/edit form, the active session) is per-peer, so
//! collaborators can browse the same list while each looking at a
//! different machine.
//!
//! ## Screens
//!
//! * **List** — saved connections with name, address, protocol badge,
//!   online status, last-used, favorites, search, and per-row Connect /
//!   Edit actions.
//! * **Form** — add or edit a connection: protocol selector
//!   (SSH / VNC / RDP / Tilo), host/port/user/auth fields, validation,
//!   and a discovered-Tilo-device picker.
//! * **Session** — the live surface. SSH opens an embedded terminal grid;
//!   VNC/RDP open a remote-framebuffer canvas with a toolbar (fullscreen,
//!   scaling, disconnect). Clipboard synchronization is not advertised until
//!   the transports provide a real consent-aware channel.
//!
//! ## Tilo relay
//!
//! A Tilo device is modelled as a distinct connection [`kind`] because it
//! does not create an app-owned remote-desktop protocol. It asks the session
//! broker for an authenticated exact-target device-mesh route, then consumes
//! the compositor's existing consented workspace-surface stream and sends
//! input through the existing target-side firewall. Workspace-wide relay
//! fan-out is refused because it cannot keep control input target-confidential.
//!
//! [`kind`]: model::ConnectionKind

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod app;
pub mod doc;
pub mod model;
pub mod resource;
pub mod rfb;
pub mod session;
pub mod ssh;
mod theme_ext;
pub mod tilo_route;
pub mod vnc;
pub mod vt;

pub use resource::{Remote, RemoteShell};
