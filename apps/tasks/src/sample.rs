//! What the capture scenarios show: a few issues of a small launch. Never
//! written into a real workspace.
//!
//! The issues are made through the tracker's own actions — a [`Replica`]
//! applies each one through the same door a person's does — and then staged
//! as the record's start, since a capture cannot wait for a handful of
//! changes, and the issues it shows must be the person's own for "Mine" and
//! its count to read as they do.

use ui::prelude::*;
use ui::tilo_resource::NonBlank;

use crate::model::{AgentRequest, Question, RunPhase, Status, StepSpend, Tracker, tracker};
use crate::{Filter, Nav, Place};

pub(crate) const SCENARIOS: &[&str] =
    &["issues", "board", "issue", "mine", "thread", "todo", "chat"];

/// One issue of the scene: its title, where it stands, whose it is, and its
/// label.
struct Sample {
    title: &'static str,
    status: Status,
    /// `true` when it is the person's own.
    mine: bool,
    /// The room's agent.
    agent: bool,
    label: Option<&'static str>,
}

const ISSUES: [Sample; 6] = [
    Sample {
        title: "Write the launch notes",
        status: Status::InProgress,
        mine: true,
        agent: false,
        label: Some("launch"),
    },
    Sample {
        title: "Record the two-minute demo",
        status: Status::Todo,
        mine: true,
        agent: false,
        label: Some("launch"),
    },
    Sample {
        title: "Tidy the onboarding docs",
        status: Status::Todo,
        mine: false,
        agent: true,
        label: None,
    },
    Sample {
        title: "Pick the venue",
        status: Status::Done,
        mine: false,
        agent: false,
        label: Some("venue"),
    },
    Sample {
        title: "Answer the press questions",
        status: Status::Backlog,
        mine: false,
        agent: false,
        label: None,
    },
    Sample {
        title: "Check the slides in dark mode",
        status: Status::InReview,
        mine: true,
        agent: false,
        label: Some("venue"),
    },
];

/// The tracker a scenario shows, its issues the person's own changes.
pub(crate) fn tracker(person: &str) -> Tracker {
    let mut replica = Replica::<Tracker>::new();
    let mut at = ui::time::unix_now_ms_i64().saturating_sub(60_000);
    for issue in &ISSUES {
        at = at.saturating_add(1_000);
        let assignee = if issue.mine {
            Some(crate::model::person_assignee(person).0)
        } else if issue.agent {
            Some(crate::model::agent_assignee("claude").0)
        } else {
            None
        };
        let Some(title) = NonBlank::parse(issue.title) else {
            continue;
        };
        replica = replica.as_author(person);
        replica.set_now_ms(at);
        if replica
            .apply(Tracker::CREATE.with(tracker::Create {
                title,
                parent: None,
                body: Some(format!("## Why\n\n{} before the launch.", issue.title)),
                status: Some(issue.status),
                assignee,
            }))
            .is_err()
        {
            continue;
        }
        // Some of the tasks have been talked about, so the thread view has
        // something to read and the person's own voice is in it.
        if matches!(issue.status, Status::InProgress | Status::InReview)
            && let Some(id) = made(&replica, issue.title)
            && let Some(text) = NonBlank::parse(&format!(
                "Started on {}: the first draft is in the folder.",
                issue.title
            ))
        {
            at = at.saturating_add(1_000);
            replica = replica.as_author(person);
            replica.set_now_ms(at);
            replica
                .apply(Tracker::COMMENT.with(tracker::Comment {
                    issue: id.clone(),
                    text,
                }))
                .ok();
        }
        let Some(label) = issue.label.and_then(NonBlank::parse) else {
            continue;
        };
        let Some(id) = made(&replica, issue.title) else {
            continue;
        };
        at = at.saturating_add(1_000);
        replica.set_now_ms(at);
        let _ = replica.apply(Tracker::ADD_LABEL.with(tracker::AddLabel { issue: id, label }));
    }
    chat(&mut replica, person, at);
    replica.state().unwrap_or_default()
}

/// The issue the chat scenario shows: an agent going through three job
/// applications, telling its progress per application, the person steering
/// it on the way, and the run waiting for the person's answer before it
/// sends a message.
const CHAT_ISSUE: &str = "chat-applications";
/// The agent the chat is with, as the room names it.
const CHAT_AGENT: &str = "agent:Claude";

/// One step the agent spent, on a hosted model.
fn step(step: u64, cost_micros: u64) -> StepSpend {
    StepSpend {
        step,
        model: "Claude Sonnet 4.6".into(),
        device: "Ada's laptop".into(),
        input_tokens: 5_200,
        output_tokens: 900,
        priced_calls: 0,
        cost_micros,
        currency: "EUR".into(),
    }
}

