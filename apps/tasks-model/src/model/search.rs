//! What system search finds of the tracker: each issue, by its number
//! (`#12`, typed as it reads), its title, its description and its labels.

use tilo_resource::{SearchEntry, SearchWeight};

use crate::model::issue::Issue;

/// One entry per issue, opening the issue.
#[must_use]
pub fn entries<'a>(
    issues: impl Iterator<Item = (&'a tilo_resource::Id, &'a Issue)>,
) -> Vec<SearchEntry> {
    issues
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
                .facet(
                    "status",
                    tilo_resource::word!("tasks-facet-status").read(),
                    status_name(issue.status),
                );
            for label in issue.labels.iter() {
                entry = entry.field(label.clone(), SearchWeight::Metadata).facet(
                    "label",
                    tilo_resource::word!("tasks-facet-label").read(),
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

fn status_name(status: crate::model::Status) -> String {
    match status {
        crate::model::Status::Backlog => tilo_resource::word!("tasks-status-backlog").read(),
        crate::model::Status::Todo => tilo_resource::word!("tasks-status-todo").read(),
        crate::model::Status::InProgress => tilo_resource::word!("tasks-status-in-progress").read(),
        crate::model::Status::InReview => tilo_resource::word!("tasks-status-in-review").read(),
        crate::model::Status::Done => tilo_resource::word!("tasks-status-done").read(),
        crate::model::Status::Cancelled => tilo_resource::word!("tasks-status-cancelled").read(),
    }
}
