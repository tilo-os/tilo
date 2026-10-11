use serde_json::json;
use tilo_automation::{Binding, StepKind};
use ui::prelude::Replica;

use super::*;

fn draft_id(replica: &Replica<Workbench>) -> Id {
    replica
        .state()
        .unwrap()
        .drafts
        .ids()
        .next()
        .cloned()
        .unwrap()
}

fn program(replica: &Replica<Workbench>) -> Program {
    replica
        .state()
        .unwrap()
        .drafts
        .values()
        .next()
        .unwrap()
        .program()
}

fn with_draft() -> (Replica<Workbench>, Id) {
    let mut replica = Replica::<Workbench>::new();
    replica
        .apply(Workbench::NEW_DRAFT.with("Tidy".into()))
        .unwrap();
    let id = draft_id(&replica);
    (replica, id)
}

fn invoke(action: &str) -> serde_json::Value {
    json!({"step": "invoke", "app_id": "so.tilo.notes",
           "resource": {"fixed": "notes.folder:inbox"}, "action": action})
}

#[test]
fn steps_are_added_into_blocks_with_readable_unique_ids() {
    let (mut replica, id) = with_draft();
    for step in [
        invoke("list"),
        invoke("list"),
        json!({"step": "if", "cond": {"truthy": {"literal": true}}, "then": []}),
    ] {
        replica
            .apply(Workbench::ADD_STEP.with((id.clone(), String::new(), step)))
            .unwrap();
    }
    replica
        .apply(Workbench::ADD_STEP.with((id.clone(), "2.then".into(), invoke("create"))))
        .unwrap();
    let program = program(&replica);
    let ids: Vec<_> = program.steps.iter().map(|step| step.id.as_str()).collect();
    assert_eq!(ids, ["list", "list2", "if"]);
    let StepKind::If { then, .. } = &program.steps[2].kind else {
        panic!("an if block")
    };
    assert_eq!(then[0].id, "create");
    assert!(
        replica
            .apply(Workbench::ADD_STEP.with((id, "0.then".into(), invoke("x"))))
            .is_err(),
        "an action step has no blocks"
    );
}

#[test]
fn arguments_bind_and_clear_and_steps_move_and_go() {
    let (mut replica, id) = with_draft();
    for action in ["list", "create"] {
        replica
            .apply(Workbench::ADD_STEP.with((id.clone(), String::new(), invoke(action))))
            .unwrap();
    }
    replica
        .apply(Workbench::SET_ARGUMENT.with((
            id.clone(),
            "1".into(),
            "title".into(),
            json!({"step_output": {"step": "list", "pointer": "/titles/0"}}).to_string(),
        )))
        .unwrap();
    let StepKind::Invoke { args, .. } = &program(&replica).steps[1].kind else {
        panic!("an action")
    };
    assert_eq!(args["title"], Binding::step("list", "/titles/0"));

    replica
        .apply(Workbench::SET_ARGUMENT.with((
            id.clone(),
            "1".into(),
            "title".into(),
            String::new(),
        )))
        .unwrap();
    let StepKind::Invoke { args, .. } = &program(&replica).steps[1].kind else {
        panic!("an action")
    };
    assert!(args.is_empty());

    replica
        .apply(Workbench::MOVE_STEP.with((id.clone(), "1".into(), true)))
        .unwrap();
    assert_eq!(program(&replica).steps[0].id, "create");
    assert!(
        replica
            .apply(Workbench::MOVE_STEP.with((id.clone(), "0".into(), true)))
            .is_err()
    );
    replica
        .apply(Workbench::REMOVE_STEP.with((id, "0".into())))
        .unwrap();
    let steps = program(&replica).steps;
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].id, "list");
}

#[test]
fn inputs_are_declared_once_and_a_saved_shortcut_reopens_as_a_draft() {
    let (mut replica, id) = with_draft();
    replica
        .apply(Workbench::ADD_INPUT.with((id.clone(), "folder".into(), "text".into())))
        .unwrap();
    assert!(
        replica
            .apply(Workbench::ADD_INPUT.with((id.clone(), "folder".into(), "text".into())))
            .is_err()
    );
    assert!(
        replica
            .apply(Workbench::ADD_INPUT.with((id.clone(), "n".into(), "colour".into())))
            .is_err()
    );
    let draft = replica.state().unwrap().drafts.get(&id).cloned().unwrap();
    let saved = draft.shortcut(Vec::new());
    assert_eq!(saved.name, "Tidy");
    assert_eq!(saved.inputs[0].name, "folder");

    replica
        .apply(Workbench::OPEN_SHORTCUT.with(serde_json::to_value(&saved).unwrap()))
        .unwrap();
    let reopened: Vec<_> = replica
        .state()
        .unwrap()
        .drafts
        .values()
        .filter(|draft| draft.replaces_saved)
        .cloned()
        .collect();
    assert_eq!(reopened.len(), 1);
    assert_eq!(reopened[0].shortcut_id, saved.id);
    assert_eq!(reopened[0].shortcut(Vec::new()), saved);
}

