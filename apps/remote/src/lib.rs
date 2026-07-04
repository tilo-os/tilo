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
pub mod session;
mod theme_ext;

use ui::widget::Widget;

/// Build and return the app's root widget tree. Single integration point
/// used by both the live event loop and the `--shots` offscreen renderer.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(app::RemoteApp::new())
}

/// Like [`app_root`] but staged with a named scenario for a headless
/// screenshot. An empty/unknown name yields the default frame.
///
/// Recognised scenarios:
/// * `"narrow"` — collapse the navigation rail into its closed overlay flap,
///   for the responsive small-viewport capture.
pub fn app_root_for_shot(scenario: &str) -> Box<dyn Widget> {
    let mut app = app::RemoteApp::new();
    app.dev_shot_scenario(scenario);
    Box::new(app)
}
