//! The web runner of the Tilo app SDK.
//!
//! In the browser an app does not own an event loop: the page's runtime
//! (`tilo-browser-runtime`) mounts registered application definitions in its
//! desktop or as a single-app mount. Running an app here therefore registers
//! its definition in [`ui_core::application::registry`] and returns.
//!
//! `tilo-app` depends on this crate only on wasm32. Apps do not depend on it.

use ui_core::application::registry;
use ui_core::launch::{LaunchError, LaunchSpec};

/// Register the app `spec` describes for the page runtime to mount.
///
/// # Errors
///
/// Never fails today; the signature matches the native runner's.
pub fn run(spec: &LaunchSpec) -> Result<(), LaunchError> {
    registry::register(spec.as_definition(spec.app_id()));
    Ok(())
}

/// [`run`]; registration cannot fail, so there is nothing to exit with.
pub fn run_or_exit(spec: &LaunchSpec) {
    let _ = run(spec);
}
