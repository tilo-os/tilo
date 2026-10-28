//! The Feedback composer's code: its application definition and how a
//! window of it is prepared over the profile's private outbox.
//!
//! Without `deferred-feedback-composer` it is linked in. With it, nothing
//! here names the composer: the bundling crate registers the composer with
//! `ui::application::registry` as an application fetched the first time it
//! opens, and that fetch hands its code over ([`install_feedback_composer`])
//! along with its definition. A first visit then downloads the composer only
//! when someone asks for Feedback.

use tilo_app::feedback_composer::FeedbackServices;
use ui::application::{AppComponent, ApplicationDefinition};
use ui::launch::LaunchError;

/// The composer's application id, under which a bundle registers it.
pub const FEEDBACK_COMPOSER: &str = tilo_desktop::feedback::PACKAGE;

/// The composer's code, as the bundle hands it over.
#[derive(Clone, Copy)]
pub struct FeedbackComposerCode {
    pub definition: fn() -> &'static ApplicationDefinition,
    /// A window of the composer: over the profile's private outbox, or, with
    /// no services, one that explains it cannot send.
    pub prepare: fn(Option<FeedbackServices>, &str, &str, u64) -> Result<AppComponent, LaunchError>,
}

#[must_use]
pub fn builtin_feedback_composer() -> FeedbackComposerCode {
    FeedbackComposerCode {
        definition: tilo_app::feedback_composer::definition,
        prepare: tilo_app::feedback_composer::prepare,
    }
}

#[cfg(not(feature = "deferred-feedback-composer"))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the deferred build has no code until it is fetched"
)]
pub(crate) fn code() -> Option<FeedbackComposerCode> {
    Some(builtin_feedback_composer())
}

#[cfg(feature = "deferred-feedback-composer")]
static CODE: std::sync::OnceLock<FeedbackComposerCode> = std::sync::OnceLock::new();

/// The composer's code, once fetched (`None` before).
#[cfg(feature = "deferred-feedback-composer")]
pub(crate) fn code() -> Option<FeedbackComposerCode> {
    CODE.get().copied()
}

/// The composer's code is in; returns its definition, for the application
/// registry's fetch to resolve with.
#[cfg(feature = "deferred-feedback-composer")]
pub fn install_feedback_composer(
    make: fn() -> FeedbackComposerCode,
) -> &'static ApplicationDefinition {
    (CODE.get_or_init(make).definition)()
}
