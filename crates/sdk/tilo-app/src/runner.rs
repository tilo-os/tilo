//! The runner this build links, selected by target. This module and the
//! target-scoped dependencies in `Cargo.toml` are the only platform selection
//! in the SDK facade; apps never name a runner.

#[cfg(all(feature = "native", not(target_family = "wasm")))]
pub use tilo_runner_native::{run, run_or_exit};

#[cfg(target_family = "wasm")]
pub use web::{run, run_or_exit};

/// In the browser an app does not own an event loop: the page's runtime
/// (`tilo-browser-runtime`) mounts registered application definitions in its
/// desktop or as a single-app mount, so running an app registers it.
#[cfg(target_family = "wasm")]
mod web {
    use ui_core::application::registry;
    use ui_core::launch::{LaunchError, LaunchSpec};

    /// Register the app `spec` describes for the page runtime to mount.
    ///
    /// # Errors
    ///
    /// Never fails; the signature matches the native runner's.
    pub fn run(spec: &LaunchSpec) -> Result<(), LaunchError> {
        registry::register(spec.definition());
        Ok(())
    }

    /// [`run`]; registration cannot fail, so there is nothing to exit with.
    pub fn run_or_exit(spec: &LaunchSpec) {
        registry::register(spec.definition());
    }
}

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
    #[expect(
        clippy::exit,
        reason = "a missing runner is a terminal launch failure at the executable boundary"
    )]
    pub fn run_or_exit(spec: &LaunchSpec) {
        if let Err(error) = run(spec) {
            tracing::error!("{error}");
            std::process::exit(2);
        }
    }
}

#[cfg(all(not(feature = "native"), not(target_family = "wasm")))]
pub use without_runner::{run, run_or_exit};

#[cfg(not(target_family = "wasm"))]
pub(crate) const RENDER_TYPE: &str = "tilo.app.render";

#[cfg(not(target_family = "wasm"))]
mod presentation;
#[cfg(not(target_family = "wasm"))]
pub(crate) use presentation::{DrawPast, Presentation, operation_types, with_native_presentations};

#[cfg(target_family = "wasm")]
pub(crate) struct Presentation;

#[cfg(target_family = "wasm")]
#[expect(
    clippy::unused_self,
    clippy::needless_pass_by_ref_mut,
    clippy::unnecessary_wraps,
    reason = "a no-op stand-in with the native presentation's signatures"
)]
impl Presentation {
    pub(crate) fn new<R: tilo_resource::Resource>(_: &crate::framework::ViewCx<R>) -> Self {
        Self
    }
    pub(crate) fn track_theme(&mut self, _: &ui_core::Theme) {}
    pub(crate) fn needs_redraw(&self) -> bool {
        false
    }
    pub(crate) fn take_window_activation_intent(&self) -> bool {
        false
    }
    pub(crate) fn start_operation(
        &mut self,
        _: &ui_core::host_operation::ResourceOperation<'_>,
        _: &ui_core::automerge::AutoCommit,
        _: &mut dyn ui_core::widget::Widget,
    ) -> Result<Option<tilo_actions::ActionOutput>, String> {
        Ok(None)
    }
}

#[cfg(target_family = "wasm")]
pub(crate) fn operation_types() -> &'static [&'static str] {
    &[]
}

#[cfg(target_family = "wasm")]
pub(crate) fn with_native_presentations(
    resource: tilo_actions::Resource,
) -> tilo_actions::Resource {
    resource
}
