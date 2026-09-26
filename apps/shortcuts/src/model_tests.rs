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
        .program
        .0
        .clone()
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
