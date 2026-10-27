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
    replica.apply(Workbench::NEW_DRAFT.with("Tidy")).unwrap();
    let id = draft_id(&replica);
    (replica, id)
}

fn invoke(action: &str) -> serde_json::Value {
    json!({"step": "invoke", "action": {"app": "so.tilo.notes", "kind": "notes.folder", "action": action},
           "resource": {"fixed": "notes.folder:inbox"}})
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
        .apply(Workbench::BIND_ARGUMENT.with((
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
        .apply(Workbench::BIND_ARGUMENT.with((
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
fn editing_a_saved_shortcut_preserves_per_action_appearance() {
    let (mut replica, id) = with_draft();
    let mut step = invoke("create");
    step["appearance"] = json!({"literal": {"name": "Example bridge"}});
    replica
        .apply(Workbench::ADD_STEP.with((id.clone(), String::new(), step.clone())))
        .unwrap();
    let saved = replica
        .state()
        .unwrap()
        .drafts
        .get(&id)
        .unwrap()
        .shortcut(Vec::new());
    let expected: StepKind = serde_json::from_value(step).unwrap();
    assert_eq!(saved.steps[0].kind, expected);

    replica
        .apply(Workbench::OPEN_SHORTCUT.with(serde_json::to_value(&saved).unwrap()))
        .unwrap();
    let reopened = replica
        .state()
        .unwrap()
        .drafts
        .iter()
        .find(|(_, draft)| draft.replaces_saved)
        .map(|(id, _)| id.clone())
        .unwrap();
    replica
        .apply(Workbench::BIND_ARGUMENT.with((
            reopened.clone(),
            "0".into(),
            "title".into(),
            serde_json::to_string(&Binding::Literal(json!("Updated title"))).unwrap(),
        )))
        .unwrap();
    let edited = replica
        .state()
        .unwrap()
        .drafts
        .get(&reopened)
        .unwrap()
        .shortcut(Vec::new());
    let StepKind::Invoke {
        appearance,
        args: fields,
        ..
    } = &edited.steps[0].kind
    else {
        panic!("expected the saved action");
    };
    assert_eq!(
        appearance,
        &Some(Binding::Literal(json!({"name": "Example bridge"})))
    );
    assert_eq!(
        fields.get("title"),
        Some(&Binding::Literal(json!("Updated title")))
    );
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
    here.apply(Workbench::BIND_ARGUMENT.with((
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
        .apply(Workbench::BIND_ARGUMENT.with((
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
        .apply(Workbench::BIND_ARGUMENT.with((
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
fn a_step_stores_the_key_of_the_action_it_invokes() {
    let (mut replica, id) = with_draft();
    replica
        .apply(Workbench::ADD_STEP.with((id, String::new(), invoke("list"))))
        .unwrap();
    let StepKind::Invoke { action, .. } = &program(&replica).steps[0].kind else {
        panic!("an action step")
    };
    assert_eq!(
        *action,
        ui::tilo_actions::ActionKey::new("so.tilo.notes", "notes.folder", "list")
    );
}

#[test]
fn the_name_and_output_are_field_edits_and_history_names_the_draft() {
    let (mut replica, id) = with_draft();
    let draft = Workbench::paths().drafts.item(&id);
    replica
        .apply(Invoke::edit(&draft.name.replace("Inbox digest")).unwrap())
        .unwrap();
    replica
        .apply(Workbench::ADD_STEP.with((id.clone(), String::new(), invoke("list"))))
        .unwrap();
    let output = Binding::StepOutput {
        step: "list".into(),
        pointer: String::new(),
    };
    replica
        .apply(Invoke::edit(&draft.output.set(Some(output.clone()))).unwrap())
        .unwrap();
    assert_eq!(program(&replica).output, Some(output));
    replica
        .apply(Workbench::REMOVE_STEP.with((id.clone(), "0".into())))
        .unwrap();
    replica.apply(Workbench::DISCARD.with(id)).unwrap();
    let catalogue = tilo_history::Catalogue::new("en-US", &[include_str!("../locales/en-US.ftl")]);
    let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
    let sentences: Vec<String> = replica
        .versions()
        .into_iter()
        .filter_map(|version| version.context)
        .filter(|entry| entry.what.is_some())
        .map(|entry| reader.sentence(&entry))
        .collect();
    assert_eq!(
        sentences,
        [
            "Started the shortcut Tidy",
            "Renamed the shortcut Tidy",
            "Added a step to Inbox digest",
            "Changed what Inbox digest reports back",
            "Removed step 0 from Inbox digest",
            "Discarded the draft Inbox digest",
        ]
    );
}

#[test]
fn morning_refresh_draft_is_inert_until_person_chooses_schedule_and_saves() {
    let mut replica = Replica::<Workbench>::new();
    replica.apply(Workbench::DRAFT_SHORTCUT.with(json!({
        "id": "injected", "name": "Refresh chosen source", "steps": [{"id": "refresh", "step": "invoke", "action": {"app": "so.tilo.weather", "kind": "weather.location", "action": "refresh"}, "resource": {"fixed": "weather.location:fixture"}}],
        "triggers": [{"trigger": "schedule", "schedule": {"every": "interval", "seconds": 60}}], "saved_by": "someone-else"
    }))).unwrap();
    let id = draft_id(&replica);
    assert!(program(&replica).triggers.is_empty());
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

/// A workbench of version 1 kept each input, step head, argument, the output
/// and the triggers as one JSON string. Opened now, every draft holds the
/// program it held, with every step, binding and trigger.
#[test]
fn a_workbench_of_version_one_opens_with_every_draft_program() {
    use ui::automerge::{ObjType, ROOT, ReadDoc as _, ScalarValue, Value};
    use ui::tilo_resource::{Doc, ModelError, Slot, evolution, read_key};

    /// Store the value at `key` of `obj` as the JSON text version 1 wrote.
    fn as_text<T: Model + serde::Serialize>(doc: &mut Doc, obj: &ui::automerge::ObjId, key: &str) {
        let value: T = read_key(doc, obj, key).unwrap();
        let slot = Slot::Key(obj, key);
        slot.remove(doc).unwrap();
        slot.put_scalar(
            doc,
            ScalarValue::Str(serde_json::to_string(&value).unwrap().into()),
        )
        .unwrap();
    }

    fn map_at(
        doc: &Doc,
        obj: &ui::automerge::ObjId,
        key: &str,
    ) -> Result<ui::automerge::ObjId, ModelError> {
        match doc.get(obj, key)? {
            Some((Value::Object(ObjType::Map), map)) => Ok(map),
            _ => Err(ModelError::shape("a map")),
        }
    }

    let mut replica = Replica::<Workbench>::new();
    replica
        .apply(Workbench::OPEN_SHORTCUT.with(json!({
            "id": "s1", "name": "File receipts",
            "inputs": [{"name": "folder", "ty": "text", "required": true, "description": "Where to file"}],
            "steps": [
                {"id": "list", "step": "invoke",
                 "action": {"app": "so.tilo.notes", "kind": "notes.folder", "action": "list"},
                 "resource": {"from_binding": {"input": "folder"}},
                 "args": {"limit": {"literal": {"count": 5, "sort": ["date", "desc"]}}}},
                {"id": "any", "step": "if",
                 "cond": {"not": {"all": [{"truthy": {"step_output": {"step": "list", "pointer": "/0"}}},
                                         {"equals": {"left": {"var": "x"}, "right": {"literal": null}}}]}},
                 "then": [{"id": "make", "step": "invoke",
                           "action": {"app": "so.tilo.notes", "kind": "notes.folder", "action": "create"},
                           "resource": {"fixed": "notes.folder:inbox"},
                           "args": {"title": {"template": "Receipts {{input.folder}}"}}}]}
            ],
            "output": {"step_output": {"step": "list"}},
            "grant": [],
            "triggers": [{"trigger": "keyboard", "chord": "Super+Shift+R"},
                         {"trigger": "schedule", "schedule": {"every": "interval", "seconds": 3600}}]
        })))
        .unwrap();
    let expected = program(&replica);
    assert_eq!(expected.steps.len(), 2);
    assert_eq!(expected.triggers.len(), 2);
    assert!(expected.output.is_some());

    let doc = replica.doc_mut();
    for draft in evolution::collection_items(doc, &ROOT, "drafts").unwrap() {
        as_text::<Option<Binding>>(doc, &draft, "output");
        as_text::<Vec<tilo_automation::TriggerSpec>>(doc, &draft, "triggers");
        let inputs = map_at(doc, &draft, "inputs").unwrap();
        for id in doc.keys(&inputs).collect::<Vec<_>>() {
            let entry = map_at(doc, &inputs, &id).unwrap();
            as_text::<Option<ActionParam>>(doc, &entry, "v");
        }
        for step in evolution::collection_items(doc, &draft, "steps").unwrap() {
            as_text::<Option<Head>>(doc, &step, "head");
            if let Ok(args) = map_at(doc, &step, "args") {
                for name in doc.keys(&args).collect::<Vec<_>>() {
                    as_text::<Option<Binding>>(doc, &args, &name);
                }
            }
        }
    }
    Slot::Key(&ROOT, evolution::VERSION_KEY)
        .put_scalar(doc, ScalarValue::Uint(1))
        .unwrap();
    doc.commit();
    let mut old = doc.fork();
    assert!(
        evolution::read_checked::<Workbench>(&old).is_err(),
        "unmigrated"
    );
    assert!(evolution::migrate::<Workbench>(&mut old).unwrap());
    let workbench = evolution::read_checked::<Workbench>(&old).unwrap();
    let draft = workbench.drafts.values().next().unwrap();
    assert_eq!(draft.program(), expected);
    assert_eq!(draft.name, "File receipts");
    assert!(draft.replaces_saved);
}

/// New drafts that would share a name are numbered, as new layers and
/// sheets are, so the list tells them apart.
#[test]
fn new_drafts_with_one_name_are_numbered() {
    let mut replica = Replica::<Workbench>::new();
    for _ in 0..3 {
        replica
            .apply(Workbench::NEW_DRAFT.with("New shortcut"))
            .unwrap();
    }
    replica.apply(Workbench::NEW_DRAFT.with("Tidy")).unwrap();
    let mut names: Vec<String> = replica
        .state()
        .unwrap()
        .drafts
        .values()
        .map(|draft| draft.name.clone())
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["New shortcut", "New shortcut 2", "New shortcut 3", "Tidy"]
    );
}

#[test]
fn a_draft_or_a_saved_shortcut_duplicates_into_a_new_unsaved_variant() {
    let (mut replica, id) = with_draft();
    replica
        .apply(Workbench::ADD_STEP.with((id.clone(), String::new(), invoke("list"))))
        .unwrap();
    let original = replica.state().unwrap().drafts.get(&id).cloned().unwrap();
    replica
        .apply(Workbench::DUPLICATE_DRAFT.with(id.clone()))
        .unwrap();
    let copy = replica
        .state()
        .unwrap()
        .drafts
        .iter()
        .find(|(key, draft)| **key != id && draft.name == "Tidy copy")
        .map(|(_, draft)| draft.clone())
        .expect("a duplicate of the draft");
    assert_eq!(copy.program(), original.program());
    assert_ne!(copy.shortcut_id, original.shortcut_id);
    assert!(!copy.replaces_saved);
    replica
        .apply(Workbench::DUPLICATE_DRAFT.with(id.clone()))
        .unwrap();
    assert!(
        replica
            .state()
            .unwrap()
            .drafts
            .values()
            .any(|draft| draft.name == "Tidy copy 2"),
        "duplicates are numbered so they can be told apart"
    );

    let saved = original.shortcut(Vec::new());
    replica
        .apply(Workbench::DUPLICATE_SHORTCUT.with(serde_json::to_value(&saved).unwrap()))
        .unwrap();
    let duplicated = replica
        .state()
        .unwrap()
        .drafts
        .values()
        .find(|draft| draft.name == "Tidy copy 3" && !draft.replaces_saved)
        .cloned()
        .expect("a duplicate of the saved shortcut");
    assert_eq!(duplicated.program(), original.program());
    assert_ne!(duplicated.shortcut_id, saved.id);
}
