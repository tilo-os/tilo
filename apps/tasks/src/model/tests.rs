#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion"
)]

use ui::tilo_resource::{Id, Replica};

use crate::model::{
    Imported, ImportedMessage, MessageKind, Question, Run, RunOutcome, RunPhase, Status,
    TaskPreset, Tracker, agent_assignee, tracker,
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
                quick: false,
                parent: None,
                preset: crate::model::TaskPreset::Inherit,
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
        .apply(Tracker::RECORD_SPEND.with(tracker::RecordSpend {
            issue: issue.clone(),
            spent: 1_200,
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
        .apply(Tracker::RECORD_SPEND.with(tracker::RecordSpend {
            issue,
            spent: 1_200,
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
            text: Some(text.into()),
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
            text: "Keep the headings".into(),
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
            text: Some("Tidying".into()),
        }))
        .unwrap();
    person.sync_with(&mut agent).unwrap();
    agent
        .apply(Tracker::RECORD_SPEND.with(tracker::RecordSpend {
            issue: issue.clone(),
            spent: 900,
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

/// An ask of `preset` under `parent`, made as its agent asks for the work
/// under a task it runs.
fn ask(id: &str, parent: Option<Id>, preset: TaskPreset) -> ui::tilo_resource::Invoke<Tracker> {
    Tracker::ASK_AGENT.with(tracker::AskAgent {
        ask: crate::model::AgentRequest {
            id: Id::from_raw(id),
            title: format!("part {id}"),
            body: None,
            agent: "claude".into(),
            model: None,
            subject: None,
            quick: false,
            parent,
            preset,
        },
    })
}

fn preset_of(replica: &Replica<Tracker>, id: &str) -> Vec<String> {
    replica
        .state()
        .unwrap()
        .issues
        .get(&Id::from_raw(id))
        .expect("the issue is there")
        .effective_permissions()
}

/// A whole task with no task under it: it holds nothing, as a subtask whose
/// task above was never settled holds nothing.
fn rooted(id: &str) -> Id {
    Id::from_raw(id)
}

fn start(replica: &mut Replica<Tracker>, id: &str) {
    replica.apply(ask(id, None, TaskPreset::Inherit)).unwrap();
}

/// A subtask of `parent`, asked by the task above's own agent, as a run
/// launching part of its work does.
fn start_below(replica: &mut Replica<Tracker>, id: &str, parent: &str) {
    let mut agent = replica.fork().as_agent_for(AGENT, "ada");
    agent
        .apply(ask(id, Some(rooted(parent)), TaskPreset::Inherit))
        .unwrap();
    replica.sync_with(&mut agent).unwrap();
}

/// The Tasks app's issues, as a permission: a change among them.
const ISSUES: &str = "so.tilo.tasks/issues.change";
/// Writing a note's body.
const NOTES: &str = "so.tilo.notes/note.body.change";

/// The permissions a preset settles on, as a list.
fn held(permissions: &[&str]) -> Vec<String> {
    permissions
        .iter()
        .map(|permission| (*permission).to_owned())
        .collect()
}

/// Stop whatever run `id`'s task has going, as its person does to change
/// what it may do.
fn stop(replica: &mut Replica<Tracker>, id: &str) {
    if replica
        .state()
        .unwrap()
        .issues
        .get(&rooted(id))
        .is_some_and(|issue| issue.active_run().is_some())
    {
        replica
            .apply(Tracker::STOP_RUN.with(tracker::StopRun { issue: rooted(id) }))
            .unwrap();
    }
}

/// A task at the top whose preset is `held`, settled by its own person.
fn task_holding(replica: &mut Replica<Tracker>, id: &str, held: &[&str]) {
    start(replica, id);
    stop(replica, id);
    replica
        .apply(Tracker::SET_PRESET.with(tracker::SetPreset {
            issue: rooted(id),
            preset: TaskPreset::Explicit {
                permissions: held.iter().map(|held| (*held).to_owned()).collect(),
            },
        }))
        .unwrap();
    assert_eq!(
        preset_of(replica, id),
        held.to_vec(),
        "{id} holds what it was asked for"
    );
}

/// Whether `error` reads as the permission `wanted`, in a person's words.
fn names(error: &str, wanted: &str) -> bool {
    error.starts_with("Not allowed:") && error.contains(wanted)
}

#[test]
fn a_task_at_the_top_settles_its_own_preset() {
    let mut person = ada();
    // Nothing narrows a task with no task above it, so a preset it names is
    // read as well formed and left to be settled as its run is admitted.
    person
        .apply(ask("root", None, TaskPreset::Inherit))
        .unwrap();
    assert!(preset_of(&person, "root").is_empty());
    let error = person
        .apply(ask(
            "word",
            None,
            TaskPreset::Explicit {
                permissions: vec!["write".into()],
            },
        ))
        .unwrap_err()
        .to_string();
    assert!(error.contains("does not name a permission"), "{error}");
}

#[test]
fn nothing_working_under_a_task_holds_what_it_does_not() {
    let mut person = ada();
    task_holding(&mut person, "root", &[ISSUES]);
    let root = rooted("root");

    // What the task above does not hold is asked of the task, never taken on
    // the asking's word.
    let error = person
        .apply(ask(
            "greedy",
            Some(root.clone()),
            TaskPreset::Explicit {
                permissions: vec![NOTES.into()],
            },
        ))
        .unwrap_err()
        .to_string();
    assert!(
        names(&error, "note body change in so.tilo.notes"),
        "{error}"
    );
    assert!(
        !error.contains("so.tilo.notes/"),
        "the permission, in the words a person reads: {error}"
    );

    // A preset that names no permission is refused whoever asks.
    let error = person
        .apply(ask(
            "word",
            Some(root),
            TaskPreset::Explicit {
                permissions: vec!["write".into()],
            },
        ))
        .unwrap_err()
        .to_string();
    assert!(error.contains("does not name a permission"), "{error}");
}

#[test]
fn a_subtask_is_settled_within_the_task_that_launched_it() {
    let mut person = ada();
    task_holding(&mut person, "root", &[ISSUES, NOTES]);
    let root = rooted("root");

    // Everything the task above holds, and nothing more.
    start_below(&mut person, "plain", "root");
    assert_eq!(preset_of(&person, "plain"), held(&[ISSUES, NOTES]));

    // All of it but the notes.
    stop(&mut person, "plain");
    person
        .apply(Tracker::SET_PRESET.with(tracker::SetPreset {
            issue: rooted("plain"),
            preset: TaskPreset::InheritMinus {
                apps: vec!["so.tilo.notes".into()],
            },
        }))
        .unwrap();
    assert_eq!(preset_of(&person, "plain"), held(&[ISSUES]));
    assert_eq!(
        person
            .state()
            .unwrap()
            .issues
            .get(&rooted("plain"))
            .unwrap()
            .permitted_by,
        held(&[ISSUES, NOTES]),
        "and it reads what the preset left of what the task above holds"
    );

    // Only these: the one it names, of what the task above holds.
    start_below(&mut person, "listed", "root");
    stop(&mut person, "listed");
    person
        .apply(Tracker::SET_PRESET.with(tracker::SetPreset {
            issue: rooted("listed"),
            preset: TaskPreset::Explicit {
                permissions: vec![ISSUES.into()],
            },
        }))
        .unwrap();
    assert_eq!(preset_of(&person, "listed"), held(&[ISSUES]));
    let _ = root;
}

#[test]
fn taking_a_permission_from_a_task_leaves_every_task_under_it_without_it() {
    let mut person = ada();
    task_holding(&mut person, "root", &[ISSUES, NOTES]);
    start_below(&mut person, "child", "root");
    start_below(&mut person, "below", "child");
    assert_eq!(
        preset_of(&person, "below"),
        held(&[ISSUES, NOTES]),
        "a task two deep works under a task under a task"
    );

    // Narrowing the task at the top leaves the tasks under it holding what
    // it holds now, never what it held.
    stop(&mut person, "root");
    person
        .apply(Tracker::SET_PRESET.with(tracker::SetPreset {
            issue: rooted("root"),
            preset: TaskPreset::Explicit {
                permissions: vec![ISSUES.into()],
            },
        }))
        .unwrap();
    assert_eq!(preset_of(&person, "root"), held(&[ISSUES]));

    // Asking again for the notes under the child is refused in the words a
    // person reads: the child's own set is what a task under it is settled
    // within, and that no longer holds them.
    let mut agent = person.fork().as_agent_for(AGENT, "ada");
    let error = agent
        .apply(ask(
            "wider",
            Some(rooted("child")),
            TaskPreset::Explicit {
                permissions: vec![NOTES.into()],
            },
        ))
        .unwrap_err()
        .to_string();
    assert!(
        names(&error, "note body change in so.tilo.notes"),
        "{error}"
    );

    // Settled again by the child's own run, the task below holds what the
    // child holds now.
    let mut child_agent = person.fork().as_agent_for(AGENT, "ada");
    child_agent
        .apply(
            Tracker::SETTLE_PERMISSIONS.with(tracker::SettlePermissions {
                issue: rooted("below"),
            }),
        )
        .unwrap();
    person.sync_with(&mut child_agent).unwrap();
    assert_eq!(preset_of(&person, "below"), held(&[ISSUES]));
    assert_eq!(
        person
            .state()
            .unwrap()
            .issues
            .get(&rooted("below"))
            .unwrap()
            .permitted_by,
        held(&[ISSUES])
    );
}

#[test]
fn only_a_person_or_the_runs_own_agent_changes_what_it_may_do() {
    let mut person = ada();
    task_holding(&mut person, "root", &[ISSUES, NOTES]);
    start_below(&mut person, "child", "root");
    let child = rooted("child");
    let narrower = TaskPreset::InheritMinus {
        apps: vec!["so.tilo.notes".into()],
    };

    // Another agent does not narrow it.
    let mut stranger = person.fork().as_agent_for("agent:other", "ada");
    assert!(
        stranger
            .apply(Tracker::SET_PRESET.with(tracker::SetPreset {
                issue: child.clone(),
                preset: narrower.clone(),
            }))
            .is_err(),
        "another agent changes nothing here"
    );

    // The person who approved the task above does, once no run of the
    // subtask is going.
    assert!(
        person
            .apply(Tracker::SET_PRESET.with(tracker::SetPreset {
                issue: child.clone(),
                preset: narrower.clone(),
            }))
            .is_err(),
        "not under a going run"
    );
    stop(&mut person, "child");
    person
        .apply(Tracker::SET_PRESET.with(tracker::SetPreset {
            issue: child.clone(),
            preset: narrower,
        }))
        .unwrap();
    assert_eq!(preset_of(&person, "child"), held(&[ISSUES]));

    // And never more than the task above holds.
    assert!(
        person
            .apply(Tracker::SET_PRESET.with(tracker::SetPreset {
                issue: child,
                preset: TaskPreset::Explicit {
                    permissions: vec!["so.tilo.mail/mailbox.change".into()],
                },
            }))
            .is_err(),
        "a permission the task above does not hold"
    );
}

#[test]
fn a_preset_names_the_permissions_it_hands_a_task() {
    // An app-wide permission admits every permission below it, and not one
    // of another app whose id starts the same way.
    assert!(crate::model::run::admits_permission(
        "so.tilo.notes/*",
        NOTES
    ));
    assert!(!crate::model::run::admits_permission(
        "so.tilo.notes/*",
        "so.tilo.notes-archive/note.body.change"
    ));
    // A permission in the words a person reads names what it is done to and
    // the app it is done in, never a path through the tree.
    assert_eq!(
        crate::model::run::permission_sentence(NOTES),
        "note body change in so.tilo.notes"
    );
    assert_eq!(
        crate::model::run::permission_sentence("so.tilo.notes/*"),
        "everything in so.tilo.notes"
    );
}
