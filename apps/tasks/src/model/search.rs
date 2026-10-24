//! What system search finds of the tracker: each issue, by its number
//! (`#12`, typed as it reads), its title, its description and its labels.

use ui::tilo_resource::{SearchEntry, SearchWeight};

use crate::model::record::Tracker;

/// One entry per issue, opening the issue.
#[must_use]
pub fn entries(tracker: &Tracker) -> Vec<SearchEntry> {
    tracker
        .issues
        .iter()
        .map(|(id, issue)| {
            let mut entry = SearchEntry::new(issue.title.clone(), issue.body.text())
                .anchored(crate::model::issue_resource(id))
                .subtitled(reference(issue.number))
                .with_icon(if issue.status.closed() {
                    "circle-check"
                } else {
                    "circle-dot"
                })
                .field(reference(issue.number), SearchWeight::Identity)
                .facet("status", "Status", status_name(issue.status));
            for label in issue.labels.iter() {
                entry = entry.field(label.clone(), SearchWeight::Metadata).facet(
                    "label",
                    "Label",
                    label.clone(),
                );
            }
            entry
        })
        .collect()
}

/// How an issue numbered `number` reads in a reference.
#[must_use]
pub fn reference(number: u64) -> String {
    format!("#{number}")
}

/// The number a reference as it reads (`#12`) names, the inverse of
/// [`reference()`].
#[must_use]
pub fn number(reference: &str) -> Option<u64> {
    reference.trim().strip_prefix('#')?.parse().ok()
}

fn status_name(status: crate::model::Status) -> &'static str {
    match status {
        crate::model::Status::Backlog => "Backlog",
        crate::model::Status::Todo => "Todo",
        crate::model::Status::InProgress => "In progress",
        crate::model::Status::InReview => "In review",
        crate::model::Status::Done => "Done",
        crate::model::Status::Cancelled => "Cancelled",
    }
}
