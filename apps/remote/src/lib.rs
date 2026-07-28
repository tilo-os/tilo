//! Remote — a qOS native app for reaching other machines: SSH terminals,
//! VNC/RDP remote desktops, and qOS-to-qOS relay sessions.
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
//!   (SSH / VNC / RDP / qOS), host/port/user/auth fields, validation,
//!   and a discovered-qOS-device picker.
//! * **Session** — the live surface. SSH opens an embedded terminal grid;
//!   VNC/RDP open a remote-framebuffer canvas with a toolbar (fullscreen,
//!   scaling, clipboard, disconnect).
//!
//! ## qOS relay
//!
//! A qOS device is modelled as a distinct connection [`kind`] because it
//! is fundamentally different from a screen-scraping remote desktop: the
//! peer does **not** stream pixels. It relays application and workspace
//! state over the compositor's P2P fabric, and we render that state
//! *locally* with this same toolkit. That keeps qOS-to-qOS sessions crisp,
//! theme-consistent, and bandwidth-light.
//!
//! [`kind`]: model::ConnectionKind

pub mod app;
pub mod doc;
pub mod model;
pub mod rfb;
pub mod session;
pub mod ssh;
pub mod vnc;
pub mod vt;
mod theme_ext;

use ui::widget::Widget;

/// Build the live app: it starts empty and shows only the user's real saved
/// connections. It never fabricates a machine list.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(app::RemoteApp::new())
}

/// Build the live app staged with a named `--shots` scenario. Empty on a
/// fresh machine — the responsive-layout scenarios still apply.
///
/// Recognised scenarios:
/// * `"narrow"` — collapse the navigation rail into its closed overlay flap,
///   for the responsive small-viewport capture.
pub fn app_root_for_shot(scenario: &str) -> Box<dyn Widget> {
    let mut app = app::RemoteApp::new();
    app.dev_shot_scenario(scenario);
    Box::new(app)
}

/// Build the sample-seeded demo app staged with a `--shots` scenario. This
/// is the ONLY entry point that fabricates a connection list — used by the
/// screenshot gallery so a populated frame is captured.
pub fn app_root_for_shot_demo(scenario: &str) -> Box<dyn Widget> {
    let mut app = app::RemoteApp::new_demo();
    // Responsive-rail scenarios and screen scenarios are both dispatched by
    // name; each ignores a name it doesn't recognise.
    app.dev_shot_scenario(scenario);
    app.dev_set_screen(scenario);
    Box::new(app)
}
