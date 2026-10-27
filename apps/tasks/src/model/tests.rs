#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion"
)]

use ui::tilo_resource::{Id, Replica};

use crate::model::{
    Imported, ImportedMessage, MessageKind, Question, RunOutcome, RunPhase, Status, Tracker,
    agent_assignee, tracker,
};

const AGENT: &str = "agent:claude";

fn create(title: &str, parent: Option<Id>, assignee: Option<&str>) -> tracker::Create {
    tracker::Create {
        title: title.to_owned(),
        parent,
        body: None,
        status: None,
        assignee: assignee.map(str::to_owned),
    }
}

fn ada() -> Replica<Tracker> {
    let mut replica = Replica::<Tracker>::new().as_author("ada");
    replica.set_now_ms(1_000);
    replica
}

fn id_of(replica: &Replica<Tracker>, title: &str) -> Id {
    replica
        .state()
        .unwrap()
        .issues
        .iter()
        .find(|(_, issue)| issue.title == title)
        .map(|(id, _)| id.clone())
        .expect("the issue is there")
}

#[test]
fn issues_are_numbered_nested_and_never_under_themselves() {
    let mut replica = ada();
    replica
        .apply(Tracker::CREATE.with(create("Ship it", None, None)))
        .unwrap();
    let parent = id_of(&replica, "Ship it");
    replica
        .apply(Tracker::CREATE.with(create("Write notes", Some(parent.clone()), None)))
        .unwrap();
    let child = id_of(&replica, "Write notes");
    let state = replica.state().unwrap();
    assert_eq!(state.issues.get(&parent).unwrap().number, 1);
    assert_eq!(state.issues.get(&child).unwrap().number, 2);
    assert_eq!(state.issues.get(&child).unwrap().author, "ada");
    assert_eq!(state.progress(&parent), (0, 1));
    assert!(
        replica
            .apply(Tracker::MOVE_UNDER.with(tracker::MoveUnder {
                issue: parent.clone(),
                parent: Some(child.clone()),
            }))
            .is_err(),
        "a parent cannot move under its own sub-issue"
    );
    assert!(
        replica
            .apply(Tracker::CREATE.with(create("  ", None, None)))
            .is_err(),
        "a blank title is refused"
    );
    // Deleting the parent lifts its sub-issue to the top level.
    replica
        .apply_confirmed(Tracker::DELETE.with(tracker::Delete { issue: parent }))
        .unwrap();
    assert_eq!(
        replica.state().unwrap().issues.get(&child).unwrap().parent,
        None
    );
}

#[test]
fn numbers_given_after_a_merge_are_past_both_sides() {
    let mut left = ada();
    let mut right = left.fork().as_author("grace");
    left.apply(Tracker::CREATE.with(create("Left", None, None)))
        .unwrap();
    right
        .apply(Tracker::CREATE.with(create("Right", None, None)))
        .unwrap();
    left.sync_with(&mut right).unwrap();
    left.apply(Tracker::CREATE.with(create("After", None, None)))
        .unwrap();
    let after = id_of(&left, "After");
    assert_eq!(left.state().unwrap().issues.get(&after).unwrap().number, 3);
}

