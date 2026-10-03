//! The runner this build links, selected by target. This module and the
//! target-scoped dependencies in `Cargo.toml` are the only platform selection
//! in the SDK facade; apps never name a runner.

#[cfg(all(feature = "native", not(target_family = "wasm")))]
pub use tilo_runner_native::{run, run_or_exit};

#[cfg(target_family = "wasm")]
pub use tilo_runner_web::{run, run_or_exit};

#[cfg(all(not(feature = "native"), not(target_family = "wasm")))]
mod without_runner {
    use ui_core::launch::{LaunchError, LaunchSpec};

    /// Built without the `native` feature: there is no runner to start.
    ///
    /// # Errors
    ///
    /// Always.
    pub fn run(spec: &LaunchSpec) -> Result<(), LaunchError> {
        Err(LaunchError::Operation(format!(
            "{} was built without a runner (enable `tilo-app/native`)",
            spec.app_id()
        )))
    }

    /// [`run`], printing the error and exiting with a failure status.
    pub fn run_or_exit(spec: &LaunchSpec) {
        if let Err(error) = run(spec) {
            tracing::error!("{error}");
            std::process::exit(2);
        }
    }
}

#[cfg(all(not(feature = "native"), not(target_family = "wasm")))]
pub use without_runner::{run, run_or_exit};
