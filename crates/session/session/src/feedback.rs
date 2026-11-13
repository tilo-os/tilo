//! Where a report about Tilo itself goes, and the report a window's box files
//! there.
//!
//! A report is what a window's send-to box holds when it was opened with one
//! place already on it ([`ui::widget::SendToBox`]): what the person wrote and
//! what the window picked, filed in the channel this computer's config names,
//! as the one action that channel takes reports with (`submit-feedback`).
//! That is the same report a browser's feedback sheet files, so the channel
//! reads, triages and answers a report from either the same way, and the
//! report is the person's own signed write where Tilo's feedback is read.
//!
//! This computer is told where that is beside its community site
//! ([`crate::platform::directory`]), and a computer told nothing offers no report: a
//! report with nowhere to go is not a report.

use serde::Deserialize;
use tilo_feedback::destination;
use tilo_feedback::report::MAX_PICKED_TEXT_CHARS;
use tilo_feedback::{
    Destination as FiledIn, Part, ReviewedFeedback, ReviewedSelection, ReviewedText, Submission,
    TextKind,
};
use tilo_reference::{Attached, ResourceAt};
use ui::floating_surface::{Recipient, RecipientKind};

/// The conversation a report goes to, and the workspace that keeps it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    /// The workspace the feedback conversation is in.
    pub workspace: String,
    /// The feedback conversation itself, by its id.
    pub channel: String,
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
            self.channel.clone(),
            tilo_desktop_surface::i18n::t("session-feedback-destination"),
        )
    }

    /// The channel's own item in the workspace's Chat, where its reports are filed.
    #[must_use]
    pub(crate) fn item(&self) -> String {
        destination::item(destination::CHANNELS, &self.channel)
    }

    /// The report one send files here: the words the person wrote and what
    /// their window picked, as the channel's review of a report reads them.
    ///
    /// # Errors
    /// What the destination refuses: no words and nothing picked, or more than
    /// a report carries.
    pub(crate) fn report(
        &self,
        app: &str,
        attached: &[Attached],
        text: &str,
    ) -> Result<Submission, String> {
        let submission = Submission {
            // One send is one report: the id is minted once, and a report sent again is another
            // report, never a second copy of this one.
            id: uuid::Uuid::new_v4().simple().to_string(),
            destination: FiledIn {
                room: self.workspace.clone(),
                // Reports are filed by Chat, the app that keeps the channel.
                provider: destination::PACKAGE.to_owned(),
                channel: self.channel.clone(),
            },
            feedback: ReviewedFeedback {
                text: text.to_owned(),
                selections: attached
                    .iter()
                    .map(|carried| picked(app, carried))
                    .collect(),
                ..ReviewedFeedback::default()
            },
        };
        submission.validate().map_err(|error| error.to_string())?;
        Ok(submission)
    }
}

/// What one thing a window carried is, as the destination reads what a report is about: the app and
/// record it came from, and the words as they read when it was sent.
fn picked(app: &str, carried: &Attached) -> ReviewedSelection {
    let (origin, label, words): (Option<&ResourceAt>, Option<&str>, Option<&str>) = match carried {
        Attached::Text { text, from } => (from.as_ref(), None, Some(text)),
        Attached::Resource { at, label, .. } => (Some(at), Some(label), None),
        Attached::Image { name, .. }
        | Attached::Video { name, .. }
        | Attached::File { name, .. } => (None, Some(name), None),
    };
    let source = origin.map_or(app, |at| at.app.as_str());
    ReviewedSelection {
        app: source.to_owned(),
        app_name: crate::app_names::app_name(source),
        build: None,
        path: origin.map_or_else(String::new, |at| {
            format!("{}/{}", at.reference.kind, at.reference.id)
        }),
        role: "text".to_owned(),
        kind: origin.map(|at| at.reference.kind.clone()),
        record: origin.and_then(|at| at.record.clone()),
        label: label.map(|label| quoted(TextKind::Interface, label)),
        texts: words
            .map(|words| vec![quoted(TextKind::Content, words)])
            .unwrap_or_default(),
        note: None,
        crop: None,
    }
}

/// `text` as it leaves the device: one run, nothing of it hidden, no longer than the pick it was.
fn quoted(kind: TextKind, text: &str) -> ReviewedText {
    ReviewedText {
        kind,
        parts: vec![Part {
            text: text.chars().take(MAX_PICKED_TEXT_CHARS).collect(),
            hidden: None,
        }],
    }
}