#[test]
fn placing_on_the_board_sets_status_and_order_in_one_step() {
    let mut replica = ada();
    for title in ["One", "Two", "Three"] {
        replica
            .apply(Tracker::CREATE.with(create(title, None, None)))
            .unwrap();
    }
    let one = id_of(&replica, "One");
    let three = id_of(&replica, "Three");
    replica
        .apply(Tracker::PLACE.with(tracker::Place {
            issue: three.clone(),
            status: Status::Todo,
            before: Some(one),
        }))
        .unwrap();
    let state = replica.state().unwrap();
    let order: Vec<&str> = state
        .issues
        .values()
        .map(|issue| issue.title.as_str())
        .collect();
    assert_eq!(order, ["Three", "One", "Two"]);
    assert_eq!(state.issues.get(&three).unwrap().status, Status::Todo);
    // Down past where it was: after Two, which was third.
    replica
        .apply(Tracker::PLACE.with(tracker::Place {
            issue: three.clone(),
            status: Status::Done,
            before: None,
        }))
        .unwrap();
    let state = replica.state().unwrap();
    assert_eq!(state.issues.at(2).unwrap().0, &three);
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one run from approval to review, step by step"
)]
fn an_agents_run_is_approved_by_a_person_reported_by_its_agent_and_steered_in_the_thread() {
    let mut person = ada();
    person
        .apply(Tracker::CREATE.with(create("Tidy the docs", None, Some(AGENT))))
        .unwrap();
    let issue = id_of(&person, "Tidy the docs");
    let mut agent = person.fork().as_agent_for(AGENT, "ada");
    let mut other = person.fork().as_agent_for("agent:other", "ada");

    // The agent cannot approve itself.
    assert!(
        agent
            .apply(Tracker::APPROVE_RUN.with(tracker::ApproveRun {
                issue: issue.clone()
            }))
            .is_err()
    );
    person
        .apply(Tracker::APPROVE_RUN.with(tracker::ApproveRun {
            issue: issue.clone(),
        }))
        .unwrap();
    let state = person.state().unwrap();
    let run = state.issues.get(&issue).unwrap().run.clone().unwrap();
    assert_eq!((run.epoch, run.starter.as_str()), (1, "ada"));
    assert_eq!(state.issues.get(&issue).unwrap().status, Status::InProgress);

    // A message to the run, which only its own agent takes in.
    person
        .apply(Tracker::SEND_MESSAGE.with(tracker::SendMessage {
            issue: issue.clone(),
            text: "Keep the headings".into(),
        }))
        .unwrap();
    agent.sync_with(&mut person).unwrap();
    other.sync_with(&mut person).unwrap();
    let pending: Vec<Id> = agent
        .state()
        .unwrap()
        .issues
        .get(&issue)
        .unwrap()
        .messages_for_run(1)
        .map(|(id, _)| id.clone())
        .collect();
    assert_eq!(pending.len(), 1);
    let report = |phase| {
        Tracker::REPORT_RUN.with(tracker::ReportRun {
            issue: issue.clone(),
            phase,
            text: Some("Read the index".into()),
        })
    };
    assert!(
        other.apply(report(RunPhase::Planning)).is_err(),
        "another agent cannot report on this run"
    );
    agent.apply(report(RunPhase::Planning)).unwrap();
    agent
        .apply(Tracker::INCORPORATE.with(tracker::Incorporate {
            issue: issue.clone(),
            through: u64::try_from(pending.len()).unwrap(),
        }))
        .unwrap();

    // It asks first; the person confirms; it finishes into review.
    agent
        .apply(Tracker::ASK_CONFIRMATION.with(tracker::AskConfirmation {
            issue: issue.clone(),
            question: Question {
                id: "q1".into(),
                app: "so.tilo.mail".into(),
                action: "send".into(),
                resource: "mail.account".into(),
                arguments_digest: "digest".into(),
                draft: ui::serde_json::json!({ "to": "team" }),
                preview: None,
            },
        }))
        .unwrap();
    person.sync_with(&mut agent).unwrap();
    assert!(
        agent
            .apply(
                Tracker::ANSWER_CONFIRMATION.with(tracker::AnswerConfirmation {
                    issue: issue.clone(),
                    confirm: true,
                    edits: None,
                    reason: None,
                })
            )
            .is_err(),
        "an agent never confirms for itself"
    );
    person
        .apply(
            Tracker::ANSWER_CONFIRMATION.with(tracker::AnswerConfirmation {
                issue: issue.clone(),
                confirm: true,
                edits: None,
                reason: None,
            }),
        )
        .unwrap();
    agent.sync_with(&mut person).unwrap();
    agent
        .apply(
            Tracker::SETTLE_CONFIRMATION.with(tracker::SettleConfirmation {
                issue: issue.clone(),
                id: "q1".into(),
            }),
        )
        .unwrap();
    agent
        .apply(Tracker::FINISH_RUN.with(tracker::FinishRun {
            issue: issue.clone(),
            outcome: RunOutcome::Done,
            summary: Some("Tidied".into()),
        }))
        .unwrap();
    let state = agent.state().unwrap();
    let done = state.issues.get(&issue).unwrap();
    assert_eq!(done.status, Status::InReview);
    assert_eq!(done.run.as_ref().unwrap().phase, RunPhase::Done);
    assert!(done.messages_for_run(1).next().is_none());
    let from_run = done
        .comments
        .values()
        .filter(|message| message.kind == MessageKind::FromRun)
        .count();
    assert_eq!(from_run, 2);
    // Reports on an ended run are refused.
    assert!(agent.apply(report(RunPhase::Executing)).is_err());
}

