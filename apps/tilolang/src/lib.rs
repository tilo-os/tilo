//! tilolang: the runtime binary that runs one installed tilolang app.
//!
//! A tilolang package is an app of its own: its signed manifest names the tilolang runtime
//! bundle as its entrypoint (`[entrypoint] runtime = "tilolang/<version>"`), and the launcher
//! starts this binary from that bundle with `TILO_PACKAGE_ID` (the package's signed id) and
//! `TILO_APP_MODULE` (its payload: the compiled module, its metadata and its surface). The
//! binary builds the app's definition from them, so each package has its own id, window, record,
//! custody and permissions. It compiles nothing: the package was compiled by `tilolang build`.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "layout weights, counts and sizes are small values; see crates/platform/tilolang-tilo"
)]

pub mod app;
#[cfg(target_os = "linux")]
pub mod hosted;
pub mod i18n;

pub mod names;
#[cfg(test)]
pub mod packages;
pub mod resource;
pub mod updates;
pub mod window;

use std::sync::{Arc, OnceLock};

use tilolang_tilo::bundle::{Bundle, BundleError};
use ui::application::ApplicationDefinition;

/// The id the runtime answers to when no package is installed in it: only tests and captures of
/// the built-in packages run like that.
pub const UNBUNDLED_ID: &str = "so.tilo.lang";

/// The resource kind of the runtime's own record: the host package's, which every tilolang
/// surface publishes.
pub const RECORD_KIND: &str = tilolang_tilo::surface::HOST_KIND;

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
            #[cfg(test)]
            match packages::surface() {
                Ok(surface) => ui::app_surface::register_package(UNBUNDLED_ID, &surface),
                Err(error) => tracing::error!(error, "the built-in packages' surface is unusable"),
            }
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
    let _ = definition();
    ui::framework::launch_spec::<app::TlApp>()
}

/// The shared toolkit and `libstd`: a thin build links both dynamically so the window's process
/// can map a new build of this crate.
#[cfg(feature = "thin")]
extern crate runtime_dylib;

/// The symbols `tilolang dev` reloads the window's code through. The process that owns the
/// window keeps it and maps a new build of this library, which builds the app's root again over
/// the state the running code wrote.
#[cfg(feature = "thin")]
mod reload_in_place {
    /// Hand one window's state back to the process that owns it.
    ///
    /// # Safety
    /// `state` points at `state_len` readable bytes, `out` at `out_cap` writable ones, both
    /// live for this call.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn tilo_dev_apply(
        id: *const u8,
        id_len: usize,
        state: *const u8,
        state_len: usize,
        out: *mut u8,
        out_cap: usize,
    ) -> isize {
        let _ = (id, id_len);
        unsafe { ui::framework::dev_reload::copy_window_state(state, state_len, out, out_cap) }
    }

    /// The address of the toolkit this library linked, which the process must share.
    #[unsafe(no_mangle)]
    pub extern "C" fn tilo_dev_linked_ui() -> *const () {
        ui::framework::dev_reload::linked_ui()
    }

    /// Build the app's root again, for the process's window to take.
    #[unsafe(no_mangle)]
    pub extern "C" fn tilo_dev_publish_root() -> i32 {
        // This library holds its own copy of everything static in this crate: the package the
        // process runs, and the build it shows. Take both before the window asks for them.
        if let Err(error) = crate::install_from_env() {
            eprintln!("tilolang: the new runtime cannot run the package it was built for: {error}");
            return 0;
        }
        if let Some(bundle) = crate::installed_bundle() {
            // The window showed the latest of the package's builds: the new root shows that
            // one, and follows the ones after it.
            crate::updates::catch_up(&bundle.meta.app);
            crate::updates::start(&bundle.meta.app);
        }
        ui::framework::dev_reload::publish_root::<crate::app::TlApp>()
    }
}
