//! Reviewed feedback reports kept with the chat's resource and their triage: the
//! structured report a person filed, what happened to it since, and the message
//! id its channel shows it under. Reading one out as the message it shows, and
//! the receipt a server answers with, are the app's.

use serde::{Deserialize, Serialize};
use tilo_actions::{ActionError, Doc};
use tilo_resource::{self, Id, Resource as _};

use crate::chat::{Chat, FeedbackReport};

/// What a moderator decided about a report: where it stands, and what it names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum TriageUpdate {
    Investigating,
    Resolved,
    Reopen,
    Duplicate {
        report: String,
    },
    Task {
        reference: tilo_reference::ResourceRef,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriageEntry {
    pub author: tilo_resource::MemberId,
    pub ts: i64,
    pub update: TriageUpdate,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub submission: tilo_feedback::Submission,
    pub author: tilo_resource::MemberId,
    pub ts: i64,
    #[serde(default)]
    pub triage: Vec<TriageEntry>,
}

fn refused(reason: impl Into<String>) -> ActionError {
    ActionError::Refused(reason.into())
}

pub const REPLAY_ACTION: &str = "replay-recording";

/// The id of the message a report is posted as.
#[must_use]
pub fn message_id(report: &str) -> String {
    format!("m_feedback_{report}")
}

/// The report whose message `message` is ([`message_id`]).
#[must_use]
pub fn report_of_message(message: &str) -> Option<&str> {
    message
        .strip_prefix("m_feedback_")
        .filter(|report| !report.is_empty())
}

/// The report `id` as the resource keeps it, its triage oldest first.
fn report_of(id: &Id, report: &FeedbackReport) -> Result<Report, ActionError> {
    let submission = report
        .submission
        .clone()
        .ok_or_else(|| refused("invalid feedback report"))?;
    submission
        .validate()
        .map_err(|error| refused(error.to_string()))?;
    if submission.id != id.as_str() {
        return Err(refused("feedback id conflict"));
    }
    let mut triage: Vec<TriageEntry> = report
        .triage
        .values()
        .filter_map(|entry| {
            Some(TriageEntry {
                author: entry.author.clone(),
                ts: entry.ts,
                update: entry.update.clone()?,
            })
        })
        .collect();
    triage.sort_by_key(|entry| {
        (
            entry.ts,
            entry.author.clone(),
            serde_json::to_string(&entry.update).unwrap_or_default(),
        )
    });
    Ok(Report {
        submission,
        author: report.author.clone(),
        ts: report.ts,
        triage,
    })
}

/// The report `id` filed in the chat `doc` holds.
pub fn report(doc: &Doc, id: &str) -> Result<Option<Report>, ActionError> {
    let chat = Chat::read(doc).unwrap_or_default();
    let id = Id::from_raw(id);
    chat.feedback
        .get(&id)
        .map(|report| report_of(&id, report))
        .transpose()
}

/// Every report filed in the chat `doc` holds.
pub fn reports(doc: &Doc) -> Result<Vec<Report>, ActionError> {
    let chat = Chat::read(doc).unwrap_or_default();
    chat.feedback
        .iter()
        .map(|(id, report)| report_of(id, report))
        .collect()
}