#[test]
fn a_budget_is_set_only_between_runs_and_within_its_ceilings() {
    let mut replica = ada();
    replica
        .apply(Tracker::CREATE.with(create("Budgeted", None, Some(AGENT))))
        .unwrap();
    let issue = id_of(&replica, "Budgeted");
    let budget = |turn_ms, total_ms| {
        Tracker::SET_EXECUTION_BUDGET.with(tracker::SetExecutionBudget {
            issue: issue.clone(),
            turn_ms,
            total_ms,
        })
    };
    assert!(replica.apply(budget(Some(10), Some(5))).is_err());
    assert!(
        replica
            .apply(budget(Some(1_000_000), Some(2_000_000)))
            .is_err()
    );
    replica.apply(budget(Some(1_000), Some(60_000))).unwrap();
    replica
        .apply(Tracker::APPROVE_RUN.with(tracker::ApproveRun {
            issue: issue.clone(),
        }))
        .unwrap();
    assert!(replica.apply(budget(None, None)).is_err());
    assert!(
        replica
            .apply(Tracker::ASSIGN.with(tracker::Assign {
                issue: issue.clone(),
                assignee: Some("agent:other".into()),
            }))
            .is_err(),
        "an agent is not swapped under its own run"
    );
}

#[test]
fn imported_tasks_are_one_issue_whoever_imports_them_and_never_come_back() {
    let task = Imported {
        task: "t1".into(),
        title: "Old task".into(),
        instruction: "Do the old thing".into(),
        status: Status::Todo,
        assignee: Some(agent_assignee("claude").0),
        author: "ada".into(),
        created_ms: 5,
        messages: vec![ImportedMessage {
            author: AGENT.into(),
            by_agent: true,
            at_ms: 6,
            text: "Started".into(),
        }],
    };
    let import = || {
        Tracker::IMPORT.with(tracker::Import {
            tasks: vec![task.clone()],
        })
    };
    let mut left = ada();
    let mut right = left.fork().as_author("grace");
    left.apply(import()).unwrap();
    right.apply(import()).unwrap();
    left.sync_with(&mut right).unwrap();
    let state = left.state().unwrap();
    assert_eq!(state.issues.len(), 1);
    let issue = state.issues.get(&task.issue_id()).unwrap();
    assert_eq!(issue.assigned_agent(), Some(AGENT));
    assert!(issue.instruction().contains("Do the old thing"));
    assert_eq!(issue.comments.len(), 1);
    left.apply_confirmed(Tracker::DELETE.with(tracker::Delete {
        issue: task.issue_id(),
    }))
    .unwrap();
    left.apply(import()).unwrap();
    assert!(left.state().unwrap().issues.is_empty());
}

#[test]
fn asking_an_agent_makes_an_issue_whose_run_the_asking_approved() {
    let mut person = ada();
    let id = Id::from_raw("asked");
    let ask = || {
        Tracker::ASK_AGENT.with(tracker::AskAgent {
            ask: crate::model::AgentRequest {
                id: Id::from_raw("asked"),
                title: "Summarize the thread".into(),
                body: None,
                agent: "claude".into(),
                model: Some(crate::model::ModelChoice {
                    profile: "work".into(),
                    model: "fast".into(),
                }),
                subject: None,
            },
        })
    };
    person.apply(ask()).unwrap();
    let state = person.state().unwrap();
    let issue = state.issues.get(&id).unwrap();
    let run = issue.active_run().unwrap();
    assert_eq!((run.agent.0.as_str(), run.starter.as_str()), (AGENT, "ada"));
    assert_eq!(run.model.as_ref().unwrap().model, "fast");
    assert_eq!(issue.status, Status::InProgress);
    assert!(person.apply(ask()).is_err(), "an id is asked once");
    let mut agent = person.fork().as_agent_for(AGENT, "ada");
    assert!(
        agent.apply(ask()).is_err(),
        "an agent asks nothing of itself"
    );
    // The model is not changed under the run.
    assert!(
        person
            .apply(Tracker::CHOOSE_MODEL.with(tracker::ChooseModel {
                issue: id,
                model: None
            }))
            .is_err()
    );
}

