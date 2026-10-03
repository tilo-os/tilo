//! Tilo graphical greeter — library root.
//!
//! The greeter is a thin `ui`-toolkit app that runs inside `cage` (a
//! kiosk wlroots compositor) at the greetd login VT. On an unambiguous
//! personal machine it asks only for the installed user's passcode; ambiguous
//! machines retain username + passcode fields. It lets the user pick a Wayland
//! session and speaks greetd's IPC protocol to authenticate and launch it.
//!
//! Modules:
//! * [`sessions`] — parse `/usr/share/wayland-sessions/*.desktop`.
//! * [`greetd_client`] — the greetd auth/start exchange over `$GREETD_SOCK`
//!   (its own crate, so the compositor's lock screen shares it without
//!   depending on the greeter).
//! * [`auth`] — background auth worker + the UI-facing update protocol.
//! * [`greeter_app`] — the root [`GreeterApp`] `Widget` (the login card).
//! * [`local_users`] — conservative local-account discovery for one-passcode login.

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

pub mod auth;
pub mod greeter_app;
pub mod local_users;
pub mod sessions;

pub use greetd_client;
pub use greeter_app::GreeterApp;
pub use sessions::{Session, read_sessions};