/// The chat's conversation, made through the tracker's own actions: the
/// person's on `person`'s replica, the agent's on a replica of the agent
/// working for them.
fn chat(replica: &mut Replica<Tracker>, person: &str, mut at: i64) {
    let issue = Id::from_raw(CHAT_ISSUE);
    replica.set_now_ms(at);
    let asked = replica.apply(Tracker::ASK_AGENT.with(tracker::AskAgent {
        ask: AgentRequest {
            id: issue.clone(),
            title: "Review the three job applications in my inbox".into(),
            body: Some(
                "Review the three applications for the frontend role in my inbox: read each CV \
                 and portfolio, and tell me who to invite."
                    .into(),
            ),
            agent: CHAT_AGENT.into(),
            ..AgentRequest::default()
        },
    }));
    if asked.is_err() {
        return;
    }
    let mut agent = replica.fork().as_agent_for(CHAT_AGENT, person);
    let said = |agent: &mut Replica<Tracker>, at: &mut i64, text: &str| {
        *at = at.saturating_add(20_000);
        agent.set_now_ms(*at);
        if let Some(text) = NonBlank::parse(text) {
            let _ = agent.apply(Tracker::REPORT_RUN.with(tracker::ReportRun {
                issue: issue.clone(),
                phase: RunPhase::Executing,
                text: Some(text),
            }));
        }
    };
    said(
        &mut agent,
        &mut at,
        "Found 3 applications for the frontend role: Jane Doe, Omar Haddad and Lena Fischer.",
    );
    at = at.saturating_add(5_000);
    agent.set_now_ms(at);
    let _ = agent.apply(Tracker::RECORD_READ.with(tracker::RecordRead {
        issue: issue.clone(),
        resource: ui::tilo_reference::ResourceAt {
            app: "so.tilo.files".into(),
            record: None,
            reference: ui::tilo_reference::ResourceRef::whole(
                "files.entry",
                "Documents/Applications/Jane Doe CV.pdf",
                "Jane Doe CV.pdf",
            ),
        },
    }));
    let _ = agent.apply(Tracker::SETTLE_SPEND.with(tracker::SettleSpend {
        issue: issue.clone(),
        step: step(1, 31_000),
    }));
    said(
        &mut agent,
        &mut at,
        "1 of 3: reviewed Jane Doe. Six years of React, led the accessibility work on a \
         shipped design system; her portfolio shows two products in use.",
    );
    // The person steers it while it works; it takes the message in.
    let _ = replica.sync_with(&mut agent);
    at = at.saturating_add(15_000);
    replica.set_now_ms(at);
    if let Some(text) = NonBlank::parse(
        "Invite Jane for an interview next week. Only draft replies to the others for now.",
    ) {
        let _ = replica.apply(Tracker::TELL_AGENT.with(tracker::TellAgent {
            issue: issue.clone(),
            text,
        }));
    }
    let _ = agent.sync_with(replica);
    let _ = agent.apply(Tracker::INCORPORATE.with(tracker::Incorporate {
        issue: issue.clone(),
        through: 1,
    }));
    said(
        &mut agent,
        &mut at,
        "2 of 3: reviewed Omar Haddad. Strong on performance work, little product design; \
         drafted a reply that keeps him in mind for the platform role.",
    );
    let _ = agent.apply(Tracker::SETTLE_SPEND.with(tracker::SettleSpend {
        issue: issue.clone(),
        step: step(2, 27_000),
    }));
    said(
        &mut agent,
        &mut at,
        "3 of 3: reviewed Lena Fischer. Recent graduate with a careful portfolio; drafted a \
         friendly no with feedback on her case study.",
    );
    ask_to_send(&mut agent, &issue, at.saturating_add(10_000));
    let _ = replica.sync_with(&mut agent);
}

/// The run asks before it sends the invitation: an action that asks first.
fn ask_to_send(agent: &mut Replica<Tracker>, issue: &Id, at: i64) {
    agent.set_now_ms(at);
    let _ = agent.apply(Tracker::ASK_CONFIRMATION.with(tracker::AskConfirmation {
        issue: issue.clone(),
        question: Question {
            id: "invite-jane".into(),
            app: "so.tilo.mail".into(),
            action: "send".into(),
            resource: "mail.account".into(),
            arguments_digest: "sample".into(),
            draft: ui::serde_json::json!({
                "action_label": "Send message",
                "resource_label": "Mail",
                "fields": [],
            }),
            preview: None,
        },
    }));
}

/// The issue `title` names, as the replica has it now.
fn made(replica: &Replica<Tracker>, title: &str) -> Option<Id> {
    replica.state().ok().and_then(|state| {
        state
            .issues
            .iter()
            .find(|(_, shown)| shown.title == title)
            .map(|(id, _)| id.clone())
    })
}

/// The view state a scenario shows.
pub(crate) fn stage(scenario: Option<&str>, nav: &Nav, tracker: &Tracker) {
    match scenario {
        Some("board") => nav.place.set(Place::Board),
        Some("mine") => {
            nav.place.set(Place::List);
            nav.filter.set(Some(Filter::Mine.key()));
        }
        Some("issue") => {
            nav.place.set(Place::List);
            if let Some((id, _)) = tracker.by_number(1) {
                nav.open(id);
            }
        }
        Some("thread") => nav.place.set(Place::Thread),
        Some("chat") => nav.chat.set(Some(Id::from_raw(CHAT_ISSUE))),
        Some("todo") => {
            nav.place.set(Place::List);
            nav.filter.set(Some(Filter::Todo.key()));
        }
        _ => nav.place.set(Place::List),
    }
}
