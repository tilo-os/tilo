//! The runner this build links, selected by target. This module and the
//! target-scoped dependencies in `Cargo.toml` are the only platform selection
//! in the SDK facade; apps never name a runner.

/// Whether this build runs as a page: it has no event loop of its own and one
/// thread that runs the toolkit's background queue between frames, so work of
/// any size moves off the frame here and a hosted runner draws in the frame.
pub(crate) const IN_A_PAGE: bool = cfg!(target_family = "wasm");

#[cfg(all(feature = "native", not(target_family = "wasm")))]
pub use tilo_runner_native::{run, run_or_exit};

#[cfg(all(not(feature = "native"), not(target_family = "wasm")))]
mod without_runner {
    use ui_core::launch::{LaunchError, LaunchSpec};

    /// Built without the `native` feature: there is no runner to start.
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

/// The `render` operation: the app's answer to "show me this resource". A
/// wasm build publishes none, so it answers no request with one (see
/// [`rendered_anchor`]); the id is named on every target so the framework's
/// request path reads the same everywhere.
pub(crate) const RENDER_TYPE: &str = "tilo.app.render";

/// The `outline` operation: the app's answer to "what does this window
/// show", the view's semantic tree as lines an agent reads, and, asked for,
/// the same view as a picture.
pub(crate) const OUTLINE_TYPE: &str = "tilo.app.outline";

mod outline;

/// Every app's window answers what it shows ([`outline`]), on every host: a computer draws
/// the view as a picture when asked, a page reads it as lines.
pub(crate) fn with_view_operations(
    mut resource: tilo_actions::ResourceSpec,
) -> tilo_actions::ResourceSpec {
    let spec = outline::spec();
    if !resource
        .actions
        .iter()
        .any(|existing| existing.id == spec.id)
    {
        resource = resource.with_action(spec);
    }
    resource
}
#[cfg(not(target_family = "wasm"))]
mod presentation;
#[cfg(not(target_family = "wasm"))]
pub(crate) use presentation::{DrawPast, Presentation, operation_types, with_native_presentations};

/// The item a payload of [`OUTLINE_TYPE`] or [`RENDER_TYPE`] asks to be shown first, when
/// it asks for one: the app stages that item before it reads its view.
pub(crate) use outline::rendered_anchor;

/// The value's key explaining that this host cannot picture the view it just described: a
/// page draws none for a caller, so it says so rather than answering nothing.
#[cfg(target_family = "wasm")]
const PICTURE_UNAVAILABLE: &str = "picture_unavailable";

#[cfg(target_family = "wasm")]
#[derive(Default)]
pub(crate) struct Presentation {
    /// The theme the window draws in, which a line's look and the answer name.
    theme: Option<tilo_ui_base::Theme>,
}

#[cfg(target_family = "wasm")]
#[expect(
    clippy::unused_self,
    clippy::needless_pass_by_ref_mut,
    reason = "stand-ins for the native presentation's methods that a page has no use for"
)]
impl Presentation {
    pub(crate) fn new<R: tilo_resource::Resource>(_: &crate::framework::ViewCx<R>) -> Self {
        Self::default()
    }
    pub(crate) fn track_theme(&mut self, theme: &tilo_ui_base::Theme) {
        self.theme = Some(theme.clone());
    }
    pub(crate) fn needs_redraw(&self) -> bool {
        false
    }
    pub(crate) fn take_window_activation_intent(&self) -> bool {
        false
    }
    pub(crate) fn request_activation(&self) {}
    pub(crate) fn without_ground(&mut self) {}
    /// What this page's view shows, as lines: a page draws no picture of its own view for
    /// a caller, and says so where one was asked for rather than leaving it unexplained.
    pub(crate) fn start_operation(
        &mut self,
        op: &ui_core::host_operation::ResourceOperation<'_>,
        doc: &ui_core::automerge::AutoCommit,
        content: &mut dyn tilo_ui_base::widget::Widget,
    ) -> Result<Option<tilo_actions::ActionOutput>, String> {
        let request = op.request();
        if request.type_id != OUTLINE_TYPE {
            return Ok(None);
        }
        let (picture, _) = outline::asked(request.payload)?;
        let theme = self.theme.clone().unwrap_or_else(tilo_ui_base::Theme::dark);
        let mut output = outline::answer(&*content, doc, &theme);
        if picture
            && let Some(fields) = output
                .value
                .as_mut()
                .and_then(serde_json::Value::as_object_mut)
        {
            fields.insert(
                PICTURE_UNAVAILABLE.into(),
                "a page cannot picture its own view; the lines are what it shows".into(),
            );
        }
        Ok(Some(output))
    }
}

#[cfg(target_family = "wasm")]
pub(crate) fn operation_types() -> &'static [&'static str] {
    &[OUTLINE_TYPE]
}

#[cfg(target_family = "wasm")]
pub(crate) fn with_native_presentations(
    resource: tilo_actions::ResourceSpec,
) -> tilo_actions::ResourceSpec {
    resource
}
