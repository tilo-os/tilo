//! qOS graphical greeter — library root.
//!
//! The greeter is a thin `ui`-toolkit app that runs inside `cage` (a
//! kiosk wlroots compositor) at the greetd login VT. It collects a
//! username + password, lets the user pick a Wayland session, and speaks
//! greetd's IPC protocol to authenticate and launch the session.
//!
//! Modules:
//! * [`sessions`] — parse `/usr/share/wayland-sessions/*.desktop`.
//! * [`greetd_client`] — the greetd auth/start exchange over `$GREETD_SOCK`.
//! * [`auth`] — background auth worker + the UI-facing update protocol.
//! * [`greeter_app`] — the root [`GreeterApp`] `Widget` (the login card).

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a test that unwraps is stating an invariant"
    )
)]

pub mod auth;
pub mod greetd_client;
pub mod greeter_app;
pub mod sessions;

pub use greeter_app::GreeterApp;
pub use sessions::{Session, read_sessions};
