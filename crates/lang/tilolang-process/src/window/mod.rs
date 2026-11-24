//! An app's windows, in the process that runs the app's code.
//!
//! A tilolang package is an app of its own: its signed manifest names the tilolang runtime
//! bundle as its entrypoint (`[entrypoint] runtime = "tilolang/<version>"`), and the launcher
//! starts this process's binary from that bundle with `TILO_PACKAGE_ID` (the package's signed id)
//! and `TILO_APP_MODULE` (its payload: the compiled module, its metadata and its surface). The
//! window builds the app's definition from them, so each package has its own id, window,
//! document, ownership and permissions. It compiles nothing: the package was compiled by
//! `tilolang build`.
//!
//! In a session the window draws what the app's instance in this binary's host sends it
//! ([`hosted::HostedWindow`]); without one (`tilolang dev`, a capture) the window runs the
//! instance in its own process, on the same host code.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "layout weights, counts and sizes are small values; see crates/lang/tilolang-tilo"
)]

pub mod app;
pub mod hosted;
pub mod i18n;
pub mod resource;
pub mod shown_on_page;
#[cfg(target_os = "linux")]
pub mod spare;
pub mod updates;

use std::sync::{Arc, OnceLock};

use tilolang_tilo::bundle::{Bundle, BundleError};
use ui::application::ApplicationDefinition;

/// The id the runtime answers to when no package is installed in it: only tests and captures of
/// the built-in packages run like that.
pub const UNBUNDLED_ID: &str = "so.tilo.lang";

/// The resource kind of the runtime's own document: the host package's, which every tilolang
/// surface publishes.
pub const RESOURCE_TYPE: &str = tilolang_tilo::surface::HOST_KIND;

/// The variable the launcher names the package's signed id with.
pub const PACKAGE_ID_ENV: &str = "TILO_PACKAGE_ID";

static INSTALLED: OnceLock<Arc<Bundle>> = OnceLock::new();

/// Why the runtime cannot run the package it was started for.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error(transparent)]
    Bundle(#[from] BundleError),
    #[error(
        "the package was signed as `{signed}`, but its module declares the app `{declared}`; \
         the runtime runs a module only under the id it was signed for"
    )]
    IdMismatch { signed: String, declared: String },
    #[error("a package is already installed in this process")]
    AlreadyInstalled,
}

/// Run `bundle` in this process: the definition every host mounts is its app's. Once per
/// process, before the definition is first asked for.
///
/// # Errors
/// A package is already installed.
pub fn install(bundle: Bundle) -> Result<(), InstallError> {
    // The files the package ships beside its sources (`pkg:media/…`): what a view or a scenario
    // names is staged from them, so nothing is fetched.
    tilo_app_window::media_source::install_package_assets(
        bundle
            .assets
            .iter()
            .map(|asset| (asset.name.clone(), asset.bytes.clone())),
    );
    INSTALLED
        .set(Arc::new(bundle))
        .map_err(|_| InstallError::AlreadyInstalled)
}

/// Install the package the launcher started this process for: the payload `TILO_APP_MODULE`
/// names, under the id `TILO_PACKAGE_ID` names.
pub fn install_from_env() -> Result<(), InstallError> {
    let bundle = Bundle::from_env()?;
    if let Some(signed) = std::env::var_os(PACKAGE_ID_ENV).filter(|id| !id.is_empty()) {
        let signed = signed.to_string_lossy();
        if signed != bundle.meta.app.as_str() {
            return Err(InstallError::IdMismatch {
                signed: signed.into_owned(),
                declared: bundle.meta.app,
            });
        }
    }
    install(bundle)
}

/// The app a spare window process was bound to: its windows are that app's, and show instances
/// of it a tilolang process runs. The process holds no package of the app.
#[derive(Debug)]
pub struct Bound {
    pub app: String,
    pub title: String,
    /// The icon its package declares (`icon: .file_text`).
    pub icon: Option<String>,
    /// Its translations, each locale's catalogue.
    pub catalogues: Vec<(String, String)>,
}

static BOUND: OnceLock<Bound> = OnceLock::new();

/// Bind this process to `bound`, once, before its first window is built.
///
/// # Errors
/// It was bound already.
pub fn bind(bound: Bound) -> Result<(), String> {
    ui::launch::bind_window_process(&bound.app);
    BOUND
        .set(bound)
        .map_err(|_| "a window process is bound once".to_owned())
}

/// The app this process was bound to, once it was.
#[must_use]
pub fn bound() -> Option<&'static Bound> {
    BOUND.get()
}

/// The package this process runs, once installed.
#[must_use]
pub fn installed() -> Option<Arc<Bundle>> {
    INSTALLED.get().cloned()
}

/// The installed package, for as long as the process runs.
#[must_use]
pub fn installed_bundle() -> Option<&'static Bundle> {
    INSTALLED.get().map(|bundle| &**bundle)
}

/// [`installed_bundle`], to hold: what an instance of it runs.
#[must_use]
pub fn installed_bundle_shared() -> Option<Arc<Bundle>> {
    INSTALLED.get().map(Arc::clone)
}

/// The platform icon an `icon: .file_text` declaration names: `file-text`.
#[must_use]
pub fn icon_name(declared: &str) -> &'static str {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| declared.replace('_', "-"))
}

/// The app as every host mounts it: the installed package's ([`install`]), through the
/// framework's root like every native app.
pub fn definition() -> &'static ApplicationDefinition {
    // Receivers decide each change against the surface; the launcher names the package's own
    // file (`TILO_SURFACE`), which the host registers. Without a launcher (a test, a capture)
    // the surface is generated from the same metadata here, before the first signed operation.
    static SURFACE: OnceLock<()> = OnceLock::new();
    SURFACE.get_or_init(|| {
        if std::env::var_os(ui::app_surface::SURFACE_FILE_ENV).is_some() {
            return;
        }
        let Some(bundle) = installed_bundle() else {
            return;
        };
        match bundle.surface() {
            Ok(surface) => ui::app_surface::register_package(&bundle.meta.app, &surface),
            Err(error) => tracing::error!(%error, "the package's surface is unusable"),
        }
    });
    ui::framework::definition::<app::TlApp>()
}

/// How the runtime launches: the live window, or a capture of one of the package's scenarios.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    serve_remote_pictures();
    let _ = definition();
    ui::framework::launch_spec::<app::TlApp>()
}

/// Serve the pictures this window shows by address from the network its app may reach, kept
/// under the app's own cache: a map's tiles pan back without a fresh fetch, and what nothing
/// reaches any more shows the last time it was drawn. This process is the app's own, in the
/// app's sandbox, so what a request reaches is what the app declares and the workspace grants.
fn serve_remote_pictures() {
    let app = installed_bundle().map_or_else(String::new, |bundle| bundle.meta.app.clone());
    let name = app.rsplit('.').next().unwrap_or("app").to_owned();
    let Some(tiles) = tilo_tiles::Tiles::of_app(&name) else {
        return;
    };
    let fetch: ui::Fetcher = std::sync::Arc::new(move |url: &str| tiles.bytes(url));
    ui::set_remote_fetcher(Some(fetch));
}