#[test]
fn concurrent_edits_to_different_parts_of_one_draft_all_survive() {
    let (mut here, id) = with_draft();
    for action in ["list", "create"] {
        here.apply(Workbench::ADD_STEP.with((id.clone(), String::new(), invoke(action))))
            .unwrap();
    }
    here.apply(Workbench::ADD_STEP.with((
        id.clone(),
        String::new(),
        json!({"step": "if", "cond": {"truthy": {"literal": true}}, "then": []}),
    )))
    .unwrap();
    let mut there = here.fork();

    // Here: bind an argument of the first step, add an input and a step
    // inside the branch.
    here.apply(Workbench::SET_ARGUMENT.with((
        id.clone(),
        "0".into(),
        "folder".into(),
        json!({"literal": "inbox"}).to_string(),
    )))
    .unwrap();
    here.apply(Workbench::ADD_INPUT.with((id.clone(), "who".into(), "text".into())))
        .unwrap();
    here.apply(Workbench::ADD_STEP.with((id.clone(), "2.then".into(), invoke("archive"))))
        .unwrap();
    // There: bind an argument of the second step, move it first, and bind
    // another argument of the first step.
    there
        .apply(Workbench::SET_ARGUMENT.with((
            id.clone(),
            "1".into(),
            "title".into(),
            json!({"literal": "Weekly"}).to_string(),
        )))
        .unwrap();
    there
        .apply(Workbench::MOVE_STEP.with((id.clone(), "1".into(), true)))
        .unwrap();
    there
        .apply(Workbench::SET_ARGUMENT.with((
            id,
            "1".into(),
            "limit".into(),
            json!({"literal": 3}).to_string(),
        )))
        .unwrap();

    here.sync_with(&mut there).unwrap();
    let merged = program(&here);
    assert_eq!(merged, program(&there), "both replicas converge");
    let ids: Vec<_> = merged.steps.iter().map(|step| step.id.as_str()).collect();
    assert_eq!(ids, ["create", "list", "if"], "the move survives");
    let StepKind::Invoke { args, .. } = &merged.steps[0].kind else {
        panic!("an action")
    };
    assert_eq!(args["title"], Binding::Literal(json!("Weekly")));
    let StepKind::Invoke { args, .. } = &merged.steps[1].kind else {
        panic!("an action")
    };
    assert_eq!(
        args.keys().map(String::as_str).collect::<Vec<_>>(),
        ["folder", "limit"],
        "two people binding arguments of the same step both keep theirs"
    );
    let StepKind::If { then, .. } = &merged.steps[2].kind else {
        panic!("an if block")
    };
    assert_eq!(then[0].id, "archive");
    assert_eq!(merged.inputs[0].name, "who");
}

#[test]
fn morning_refresh_draft_is_inert_until_person_chooses_schedule_and_saves() {
    let mut replica = Replica::<Workbench>::new();
    replica.apply(Workbench::DRAFT_SHORTCUT.with(json!({
        "id": "injected", "name": "Refresh chosen source", "steps": [{"id": "refresh", "step": "invoke", "app_id": "so.tilo.weather", "resource": {"fixed": "weather.location:fixture"}, "action": "refresh"}],
        "triggers": [{"trigger": "schedule", "schedule": {"every": "interval", "seconds": 60}}], "saved_by": "someone-else"
    }))).unwrap();
    let id = draft_id(&replica);
    assert!(program(&replica).triggers.is_empty());
    assert!(program(&replica).consents.is_empty());
    assert!(
        !replica
            .state()
            .unwrap()
            .drafts
            .values()
            .next()
            .unwrap()
            .replaces_saved
    );
    assert_ne!(
        replica
            .state()
            .unwrap()
            .drafts
            .values()
            .next()
            .unwrap()
            .shortcut_id,
        "injected"
    );
    replica
        .apply(Workbench::SET_SCHEDULE.with((id.clone(), true, 9, 15)))
        .unwrap();
    assert_eq!(
        program(&replica).triggers,
        vec![tilo_automation::TriggerSpec::Schedule {
            schedule: tilo_automation::Schedule::Calendar {
                at: tilo_automation::TimeOfDay {
                    hour: 9,
                    minute: 15
                },
                weekdays: Vec::new()
            },
            catch_up: tilo_automation::CatchUp::RunOnce
        }]
    );
    assert!(
        replica
            .apply(Workbench::SET_SCHEDULE.with((id.clone(), true, 24, 0)))
            .is_err()
    );
    replica
        .apply(Workbench::SET_SCHEDULE.with((id, false, 9, 15)))
        .unwrap();
    assert!(program(&replica).triggers.is_empty());
}

#[test]
fn morning_refresh_schedule_edits_preserve_other_saved_triggers() {
    use tilo_automation::{CatchUp, Schedule, TimeOfDay, TriggerSpec, Weekday};
    let (replica, id) = with_draft();
    let mut saved = replica
        .state()
        .unwrap()
        .drafts
        .get(&id)
        .unwrap()
        .shortcut(Vec::new());
    let existing = vec![
        TriggerSpec::Schedule {
            schedule: Schedule::Interval { seconds: 3600 },
            catch_up: CatchUp::RunOnce,
        },
        TriggerSpec::Schedule {
            schedule: Schedule::Calendar {
                at: TimeOfDay {
                    hour: 10,
                    minute: 30,
                },
                weekdays: vec![Weekday::Monday],
            },
            catch_up: CatchUp::RunOnce,
        },
        TriggerSpec::Keyboard {
            chord: "Super+R".into(),
        },
    ];
    saved.triggers = existing.clone();
    let mut reopened = Replica::<Workbench>::new();
    reopened
        .apply(Workbench::OPEN_SHORTCUT.with(serde_json::to_value(saved).unwrap()))
        .unwrap();
    let id = draft_id(&reopened);
    reopened
        .apply(Workbench::SET_SCHEDULE.with((id.clone(), true, 9, 15)))
        .unwrap();
    assert_eq!(program(&reopened).triggers[..existing.len()], existing);
    reopened
        .apply(Workbench::SET_SCHEDULE.with((id, false, 9, 15)))
        .unwrap();
    assert_eq!(program(&reopened).triggers, existing);
}
