//! How a continuation placeholder tab is titled.
//!
//! The names come from above the workspace model: the desktop knows which
//! apps are installed and where a file another device has open lives here.
//! The session hands its [`ContinuationLabels`] to whoever materializes a
//! continuation ([`super::WorkspaceTree::materialize_continuation_workspace`]),
//! so the model never asks the launcher or the document opener itself.

use crate::shell::personal_continuity::LaunchIntent;

/// The session's names for what another device has open.
pub trait ContinuationLabels {
    /// The display name of the installed app `app_id`, if the session knows
    /// it. Called on the compositor loop: answer from what was last
    /// enumerated, never by reading the app registry.
    fn app_name(&self, app_id: &str) -> Option<String>;

    /// The name a file reference's placeholder tab carries.
    fn file_reference_label(&self, reference: &LaunchIntent) -> String;
}

/// Labels for a session that resolves nothing: an app goes by its id, as an
/// app the session has not enumerated does, and a file by the generic name a
/// reference that resolves to no file gets.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnresolvedContinuationLabels;

impl ContinuationLabels for UnresolvedContinuationLabels {
    fn app_name(&self, _app_id: &str) -> Option<String> {
        None
    }

    fn file_reference_label(&self, _reference: &LaunchIntent) -> String {
        crate::shell::i18n::t("doc-open-unnamed-file")
    }
}
