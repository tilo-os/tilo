//! Remote — a Tilo native app for reaching other machines: SSH terminals,
//! VNC/RDP remote desktops, and Tilo-to-Tilo relay sessions.
//!
//! The app is built on the CRDT-native `ui` toolkit. Its durable state —
//! the list of saved connections — is a typed model ([`model::Remote`])
//! stored in the shared document, so a machine list edited on one device
//! converges on every device the document is shared with. View state (which screen you're
//! on, the in-progress add/edit form, the active session) is per-peer, so
//! collaborators can browse the same list while each looking at a
//! different machine.
//!
//! ## Screens
//!
//! The window is a navigation shell: a docked rail (all connections, the
//! pinned ones, the open session) beside the current screen.
//!
//! * **List** — saved connections: protocol, name, address, whether the
//!   machine answered last time and when it was last used (or why it cannot
//!   be opened here). Activating a row connects; editing and pinning are
//!   row actions. Finding a connection is the platform search.
//! * **Form** — add or edit a connection: protocol, the Tilo devices
//!   nearby, host, port, user, group and authentication, checked on Save.
//!   A password or key passphrase is kept in this device's secret store,
//!   never in the shared document.
//! * **Session** — the live surface. SSH opens an embedded terminal grid;
//!   VNC and Tilo devices open the remote desktop, which can fit the window
//!   or fill it. A session that shows no surface (connecting, failed,
//!   unavailable, ended) says why in one sentence and offers the way
//!   forward. Clipboard synchronization is not advertised until the
//!   transports provide a real consent-aware channel.
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
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod app;
pub mod model;
pub mod resource;
pub mod rfb;
mod sample;
pub mod session;
pub mod ssh;
pub mod tilo_route;
mod view;
pub mod vnc;
pub mod vt;

pub use resource::{Remote, RemoteShell};
