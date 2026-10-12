//! Tilo graphical greeter — library root.
//!
//! The greeter is a thin `ui`-toolkit app that runs inside `cage` (a
//! kiosk wlroots compositor) at the greetd login VT. On an unambiguous
//! personal machine it asks only for the installed user's passcode; ambiguous
//! machines retain username + passcode fields. When more than one Wayland
//! session is installed it offers the choice, and it speaks greetd's IPC
//! protocol to authenticate and launch the session.
//!
//! Modules:
//! * [`sessions`] — parse `/usr/share/wayland-sessions/*.desktop`.
//! * [`greetd_client`] — the greetd auth/start exchange over `$GREETD_SOCK`
//!   (its own crate, so the compositor's lock screen shares it without
//!   depending on the greeter).
//! * [`auth`] — background auth worker + the UI-facing update protocol.
//! * [`device_status`] — battery and Wi-Fi for the status corner.
//! * [`greeter_app`] — the root [`GreeterApp`] `Widget` (the sign-in panel).
//! * [`i18n`] — the sign-in screen's string catalogue.
//! * [`local_users`] — conservative local-account discovery for one-passcode login.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod auth;
pub mod device_status;
pub mod greeter_app;
pub mod i18n;
pub mod local_users;
pub mod output_scale;
pub mod sessions;

pub use greetd_client;
pub use greeter_app::GreeterApp;
pub use sessions::{Session, read_sessions};