#[test]
fn an_issue_reads_its_goal_its_urgency_and_its_cost_and_its_agent_records_what_it_used() {
    use crate::model::Area;

    let mut person = ada();
    person
        .apply(Tracker::CREATE.with(create("Quarterly numbers", None, Some(AGENT))))
        .unwrap();
    let issue = id_of(&person, "Quarterly numbers");
    let goal = Area {
        resource: "notes/finance/q3".into(),
        app: "so.tilo.notes".into(),
        kind: "notes.note".into(),
        label: "Q3 financials".into(),
    };
    person
        .apply(Tracker::PLAN.with(tracker::Plan {
            issue: issue.clone(),
            area: Some(goal.clone()),
            priority: Some(2),
            cost_budget: Some(5_000),
            due_ms: Some(1_800_000_000_000),
        }))
        .unwrap();
    let planned = person.state().unwrap();
    let read = planned.issues.get(&issue).unwrap();
    assert_eq!(read.area.as_ref(), Some(&goal));
    assert_eq!(read.priority, Some(2));
    assert_eq!(read.cost_budget, Some(5_000));
    assert_eq!(read.due_ms, Some(1_800_000_000_000));

    // Planning is a person's: an agent does not set its own budget.
    let mut agent = person.fork().as_agent_for(AGENT, "ada");
    assert!(
        agent
            .apply(Tracker::PLAN.with(tracker::Plan {
                issue: issue.clone(),
                area: None,
                priority: Some(1),
                cost_budget: Some(1_000_000),
                due_ms: None,
            }))
            .is_err(),
        "only a person plans"
    );
    // Nor is an urgency outside 1 to 4, or a goal that names no resource.
    for wrong in [
        tracker::Plan {
            issue: issue.clone(),
            area: None,
            priority: Some(5),
            cost_budget: None,
            due_ms: None,
        },
        tracker::Plan {
            issue: issue.clone(),
            area: None,
            priority: None,
            cost_budget: Some(0),
            due_ms: None,
        },
        tracker::Plan {
            issue: issue.clone(),
            area: Some(Area::default()),
            priority: None,
            cost_budget: None,
            due_ms: None,
        },
    ] {
        assert!(person.apply(Tracker::PLAN.with(wrong)).is_err());
    }

    // The run's agent says where it runs and what it used; nobody else does.
    person
        .apply(Tracker::APPROVE_RUN.with(tracker::ApproveRun {
            issue: issue.clone(),
        }))
        .unwrap();
    let mut agent = person.fork().as_agent_for(AGENT, "ada");
    let mut other = agent.fork().as_agent_for("agent:other", "ada");
    let on_steam_deck = || {
        Tracker::RUN_ON_DEVICE.with(tracker::RunOnDevice {
            issue: issue.clone(),
            device: "Ada's laptop".into(),
        })
    };
    assert!(other.apply(on_steam_deck()).is_err());
    agent.apply(on_steam_deck()).unwrap();
    agent
        .apply(Tracker::RECORD_SPEND.with(tracker::RecordSpend {
            issue: issue.clone(),
            spent: 1_200,
        }))
        .unwrap();
    person.sync_with(&mut agent).unwrap();
    let started = person.state().unwrap();
    let run = started.issues.get(&issue).unwrap();
    assert_eq!(
        run.run.as_ref().unwrap().device.as_deref(),
        Some("Ada's laptop")
    );
    assert_eq!(run.spend, Some(1_200));
    assert_eq!(run.cost_budget, Some(5_000));
}
