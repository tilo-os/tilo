//! What the screenshot scenarios show: a person's assistant in the middle of a working day,
//! as the resources it will be read from would hold it. Never written into a real workspace.

use ui::prelude::*;

use crate::panel::Tab;
use crate::sources::{Assistant, Event, Progress, Scheduled, Stage, Staged, SubAgent, Task};
use crate::{Nav, Place, Window};

pub(crate) const SCENARIOS: &[&str] = &[
    "empty",
    "main-chat",
    "sub-agent",
    "job",
    "scheduled",
    "resolved",
    "supervision",
];

const USAGE_DROP: &str = "usage-drop";

fn sub_agent(id: &str, job: &str, role: &str, progress: Progress) -> SubAgent {
    SubAgent {
        id: id.to_owned(),
        job: job.to_owned(),
        role: role.to_owned(),
        progress,
    }
}

fn task(id: &str, title: &str, note: Option<&str>, stage: Stage) -> Task {
    Task {
        id: id.to_owned(),
        title: title.to_owned(),
        note: note.map(str::to_owned),
        stage,
    }
}

fn assistant() -> Assistant {
    Assistant {
        id: "assistant".to_owned(),
        name: "Assistant".to_owned(),
        model: Some("google/gemini-3-pro".to_owned()),
        spent_micros: 3_120_000,
        limit_micros: Some(10_000_000),
    }
}

fn sub_agents() -> Vec<SubAgent> {
    vec![
        sub_agent(
            USAGE_DROP,
            "Usage drop: investigate",
            "Coder",
            Progress::Working,
        ),
        sub_agent(
            "board-deck",
            "Find the third-quarter board deck",
            "Researcher",
            Progress::NeedsYou,
        ),
        sub_agent(
            "competitors",
            "Competitors' posts this week",
            "Researcher",
            Progress::Working,
        ),
        sub_agent(
            "data-room",
            "Data room: open requests",
            "Researcher",
            Progress::Done,
        ),
    ]
}

fn tasks() -> Vec<Task> {
    vec![
        task(
            "reply",
            "Reply to Maya at Northwind",
            None,
            Stage::ReadyForReview,
        ),
        task(
            "deck",
            "Board deck for Friday: first draft",
            None,
            Stage::ReadyForReview,
        ),
        task(
            "find-deck",
            "Find the third-quarter board deck",
            Some("Two versions exist. Which one?"),
            Stage::NeedsInput,
        ),
        task(
            "usage",
            "Find out why usage dropped",
            None,
            Stage::InProgress,
        ),
        task(
            "posts",
            "Compare this week's competitor posts with last week's",
            None,
            Stage::InProgress,
        ),
        task(
            "churn",
            "Pull the churn numbers before the Atlas call",
            None,
            Stage::UpNext,
        ),
        task(
            "retry",
            "Check that billing retry is live",
            None,
            Stage::UpNext,
        ),
        task("invoice", "File October's invoices", None, Stage::Resolved),
    ]
}

fn scheduled() -> Vec<Scheduled> {
    vec![
        Scheduled {
            id: "call".to_owned(),
            name: "Remind: call Jonas".to_owned(),
            when: "Today at 10:14, once".to_owned(),
            on: true,
        },
        Scheduled {
            id: "rundown".to_owned(),
            name: "Morning rundown".to_owned(),
            when: "Every day at 7:30".to_owned(),
            on: true,
        },
    ]
}

fn events() -> Vec<Event> {
    vec![
        Event {
            id: "posts".to_owned(),
            name: "When a competitor posts".to_owned(),
            does: "Compare it with last week and tell me what changed.".to_owned(),
            on: true,
        },
        Event {
            id: "mails".to_owned(),
            name: "Every ten new mails".to_owned(),
            does: "One summary.".to_owned(),
            on: true,
        },
        Event {
            id: "dates".to_owned(),
            name: "When a mail contains a date".to_owned(),
            does: "Put it in my calendar.".to_owned(),
            on: false,
        },
    ]
}

fn staged() -> Staged {
    Staged {
        assistant: Some(assistant()),
        sub_agents: sub_agents(),
        tasks: tasks(),
        scheduled: scheduled(),
        events: events(),
        learned: vec![
            "You sign mails with Best, Xaver".to_owned(),
            "Atlas calls move to Thursdays".to_owned(),
            "Reports go into the Fundraising folder".to_owned(),
        ],
    }
}

/// What `fixture`'s scenario reads in place of the workspace's records: nothing for a
/// workspace nobody set an assistant up in.
pub(crate) fn fixture(fixture: &mut Fixture<Window>) {
    match fixture.scenario() {
        None | Some("empty") => {}
        Some(_) => fixture.provide(staged()),
    }
}

/// Show the place `scenario` is a picture of.
pub(crate) fn stage(scenario: Option<&str>, nav: &Nav) {
    match scenario {
        Some("sub-agent") => nav.show(&Place::SubAgent(USAGE_DROP.to_owned())),
        Some("job") => {
            nav.job
                .set("Find out why sign-ups dropped since Tuesday".to_owned());
            nav.ask_for_job();
        }
        Some("scheduled") => nav.tab.set(Tab::Scheduled),
        Some("resolved") => nav.tab.set(Tab::Resolved),
        _ => {}
    }
}
