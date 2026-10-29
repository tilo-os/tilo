#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion"
)]

use ui::tilo_resource::{Id, NonBlank, Replica};

use crate::model::{
    Imported, ImportedMessage, MessageKind, Question, Run, RunOutcome, RunPhase, SpendLimits,
    Status, StepSpend, Tracker, agent_assignee, tracker,
};

const AGENT: &str = "agent:claude";

/// A settled step receipt for `cents` micro-units on the default model, as a
/// provider reports one.
fn priced_step(step: u64, cost_micros: u64) -> StepSpend {
    StepSpend {
        step,
        model: "claude-sonnet-4-6".into(),
        device: "Ada's laptop".into(),
        input_tokens: 800,
        output_tokens: 400,
        priced_calls: 0,
        cost_micros,
        currency: "EUR".into(),
    }
}

/// `text` as the non-blank text an action takes.
fn nb(text: &str) -> NonBlank {
    NonBlank::parse(text).expect("the test's text says something")
}

fn create(title: &str, parent: Option<Id>, assignee: Option<&str>) -> tracker::Create {
    tracker::Create {
        title: nb(title),
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
        serde_json::from_value::<tracker::Create>(serde_json::json!({"title": "  "})).is_err(),
        "a blank title is refused where the arguments are read"
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
            text: nb("Keep the headings"),
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
            text: Some(nb("Read the index")),
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
            summary: Some(nb("Tidied")),
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
            limits: None,
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
fn a_task_is_a_shared_thread_two_people_take_part_in() {
    let mut ada = ada().as_invoker("ada");
    ada.apply(Tracker::CREATE.with(create("Plan the offsite", None, Some(AGENT))))
        .unwrap();
    let issue = id_of(&ada, "Plan the offsite");
    let mut bob = ada.fork().as_invoker("bob");
    // The task has no run yet, so a message joins the thread instead of
    // failing for want of an agent to read it.
    ada.apply(Tracker::SEND_MESSAGE.with(tracker::SendMessage {
        issue: issue.clone(),
        text: nb("Book the room"),
    }))
    .unwrap();
    bob.sync_with(&mut ada).unwrap();
    bob.apply(Tracker::SEND_MESSAGE.with(tracker::SendMessage {
        issue: issue.clone(),
        text: nb("I will write the agenda"),
    }))
    .unwrap();
    ada.sync_with(&mut bob).unwrap();
    for replica in [&ada, &bob] {
        let state = replica.state().unwrap();
        let said: Vec<(String, String)> = state.issues[&issue]
            .comments
            .values()
            .map(|message| (message.author.clone(), message.text.clone()))
            .collect();
        assert_eq!(
            said,
            vec![
                ("ada".to_owned(), "Book the room".to_owned()),
                ("bob".to_owned(), "I will write the agenda".to_owned()),
            ],
            "both people read both messages, each attributed to its author"
        );
        assert!(
            state.issues[&issue]
                .comments
                .values()
                .all(|message| message.kind == MessageKind::Comment),
            "a message no run is going reads as a comment"
        );
    }
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
                quick: false,
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
            device: nb("Ada's laptop"),
        })
    };
    assert!(other.apply(on_steam_deck()).is_err());
    agent.apply(on_steam_deck()).unwrap();
    agent
        .apply(Tracker::SETTLE_SPEND.with(tracker::SettleSpend {
            issue: issue.clone(),
            step: priced_step(1, 1_200),
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

/// A run's lookups are listed on the task it works on: every resource it
/// reached past the one the request named, in the order it read them, each
/// naming the resource so the person follows it where the run went. Only the
/// run's own agent records one, and a read of nothing is refused.
#[test]
fn a_run_lists_every_resource_it_looked_up_on_its_task() {
    let mut person = ada();
    person
        .apply(Tracker::CREATE.with(create("Say what changed", None, Some(AGENT))))
        .unwrap();
    let issue = id_of(&person, "Say what changed");
    person
        .apply(Tracker::APPROVE_RUN.with(tracker::ApproveRun {
            issue: issue.clone(),
        }))
        .unwrap();
    let mut agent = person.fork().as_agent_for(AGENT, "ada");
    let mut other = agent.fork().as_agent_for("agent:other", "ada");
    let read = |resource: &str, label: &str| {
        Tracker::RECORD_READ.with(tracker::RecordRead {
            issue: issue.clone(),
            resource: ui::tilo_reference::ResourceAt {
                app: "so.tilo.notes".into(),
                record: None,
                reference: ui::tilo_reference::ResourceRef::whole("notes.page", resource, label),
            },
        })
    };

    assert!(
        other
            .apply(read("notes.page:release", "Release notes"))
            .is_err(),
        "another agent cannot list a read on this run"
    );
    for resource in ["", "  "] {
        assert!(
            agent.apply(read(resource, "Nothing")).is_err(),
            "a read names the resource it read"
        );
    }
    agent
        .apply(read("notes.page:release", "Release notes"))
        .unwrap();
    agent
        .apply(read("notes.page:standup", "Standup notes"))
        .unwrap();
    person.sync_with(&mut agent).unwrap();

    let state = person.state().unwrap();
    let lookups: Vec<(String, &str)> = state
        .issues
        .get(&issue)
        .unwrap()
        .comments
        .values()
        .filter(|message| message.kind == MessageKind::Lookup)
        .map(|message| {
            let read: ui::tilo_reference::ResourceAt =
                serde_json::from_str(&message.lookup).expect("a lookup names what it read");
            (
                format!("{}#{}", read.app, read.reference.id),
                message.text.as_str(),
            )
        })
        .collect();
    assert_eq!(
        lookups,
        [
            (
                "so.tilo.notes#notes.page:release".to_owned(),
                "Release notes"
            ),
            (
                "so.tilo.notes#notes.page:standup".to_owned(),
                "Standup notes"
            ),
        ]
    );
    // The run ended: what it read is part of it, so a later read is refused.
    agent
        .apply(Tracker::FINISH_RUN.with(tracker::FinishRun {
            issue: issue.clone(),
            outcome: RunOutcome::Done,
            summary: None,
        }))
        .unwrap();
    assert!(agent.apply(read("notes.page:other", "Other")).is_err());
}

/// Every field of a task lives in the tracker's own document, so it syncs to
/// the person's other devices, survives a restart, and merges between two
/// devices working on it as any other collaborative edit.
#[test]
fn a_task_syncs_to_another_device_survives_a_restart_and_merges_its_edits() {
    use crate::model::Area;

    let mut on_laptop = ada();
    on_laptop
        .apply(Tracker::CREATE.with(create("Prepare Q3", None, Some(AGENT))))
        .unwrap();
    let issue = id_of(&on_laptop, "Prepare Q3");
    on_laptop
        .apply(Tracker::PLAN.with(tracker::Plan {
            issue: issue.clone(),
            area: Some(Area {
                resource: "notes/finance/q3".into(),
                app: "so.tilo.notes".into(),
                kind: "notes.note".into(),
                label: "Q3 financials".into(),
            }),
            priority: Some(2),
            cost_budget: Some(5_000),
            due_ms: Some(1_800_000_000_000),
        }))
        .unwrap();
    // The person's other device joins with the tracker it holds and takes in
    // the rest: every field of the task is there, from the record alone. Each
    // device writes its own changes for the person at it, as a host does.
    let mut on_laptop = on_laptop.as_invoker("ada");
    let mut on_desktop = on_laptop.fork().as_invoker("ada");
    on_desktop.sync_with(&mut on_laptop).unwrap();
    let synced = on_desktop.state().unwrap();
    let read = synced.issues.get(&issue).unwrap();
    assert_eq!(read.title, "Prepare Q3");
    assert_eq!(read.assigned_agent(), Some(AGENT));
    assert_eq!(read.priority, Some(2));
    assert_eq!(read.cost_budget, Some(5_000));
    assert_eq!(
        read.area.as_ref().map(|area| area.label.as_str()),
        Some("Q3 financials")
    );

    // Both work on it at once, off the net: the desktop nests a subtask, the
    // laptop approves the run and its agent records what it used. Both edits
    // are ordinary collaborative changes and both survive the merge.
    on_desktop
        .apply(Tracker::CREATE.with(create("Gather the ledgers", Some(issue.clone()), None)))
        .unwrap();
    on_laptop
        .apply(Tracker::APPROVE_RUN.with(tracker::ApproveRun {
            issue: issue.clone(),
        }))
        .unwrap();
    let mut agent = on_laptop.fork().as_agent_for(AGENT, "ada");
    agent
        .apply(Tracker::SETTLE_SPEND.with(tracker::SettleSpend {
            issue: issue.clone(),
            step: priced_step(1, 1_200),
        }))
        .unwrap();
    on_laptop.sync_with(&mut agent).unwrap();
    on_desktop.sync_with(&mut on_laptop).unwrap();
    for merged in [on_laptop.state().unwrap(), on_desktop.state().unwrap()] {
        let task = merged.issues.get(&issue).unwrap();
        assert_eq!(task.spend, Some(1_200), "the run's spend is on both");
        assert!(
            merged
                .issues
                .iter()
                .any(|(_, other)| other.title == "Gather the ledgers"),
            "the subtask made on the other device is on both"
        );
    }

    // Restart: the tracker is rebuilt from its own bytes, and every field the
    // person could read before is still there.
    let before = on_desktop.state().unwrap();
    let reopened = Replica::<Tracker>::load(&on_desktop.save()).unwrap();
    let after = reopened.state().unwrap();
    assert_eq!(after.issues.len(), before.issues.len());
    let reopened_issue = after.issues.get(&issue).unwrap();
    assert_eq!(reopened_issue, before.issues.get(&issue).unwrap());
    assert_eq!(reopened_issue.spend, Some(1_200));
    assert_eq!(reopened_issue.priority, Some(2));
}

/// The task is a published resource of the action registry, and each change
/// says, typed, which task made it and which person it acted under.
#[test]
fn the_task_is_published_with_its_verbs_and_its_changes_carry_the_task() {
    let mut person = ada();
    person
        .apply(Tracker::CREATE.with(create("Release the notes", None, Some(AGENT))))
        .unwrap();
    let issue = id_of(&person, "Release the notes");

    // Published through the registry, with the verbs a surface or an agent
    // runs, rather than an app-private store.
    let tree = person.published().unwrap();
    assert_eq!(tree.kind, crate::model::KIND);
    let verbs: Vec<&str> = tree
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    for verb in ["create", "comment", "send-message", "approve-run", "plan"] {
        assert!(verbs.contains(&verb), "{verb} is published; got {verbs:?}");
    }

    // Every change says what it did and for whom, in fields the person reads:
    // the agent, the kind of actor, and the person it acted under.
    person
        .apply(Tracker::APPROVE_RUN.with(tracker::ApproveRun {
            issue: issue.clone(),
        }))
        .unwrap();
    let mut agent = person.fork().as_agent_for(AGENT, "ada");
    agent
        .apply(Tracker::SETTLE_SPEND.with(tracker::SettleSpend {
            issue,
            step: priced_step(1, 1_200),
        }))
        .unwrap();
    let versions = agent.versions();
    let spent = versions
        .iter()
        .rev()
        .find_map(|version| {
            let context = version.context.as_ref()?;
            context.what.as_ref()?;
            Some(context)
        })
        .expect("the run's spend is a version with what it did");
    let invoker = spent.invoker.as_ref().expect("the change names its actor");
    assert_eq!(invoker.actor, AGENT);
    assert_eq!(invoker.kind, ui::tilo_actions::ActorKind::Agent);
    assert_eq!(invoker.on_behalf_of.as_deref(), Some("ada"));
}

/// The run of `issue`, as a replica of `replica` reads it.
fn run_of(replica: &Replica<Tracker>, issue: &Id) -> Run {
    replica
        .state()
        .unwrap()
        .issues
        .get(issue)
        .and_then(|issue| issue.run.clone())
        .expect("the issue has a run")
}

#[test]
fn a_paused_run_takes_no_step_until_a_person_lets_it_go_on_from_its_boundary() {
    let mut person = ada();
    person
        .apply(Tracker::CREATE.with(create("Tidy the docs", None, Some(AGENT))))
        .unwrap();
    let issue = id_of(&person, "Tidy the docs");
    let mut agent = person.fork().as_agent_for(AGENT, "ada");
    person
        .apply(Tracker::APPROVE_RUN.with(tracker::ApproveRun {
            issue: issue.clone(),
        }))
        .unwrap();
    agent.sync_with(&mut person).unwrap();
    let report = |text: &str| {
        Tracker::REPORT_RUN.with(tracker::ReportRun {
            issue: issue.clone(),
            phase: RunPhase::Executing,
            text: Some(nb(text)),
        })
    };
    let pause = || {
        Tracker::PAUSE_RUN.with(tracker::PauseRun {
            issue: issue.clone(),
        })
    };
    let resume = || {
        Tracker::RESUME_RUN.with(tracker::ResumeRun {
            issue: issue.clone(),
        })
    };
    agent.apply(report("Tidying")).unwrap();
    // The person sees the step its agent reported before pausing it, so the
    // pause records where the run goes on from.
    person.sync_with(&mut agent).unwrap();
    assert_eq!(run_of(&person, &issue).phase, RunPhase::Executing);

    assert!(
        agent.apply(pause()).is_err(),
        "an agent does not pause its own run"
    );
    assert!(person.apply(resume()).is_err(), "no run is paused yet");
    // A message sent before the pause is part of the run: it exists at the
    // boundary the pause records.
    person
        .apply(Tracker::SEND_MESSAGE.with(tracker::SendMessage {
            issue: issue.clone(),
            text: nb("Keep the headings"),
        }))
        .unwrap();
    person.apply(pause()).unwrap();
    assert!(
        person.apply(pause()).is_err(),
        "a paused run is not paused again"
    );

    person.sync_with(&mut agent).unwrap();
    let run = run_of(&agent, &issue);
    assert_eq!(
        run.phase,
        RunPhase::Executing,
        "a paused run keeps its step"
    );
    assert!(run.phase.active(), "a paused run is still going");
    let paused = run.paused.expect("the boundary is recorded");
    assert_eq!(
        paused.phase(),
        Some(RunPhase::Executing),
        "it goes on in the step a person paused"
    );
    assert_eq!(paused.through, 1);
    assert!(
        agent.apply(report("Carrying on")).is_err(),
        "a paused run takes no new step"
    );
    assert!(
        agent
            .apply(Tracker::FINISH_RUN.with(tracker::FinishRun {
                issue: issue.clone(),
                outcome: RunOutcome::Done,
                summary: None,
            }))
            .is_err(),
        "a paused run does not end itself"
    );

    // It goes on in the step it stopped in.
    person.apply(resume()).unwrap();
    person.sync_with(&mut agent).unwrap();
    let run = run_of(&agent, &issue);
    assert_eq!(run.phase, RunPhase::Executing);
    assert!(
        run.paused.is_none(),
        "it goes on where it stopped, and is no longer paused"
    );
    agent.apply(report("Carrying on")).unwrap();
}

#[test]
fn a_run_that_stopped_showing_a_step_is_ended_as_idle_and_leaves_what_it_did() {
    let mut person = ada();
    person
        .apply(Tracker::CREATE.with(create("Tidy the docs", None, Some(AGENT))))
        .unwrap();
    let issue = id_of(&person, "Tidy the docs");
    let mut agent = person.fork().as_agent_for(AGENT, "ada");
    person
        .apply(Tracker::APPROVE_RUN.with(tracker::ApproveRun {
            issue: issue.clone(),
        }))
        .unwrap();
    agent.sync_with(&mut person).unwrap();
    agent
        .apply(Tracker::REPORT_RUN.with(tracker::ReportRun {
            issue: issue.clone(),
            phase: RunPhase::Executing,
            text: Some(nb("Tidying")),
        }))
        .unwrap();
    person.sync_with(&mut agent).unwrap();
    agent
        .apply(Tracker::SETTLE_SPEND.with(tracker::SettleSpend {
            issue: issue.clone(),
            step: priced_step(1, 900),
        }))
        .unwrap();
    person.sync_with(&mut agent).unwrap();

    let idle = |after_ms: i64| {
        Tracker::END_IDLE_RUN.with(tracker::EndIdleRun {
            issue: issue.clone(),
            after_ms,
        })
    };
    assert!(
        person.apply(idle(0)).is_err(),
        "an idle run waits at least a moment"
    );
    assert!(
        person.apply(idle(600_000)).is_err(),
        "a run that just showed a step is not idle"
    );
    assert_eq!(run_of(&person, &issue).phase, RunPhase::Executing);

    // Once it has been quiet for longer than its person allows, it stops.
    person.set_now_ms(run_of(&person, &issue).updated_ms + 600_000);
    person.apply(idle(600_000)).unwrap();
    let state = person.state().unwrap();
    let ended = state.issues.get(&issue).unwrap();
    assert_eq!(
        ended.status,
        Status::Todo,
        "it is left to be picked up again"
    );
    assert_eq!(
        ended.spend,
        Some(900),
        "what it did before it went quiet stays"
    );
    let run = ended.run.as_ref().expect("the run is kept to read");
    assert_eq!(run.phase, RunPhase::Stopped);
}

#[test]
fn stopping_a_paused_run_ends_it_and_leaves_what_it_did() {
    let mut person = ada();
    person
        .apply(Tracker::CREATE.with(create("Tidy the docs", None, Some(AGENT))))
        .unwrap();
    let issue = id_of(&person, "Tidy the docs");
    person
        .apply(Tracker::APPROVE_RUN.with(tracker::ApproveRun {
            issue: issue.clone(),
        }))
        .unwrap();
    person
        .apply(Tracker::PAUSE_RUN.with(tracker::PauseRun {
            issue: issue.clone(),
        }))
        .unwrap();
    person
        .apply(Tracker::STOP_RUN.with(tracker::StopRun {
            issue: issue.clone(),
        }))
        .unwrap();
    let state = person.state().unwrap();
    let stopped = state.issues.get(&issue).unwrap();
    assert_eq!(stopped.status, Status::Todo);
    let run = stopped.run.as_ref().unwrap();
    assert_eq!(run.phase, RunPhase::Stopped);
    assert!(run.paused.is_none(), "a stopped run is not still paused");
}

#[test]
fn a_quick_question_is_a_task_that_ends_when_its_agent_answers_it() {
    let mut person = ada();
    let id = Id::from_raw("quick");
    person
        .apply(Tracker::ASK_AGENT.with(tracker::AskAgent {
            ask: crate::model::AgentRequest {
                id: id.clone(),
                title: "Which venue seats 120?".into(),
                body: None,
                agent: "claude".into(),
                model: None,
                subject: None,
                quick: true,
            },
        }))
        .unwrap();
    assert!(
        person.state().unwrap().issues.get(&id).unwrap().quick,
        "the task it made is a question"
    );
    let mut agent = person.fork().as_agent_for(AGENT, "ada");
    agent
        .apply(Tracker::FINISH_RUN.with(tracker::FinishRun {
            issue: id.clone(),
            outcome: RunOutcome::Done,
            summary: Some(nb("The old depot on Mill Street seats 120.")),
        }))
        .unwrap();
    let state = agent.state().unwrap();
    let issue = state.issues.get(&id).unwrap();
    assert_eq!(
        issue.status,
        Status::Done,
        "an answered question ends rather than waiting for a review"
    );
    // What was asked and what came back stay in the task, findable later.
    assert_eq!(issue.comments.len(), 1);
    assert_eq!(issue.title, "Which venue seats 120?");
}

#[test]
fn work_asked_of_an_agent_waits_for_a_person_to_review_what_came_back() {
    let mut person = ada();
    let id = Id::from_raw("work");
    person
        .apply(Tracker::ASK_AGENT.with(tracker::AskAgent {
            ask: crate::model::AgentRequest {
                id: id.clone(),
                title: "Write the launch notes".into(),
                body: None,
                agent: "claude".into(),
                model: None,
                subject: None,
                quick: false,
            },
        }))
        .unwrap();
    let mut agent = person.fork().as_agent_for(AGENT, "ada");
    agent
        .apply(Tracker::FINISH_RUN.with(tracker::FinishRun {
            issue: id.clone(),
            outcome: RunOutcome::Done,
            summary: None,
        }))
        .unwrap();
    assert_eq!(
        agent.state().unwrap().issues.get(&id).unwrap().status,
        Status::InReview,
        "work done by an agent is reviewed, not closed"
    );
}

/// The project is an ordinary resource above the tasks: it names itself and
/// its kind, the tasks that work towards it are the ones it holds, and those
/// same tasks are what their person's own list and board show.
#[test]
fn a_project_holds_the_tasks_that_work_towards_it_and_the_people_involved() {
    use crate::model::{Kind, area_goal};

    let mut person = ada();
    person
        .apply(Tracker::CREATE_AREA.with(tracker::CreateArea {
            name: nb("Launch"),
            kind: Some(Kind::Project),
        }))
        .unwrap();
    let project = person
        .state()
        .unwrap()
        .areas
        .iter()
        .next()
        .map(|(id, _)| id.clone())
        .expect("the project is there");
    let read = person.state().unwrap();
    let made = read.area(&project).expect("the project");
    assert_eq!(made.name, "Launch");
    assert_eq!(made.kind, Kind::Project);
    assert!(
        serde_json::from_value::<tracker::CreateArea>(serde_json::json!({"name": "  "})).is_err(),
        "a project with no name is refused where the arguments are read"
    );

    // Two tasks work towards it, one of them as a sub-task of the other, and
    // a third works towards nothing: the project holds the two, at any depth
    // through their sub-tasks.
    for title in ["Write the notes", "Draw the slides", "Book a holiday"] {
        person
            .apply(Tracker::CREATE.with(create(title, None, None)))
            .unwrap();
    }
    let notes = id_of(&person, "Write the notes");
    person
        .apply(Tracker::CREATE.with(create("Read them back", Some(notes.clone()), None)))
        .unwrap();
    let read_back = id_of(&person, "Read them back");
    for issue in [&notes, &read_back] {
        person
            .apply(Tracker::PLAN.with(tracker::Plan {
                issue: issue.clone(),
                area: Some(area_goal(&project, "Launch")),
                priority: None,
                cost_budget: None,
                due_ms: None,
            }))
            .unwrap();
    }
    let held: Vec<String> = person
        .state()
        .unwrap()
        .in_area(&project)
        .map(|(_, issue)| issue.title.clone())
        .collect();
    assert_eq!(held, ["Write the notes", "Read them back"]);

    // The task reads in the project and in the tracker's list alike: the
    // same issue, named as working towards the project.
    let listed = person.state().unwrap().issues.get(&notes).cloned();
    let listed = listed.expect("the task is in the tracker's list");
    let listed = listed.area.as_ref().map(|area| area.label.as_str());
    assert_eq!(listed, Some("Launch"));
}

/// Who is involved in a project is the project's own to keep, and a document
/// it holds is a link one of its tasks carries.
#[test]
fn a_projects_people_carry_their_role_and_its_documents_are_its_tasks_links() {
    use crate::model::area_goal;

    let mut person = ada();
    person
        .apply(Tracker::CREATE_AREA.with(tracker::CreateArea {
            name: nb("Launch"),
            kind: None,
        }))
        .unwrap();
    let project = person
        .state()
        .unwrap()
        .areas
        .iter()
        .map(|(id, _)| id.clone())
        .next()
        .expect("the project");
    person
        .apply(Tracker::CREATE.with(create("Write the notes", None, None)))
        .unwrap();
    let notes = id_of(&person, "Write the notes");
    person
        .apply(Tracker::PLAN.with(tracker::Plan {
            issue: notes.clone(),
            area: Some(area_goal(&project, "Launch")),
            priority: None,
            cost_budget: None,
            due_ms: None,
        }))
        .unwrap();

    // A document it holds is a link the task carries, so it is read on the
    // project's page without being stored a second time.
    person
        .apply(Tracker::LINK.with(tracker::Link {
            issue: notes,
            app: "so.tilo.notes".parse().unwrap(),
            kind: "notes.note".into(),
            resource: "notes/launch".parse().unwrap(),
            label: "The launch plan".into(),
            record: None,
        }))
        .unwrap();

    // Who is involved is the project's own to keep, with the role each has.
    for (name, role) in [("ada", "writes the notes"), ("grace", "draws")] {
        person
            .apply(Tracker::INVOLVE.with(tracker::Involve {
                area: project.clone(),
                name: nb(name),
                role: Some(role.into()),
            }))
            .unwrap();
    }
    let read = person.state().unwrap();
    let people: Vec<(String, String)> = read
        .area(&project)
        .expect("the project")
        .people
        .values()
        .map(|person| (person.name.clone(), person.role.clone()))
        .collect();
    assert_eq!(
        people,
        [
            ("ada".to_owned(), "writes the notes".to_owned()),
            ("grace".to_owned(), "draws".to_owned())
        ]
    );

    // Taking someone out of the project leaves the project's other people.
    let grace = read
        .area(&project)
        .expect("the project")
        .people
        .iter()
        .find(|(_, person)| person.name == "grace")
        .map(|(id, _)| id.clone())
        .expect("grace is involved");
    person
        .apply(Tracker::REMOVE_PERSON.with(tracker::RemovePerson {
            area: project.clone(),
            person: grace,
        }))
        .unwrap();
    assert_eq!(
        person.state().unwrap().area(&project).unwrap().people.len(),
        1
    );
    assert!(
        person
            .apply(Tracker::REMOVE_PERSON.with(tracker::RemovePerson {
                area: Id::from_raw("gone"),
                person: Id::from_raw("gone"),
            }))
            .is_err(),
        "a project that is gone takes nobody out"
    );
}

/// A project and an area differ only in what they say about themselves: the
/// kind is what a person reads, and a project is one by default.
#[test]
fn an_area_is_a_project_that_goes_on() {
    use crate::model::Kind;

    let mut person = ada();
    person
        .apply(Tracker::CREATE_AREA.with(tracker::CreateArea {
            name: nb("Support"),
            kind: Some(Kind::Area),
        }))
        .unwrap();
    person
        .apply(Tracker::CREATE_AREA.with(tracker::CreateArea {
            name: nb("Launch"),
            kind: None,
        }))
        .unwrap();
    let read = person.state().unwrap();
    let kinds: Vec<Kind> = read.areas.values().map(|area| area.kind).collect();
    assert_eq!(kinds, [Kind::Area, Kind::Project]);

    let support = read
        .areas
        .iter()
        .find(|(_, area)| area.name == "Support")
        .map(|(id, _)| id.clone())
        .expect("the area");
    person
        .apply(Tracker::RENAME_AREA.with(tracker::RenameArea {
            area: support.clone(),
            name: nb("Customer support"),
        }))
        .unwrap();
    let read = person.state().unwrap();
    assert_eq!(read.area(&support).unwrap().name, "Customer support");
    assert_eq!(
        read.area(&support).unwrap().kind,
        Kind::Area,
        "the kind stays"
    );
    assert!(
        serde_json::from_value::<tracker::RenameArea>(
            serde_json::json!({"area": support.to_string(), "name": " "})
        )
        .is_err(),
        "a project keeps a name"
    );
}

/// The project and who is involved in it are the tracker's document, as the
/// tasks are: they reach the person's other device and survive a restart.
#[test]
fn a_projects_tasks_and_people_reach_the_other_device_and_a_restart() {
    use crate::model::{Kind, area_goal};

    let mut on_laptop = ada();
    on_laptop
        .apply(Tracker::CREATE_AREA.with(tracker::CreateArea {
            name: nb("Launch"),
            kind: Some(Kind::Project),
        }))
        .unwrap();
    let project = on_laptop
        .state()
        .unwrap()
        .areas
        .iter()
        .map(|(id, _)| id.clone())
        .next()
        .expect("the project");
    on_laptop
        .apply(Tracker::INVOLVE.with(tracker::Involve {
            area: project.clone(),
            name: nb("ada"),
            role: Some("writes".into()),
        }))
        .unwrap();
    on_laptop
        .apply(Tracker::CREATE.with(create("Write the notes", None, None)))
        .unwrap();
    let notes = id_of(&on_laptop, "Write the notes");
    on_laptop
        .apply(Tracker::PLAN.with(tracker::Plan {
            issue: notes,
            area: Some(area_goal(&project, "Launch")),
            priority: None,
            cost_budget: None,
            due_ms: None,
        }))
        .unwrap();

    let mut on_laptop = on_laptop.as_invoker("ada");
    let mut on_desktop = on_laptop.fork().as_invoker("ada");
    on_desktop.sync_with(&mut on_laptop).unwrap();
    for merged in [on_laptop.state().unwrap(), on_desktop.state().unwrap()] {
        let area = merged.area(&project).expect("the project on both");
        assert_eq!(area.name, "Launch");
        assert_eq!(area.people.len(), 1);
        assert_eq!(merged.in_area(&project).count(), 1);
    }

    let reopened = Replica::<Tracker>::load(&on_desktop.save()).unwrap();
    let after = reopened.state().unwrap();
    let area = after.area(&project).expect("the project after a restart");
    assert_eq!(area.name, "Launch");
    assert_eq!(area.kind, Kind::Project);
    assert_eq!(
        area.people
            .values()
            .next()
            .map(|person| person.name.as_str()),
        Some("ada")
    );
    assert_eq!(after.in_area(&project).count(), 1);
}

/// The project and the people in it are published as anything of the record
/// is: the tracker offers a place to make one, and each project is a resource
/// of its own with the actions a person takes on it.
#[test]
fn the_project_is_published_with_its_verbs_and_a_resource_of_its_own() {
    use crate::model::{Kind, area_resource};

    let mut person = ada();
    person
        .apply(Tracker::CREATE_AREA.with(tracker::CreateArea {
            name: nb("Launch"),
            kind: Some(Kind::Project),
        }))
        .unwrap();
    let project = person
        .state()
        .unwrap()
        .areas
        .iter()
        .map(|(id, _)| id.clone())
        .next()
        .expect("the project");

    let tree = person.published().unwrap();
    let verbs: Vec<&str> = tree
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    for verb in ["create-area", "rename-area", "involve", "remove-person"] {
        assert!(verbs.contains(&verb), "{verb} is published; got {verbs:?}");
    }
    let children: Vec<&str> = tree
        .children
        .iter()
        .map(|child| child.id.as_str())
        .collect();
    let address = area_resource(&project);
    assert!(
        children.contains(&address.as_str()),
        "the project is a resource of its own: {children:?}"
    );
}

/// A budget the tests set with only the limits they care about.
fn limits(max_tokens: Option<u64>, max_cost_micros: Option<u64>, currency: &str) -> SpendLimits {
    SpendLimits {
        max_tokens,
        max_priced_calls: None,
        max_cost_micros,
        currency: currency.into(),
    }
}

#[test]
fn a_step_that_would_cross_a_budget_is_refused_naming_the_limit() {
    let mut replica = ada();
    replica
        .apply(Tracker::CREATE.with(create("Budgeted", None, Some(AGENT))))
        .unwrap();
    let issue = id_of(&replica, "Budgeted");
    replica
        .apply(
            Tracker::SET_EXECUTION_BUDGET.with(tracker::SetExecutionBudget {
                issue: issue.clone(),
                turn_ms: Some(1_000),
                total_ms: Some(60_000),
                limits: Some(limits(Some(1_000), Some(2_000_000), "EUR")),
            }),
        )
        .unwrap();
    replica
        .apply(Tracker::APPROVE_RUN.with(tracker::ApproveRun {
            issue: issue.clone(),
        }))
        .unwrap();
    let mut agent = replica.fork().as_agent_for(AGENT, "ada");
    // A hold that fits is taken; the run's budget is measured with it.
    agent
        .apply(Tracker::HOLD_SPEND.with(tracker::HoldSpend {
            issue: issue.clone(),
            tokens: 900,
            cost_micros: 0,
        }))
        .unwrap();
    let refused = agent
        .apply(Tracker::HOLD_SPEND.with(tracker::HoldSpend {
            issue,
            tokens: 200,
            cost_micros: 0,
        }))
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("1,000 tokens") || refused.contains("1000 tokens"),
        "the refusal names the limit it would cross: {refused}"
    );
}

#[test]
fn a_run_on_a_local_model_records_its_model_and_device_and_spends_no_money() {
    let mut replica = ada();
    replica
        .apply(Tracker::CREATE.with(create("Local", None, Some(AGENT))))
        .unwrap();
    let issue = id_of(&replica, "Local");
    replica
        .apply(Tracker::APPROVE_RUN.with(tracker::ApproveRun {
            issue: issue.clone(),
        }))
        .unwrap();
    let mut agent = replica.fork().as_agent_for(AGENT, "ada");
    agent
        .apply(Tracker::SETTLE_SPEND.with(tracker::SettleSpend {
            issue: issue.clone(),
            step: StepSpend {
                step: 1,
                model: "llama-3.3-70b".into(),
                device: "Ada's laptop".into(),
                input_tokens: 700,
                output_tokens: 300,
                priced_calls: 0,
                cost_micros: 0,
                currency: String::new(),
            },
        }))
        .unwrap();
    replica.sync_with(&mut agent).unwrap();
    let state = replica.state().unwrap();
    let run = state.issues.get(&issue).unwrap().run.as_ref().unwrap();
    let spend = run.spend.as_ref().expect("the run recorded what it spent");
    assert_eq!(spend.tokens(), 1_000);
    assert_eq!(spend.cost(), 0, "a local model spends time, not money");
    let step = spend.steps.first().expect("the step is kept");
    assert_eq!(step.model, "llama-3.3-70b");
    assert_eq!(step.device, "Ada's laptop");
    assert_eq!(state.issues.get(&issue).unwrap().spend, None);
}

#[test]
fn a_subtasks_spend_counts_towards_its_parent() {
    let mut replica = ada();
    replica
        .apply(Tracker::CREATE.with(create("Ship it", None, Some(AGENT))))
        .unwrap();
    let parent = id_of(&replica, "Ship it");
    replica
        .apply(Tracker::CREATE.with(create("Write notes", Some(parent.clone()), Some(AGENT))))
        .unwrap();
    let child = id_of(&replica, "Write notes");
    for issue in [parent.clone(), child.clone()] {
        replica
            .apply(Tracker::APPROVE_RUN.with(tracker::ApproveRun {
                issue: issue.clone(),
            }))
            .unwrap();
        let mut agent = replica.fork().as_agent_for(AGENT, "ada");
        agent
            .apply(Tracker::SETTLE_SPEND.with(tracker::SettleSpend {
                issue,
                step: priced_step(1, 1_500_000),
            }))
            .unwrap();
        replica.sync_with(&mut agent).unwrap();
    }
    let state = replica.state().unwrap();
    assert_eq!(state.issues.get(&child).unwrap().spend, Some(1_500_000));
    assert_eq!(
        state.issues.get(&parent).unwrap().spend,
        Some(3_000_000),
        "the parent's spend is its own and its sub-issue's"
    );
}

#[test]
fn a_run_that_reaches_its_budget_pauses_saying_which_limit_it_reached() {
    let mut replica = ada();
    replica
        .apply(Tracker::CREATE.with(create("Tight", None, Some(AGENT))))
        .unwrap();
    let issue = id_of(&replica, "Tight");
    replica
        .apply(Tracker::APPROVE_RUN.with(tracker::ApproveRun {
            issue: issue.clone(),
        }))
        .unwrap();
    let mut agent = replica.fork().as_agent_for(AGENT, "ada");
    assert!(
        serde_json::from_value::<tracker::PauseRunForBudget>(
            serde_json::json!({"issue": issue.to_string(), "reason": "   "})
        )
        .is_err(),
        "a pause names the limit it reached"
    );
    agent
        .apply(
            Tracker::PAUSE_RUN_FOR_BUDGET.with(tracker::PauseRunForBudget {
                issue: issue.clone(),
                reason: nb("This task's budget of 1.00 EUR is spent"),
            }),
        )
        .unwrap();
    replica.sync_with(&mut agent).unwrap();
    let state = replica.state().unwrap();
    let paused = state
        .issues
        .get(&issue)
        .unwrap()
        .run
        .as_ref()
        .and_then(|run| run.paused.as_ref())
        .expect("the run is paused");
    assert_eq!(paused.reason, "This task's budget of 1.00 EUR is spent");
    assert_eq!(
        paused.phase(),
        Some(RunPhase::Executing),
        "it waits in the step it was in"
    );
}

/// A conversation with an agent goes on after its run ended: what the
/// person tells it then starts the next run, which reads the message as its
/// direction; while a run goes, the same message steers it.
#[test]
fn telling_an_agent_steers_its_run_or_starts_the_next_one_with_the_message() {
    let mut person = ada();
    person
        .apply(Tracker::CREATE.with(create("Review the applications", None, Some(AGENT))))
        .unwrap();
    let issue = id_of(&person, "Review the applications");
    let tell = |text: &str| {
        Tracker::TELL_AGENT.with(tracker::TellAgent {
            issue: issue.clone(),
            text: nb(text),
        })
    };

    // Nothing runs yet: telling the agent starts its first run with it.
    person.apply(tell("Start with the newest")).unwrap();
    assert_eq!(run_of(&person, &issue).epoch, 1);
    let state = person.state().unwrap();
    let first: Vec<&str> = state
        .issues
        .get(&issue)
        .unwrap()
        .messages_for_run(1)
        .map(|(_, message)| message.text.as_str())
        .collect();
    assert_eq!(first, ["Start with the newest"]);

    // While it goes, the message steers that run.
    person.apply(tell("Skip the duplicates")).unwrap();
    assert_eq!(run_of(&person, &issue).epoch, 1);
    assert_eq!(
        person
            .state()
            .unwrap()
            .issues
            .get(&issue)
            .unwrap()
            .messages_for_run(1)
            .count(),
        2
    );

    // The run ends; the next message starts run 2, approved by the person,
    // and only that message is the new run's direction.
    let mut agent = person.fork().as_agent_for(AGENT, "ada");
    agent
        .apply(Tracker::INCORPORATE.with(tracker::Incorporate {
            issue: issue.clone(),
            through: 2,
        }))
        .unwrap();
    agent
        .apply(Tracker::FINISH_RUN.with(tracker::FinishRun {
            issue: issue.clone(),
            outcome: RunOutcome::Done,
            summary: Some(nb("Reviewed 3 applications")),
        }))
        .unwrap();
    person.sync_with(&mut agent).unwrap();
    person.apply(tell("Now draft the replies")).unwrap();
    let run = run_of(&person, &issue);
    assert_eq!((run.epoch, run.starter.as_str()), (2, "ada"));
    assert!(run.phase.active());
    let state = person.state().unwrap();
    let next: Vec<&str> = state
        .issues
        .get(&issue)
        .unwrap()
        .messages_for_run(2)
        .map(|(_, message)| message.text.as_str())
        .collect();
    assert_eq!(next, ["Now draft the replies"]);

    // Only a person tells an agent, and only an agent's issue has one.
    assert!(agent.apply(tell("Approve yourself")).is_err());
    person
        .apply(Tracker::CREATE.with(create("Book the room", None, None)))
        .unwrap();
    let nobodys = id_of(&person, "Book the room");
    assert!(
        person
            .apply(Tracker::TELL_AGENT.with(tracker::TellAgent {
                issue: nobodys,
                text: nb("Hello"),
            }))
            .is_err()
    );
}
