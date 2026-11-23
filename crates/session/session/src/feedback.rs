//! Where a report about Tilo itself goes.
//!
//! A report is what a window's send-to box holds when it was opened with one place already on it
//! ([`ui::widget::SendToBox`]): what the person wrote and what the window picked, filed as Chat's
//! `report` in the workspace this computer's config names, which posts it in that workspace's
//! `#feedback` channel. The people of that workspace read, answer and react to it there as to any
//! message, and the report is the person's own signed write.
//!
//! This computer is told where that is beside its community site
//! ([`crate::platform::directory`]), and a computer told nothing offers no report: a
//! report with nowhere to go is not a report.

use serde::Deserialize;
use ui::floating_surface::{Recipient, RecipientKind};

/// The workspace a report goes to.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    /// The workspace whose `#feedback` channel keeps the reports.
    pub workspace: String,
}

impl Destination {
    /// The destination this computer is configured with, if any.
    #[must_use]
    pub(crate) fn configured() -> Option<Self> {
        crate::platform::directory::feedback()
    }

    /// The chip a report's box opens with: where the report goes, named so the person reads it
    /// before they write. The search never finds it and the box keeps it
    /// ([`RecipientKind::Feedback`]), so a report cannot be addressed elsewhere.
    #[must_use]
    pub(crate) fn recipient(&self) -> Recipient {
        Recipient::found(
            RecipientKind::Feedback,
            self.workspace.clone(),
            tilo_session_core::desktop::i18n::t("session-feedback-destination"),
        )
    }
}
