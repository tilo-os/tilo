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
//! * [`greeter_app`] — the root [`GreeterApp`] `Widget` (the login card).

pub mod sessions;
pub mod greetd_client;
pub mod greeter_app;

pub use greeter_app::GreeterApp;
pub use sessions::{read_sessions, Session};
