//! tilolang: the host app that runs every tilolang app package.
//!
//! One Tilo app, `so.tilo.lang`, hosts every package in process. Each package is compiled at
//! startup from the source embedded with the app, runs in its own wasm instance, and keeps its
//! resources in the room's record. A window runs one package ([`app`]) through the framework's
//! root, its views rendered onto the native shell ([`window`]).

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "layout weights, counts and sizes are small values; see crates/platform/tilolang-tilo"
)]

pub mod app;
pub mod dev;
pub mod i18n;
pub mod names;
pub mod packages;
pub mod page;
pub mod reconcile;
pub mod resource;
pub mod search;
pub mod window;

use std::sync::OnceLock;

use ui::application::ApplicationDefinition;

/// The app id of the host.
pub const APP_ID: &str = "so.tilo.lang";

/// The record kind of the room's tilolang record.
pub const RECORD_KIND: &str = "tilolang.room";

/// The surface file the host registers for its app id: the host resource and every built-in
/// package's kinds and verbs, generated from the packages' metadata.
fn surface_bytes() -> Vec<u8> {
    let sources: Vec<(&str, &str)> = packages::EMBEDDED
        .iter()
        .map(|package| (package.source, package.ids))
        .collect();
    tilolang_tilo::host::builtin_surface(&sources)
}

/// The app as every host mounts it: through the framework's root, like every native app.
pub fn definition() -> &'static ApplicationDefinition {
    // Receivers decide each change against this surface; name it before the first signed
    // operation.
    static SURFACE: OnceLock<()> = OnceLock::new();
    SURFACE.get_or_init(|| ui::app_surface::register_package(APP_ID, &surface_bytes()));
    ui::framework::definition::<app::TlApp>()
}

/// How the host launches: the live window, or a capture of one of the package's scenarios.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    let _ = definition();
    ui::framework::launch_spec::<app::TlApp>()
}
