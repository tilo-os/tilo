//! What the screenshot scenarios show: a few issues of a small launch, made
//! through the tracker's own actions. Never written into a real workspace.

use crate::model::{Status, Tracker, tracker};
use ui::prelude::*;

use crate::{Nav, Place};

pub(crate) const SCENARIOS: &[&str] = &["issues", "board", "issue"];

fn create(title: &str, status: Status, assignee: Option<&str>) -> Invoke<Tracker> {
    Tracker::CREATE.with(tracker::Create {
        title: title.to_owned(),
        parent: None,
        body: Some(format!("## Why\n\n{title} before the launch on Thursday.")),
        status: Some(status),
        assignee: assignee.map(str::to_owned),
    })
}

pub(crate) fn scenario(name: Option<&str>) -> Vec<Invoke<Tracker>> {
    if name.is_none() {
        return Vec::new();
    }
    vec![
        create(
            "Write the launch notes",
            Status::InProgress,
            Some("person:ada"),
        ),
        create("Record the two-minute demo", Status::Todo, None),
        create(
            "Tidy the onboarding docs",
            Status::Todo,
            Some("agent:claude"),
        ),
        create("Pick the venue", Status::Done, Some("person:grace")),
        create("Answer the press questions", Status::Backlog, None),
        create(
            "Check the slides in dark mode",
            Status::InReview,
            Some("person:ada"),
        ),
    ]
}

/// The view state a scenario shows.
pub(crate) fn stage(scenario: Option<&str>, nav: &Nav, tracker: &Tracker) {
    match scenario {
        Some("board") => nav.place.set(Place::Board),
        Some("issue") => {
            if let Some((id, _)) = tracker.by_number(1) {
                nav.open(id);
            }
        }
        _ => nav.place.set(Place::List),
    }
}
