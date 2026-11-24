//! Interactive use of the existing screenshot fixture, never native controllers.
//!
//! Keep admission fail-closed: a newly added effect is not safe merely because
//! its button is hidden. Both queued controls and semantic invocations use this
//! list before entering the production action handler.

use super::InstallerPreviewEffect;
use ui::tilo_resource::Effect as _;

pub(super) fn admits(effect: &InstallerPreviewEffect) -> bool {
    admits_id(effect.id())
}

pub(super) fn admits_id(id: &str) -> bool {
    matches!(
        id,
        "read-preview"
            | "choose-preview"
            | "choose-and-continue-preview"
            | "set-migrate-ticket"
            | "set-preview-device-name"
            | "set-preview-password"
            | "set-preview-confirmation"
            | "toggle-storage-details"
            | "set-storage-size"
            | "continue-preview"
            | "back-in-preview"
            | "restart-preview"
            | "edit-preview"
    )
}
