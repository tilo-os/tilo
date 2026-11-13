use std::sync::Arc;

use automerge::AutoCommit;
use serde_json::json;
use tilo_actions::{ActionKind, ActionRegistry};
use tilolang_runtime::{Meta, SourceWords};
use tilolang_tilo::action::{ACTION_TYPE, DynAction, register_actions};
use tilolang_tilo::publish::{
    ActionTypes, PlanRequest, Planner, publish_package, publish_record, resource_id,
};
use tilolang_tilo::record::DynValue;
use tilolang_tilo::surface::{HOST_KIND, host_surface, package_surfaces, surface_toml};

use crate::fixtures::{COUNTER, Replica, TASKS_SOURCE, TODO, field_id};

fn counter() -> Meta {
    let (_, packages) = Replica::new("ada", AutoCommit::new(), &[(COUNTER, "")]);
    packages[0].meta.clone()
}

#[test]
fn the_surface_names_a_kind_per_type_and_a_verb_per_action() {
    let meta = counter();
    let surfaces = package_surfaces(&meta);
    let kinds: Vec<&str> = surfaces
        .iter()
        .map(|surface| surface.kind.as_str())
        .collect();
    assert_eq!(
        kinds,
        ["tl.so.tilo.lang.counter.Counter", "tl.so.tilo.lang.counter"]
    );
    let increment = &meta.actions[0];
    let counter = &surfaces[0];
    // By the action's id, the one its changes' claims name: its name is what the door offers
    // it by, not what a record keeps.
    assert_eq!(counter.verb(&increment.id).unwrap().0, ActionKind::Undoable);
    assert!(counter.verb(&increment.name).is_none());
    // The record parameter is the resource itself, so the verb takes none.
    assert!(
        counter
            .verbs
            .iter()
            .find(|verb| verb.id == increment.id)
            .unwrap()
            .params
            .is_empty()
    );
    assert!(counter.verb("edit-text").is_some() && counter.verb("edit-flag").is_some());
}

/// A package's root is created by the host on its own, as a committed change of its own: the
/// package's surface publishes the verb that change names, or every receiver (and the identity
/// service signing it) refuses the app's first change in a real session.
#[test]
fn the_root_the_host_creates_names_a_verb_the_surface_publishes() {
    let (replica, packages) = Replica::new("ada", AutoCommit::new(), &[(COUNTER, "")]);
    let meta = &packages[0].meta;
    let first = replica.host.next_ticket();
    replica.host.root_of(&meta.app).unwrap();
    let committed = replica.host.take_committed_from(first);
    assert!(!committed.is_empty(), "creating the root commits a change");
    let surfaces = package_surfaces(meta);
    for entry in committed {
        let action = &entry.action;
        let surface = surfaces
            .iter()
            .find(|surface| surface.kind == action.kind)
            .unwrap_or_else(|| panic!("no surface of kind {}", action.kind));
        assert_eq!(
            surface.verb(&action.action_id).map(|verb| verb.0),
            Some(ActionKind::Undoable),
            "{} publishes no {}",
            action.kind,
            action.action_id
        );
    }
}

/// The key of the node bound to the field `field` in `node`'s tree.
fn bound_to(node: &tilolang_runtime::UiNode, field: &str) -> Option<String> {
    if node
        .binding
        .as_ref()
        .and_then(|binding| binding.field())
        .is_some_and(|(_, bound)| bound == field)
    {
        return Some(node.key.clone());
    }
    node.children
        .iter()
        .find_map(|child| bound_to(child, field))
}

/// Typing into a bound text field commits a change of its own: the package's surface publishes
/// the verb that change names, or the identity service refuses to sign the app's first
/// keystroke and the app's collaboration ends there.
#[test]
fn a_bound_fields_edit_names_a_verb_the_surface_publishes() {
    let (mut replica, packages) = Replica::new("ada", AutoCommit::new(), &[(TODO, "")]);
    let meta = packages[0].meta.clone();
    replica.host.root_of(&meta.app).unwrap();
    replica.commit();
    let view = replica.view(&meta.app);
    let title = crate::fixtures::field_id(&meta, "TodoList", "title");
    let input = bound_to(view.tree(), &title).expect("the list's title is bound");
    let first = replica.host.next_ticket();
    view.edit(&input, &tilolang_runtime::value::Value::text("Groceries"))
        .unwrap();
    // The keystroke is a change of the record as it is typed, and the change it queues is what
    // the workspace commits.
    let committed = replica.host.take_committed_from(first);
    assert_eq!(committed.len(), 1, "the keystroke commits one change");
    let surfaces = package_surfaces(&meta);
    let action = &committed[0].action;
    let surface = surfaces
        .iter()
        .find(|surface| surface.kind == action.kind)
        .unwrap_or_else(|| panic!("no surface of kind {}", action.kind));
    assert_eq!(
        surface.verb(&action.action_id).map(|verb| verb.0),
        Some(ActionKind::Undoable),
        "{} publishes no {}",
        action.kind,
        action.action_id
    );
    // What was typed is the edit the change records, never the text it leaves: the change names
    // the field, and its edit carries the words.
    assert_eq!(
        action.args,
        json!({"field": title}),
        "a keystroke's change names the field it edits"
    );
}

#[test]
fn collection_fields_are_listed_below_their_item() {
    let (_, packages) = Replica::new("ada", AutoCommit::new(), &[(TODO, "")]);
    let meta = &packages[0].meta;
    let surfaces = package_surfaces(meta);
    let list = surfaces
        .iter()
        .find(|surface| surface.kind.ends_with(".TodoList"))
        .unwrap();
    let items = meta
        .types
        .iter()
        .find(|t| t.name == "TodoList")
        .unwrap()
        .fields[1]
        .id
        .clone();
    assert!(
        list.fields
            .iter()
            .any(|f| f.path == items && f.stored == "collection")
    );
    assert!(
        list.fields
            .iter()
            .any(|f| f.path.starts_with(&format!("{items}.*.")))
    );
}

#[test]
fn the_surface_file_parses() {
    let meta = counter();
    let bytes = surface_toml(&[&meta]).unwrap();
    let value: toml::Value = toml::from_str(std::str::from_utf8(&bytes).unwrap()).unwrap();
    let kinds: Vec<&str> = value["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|resource| resource["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds[0], HOST_KIND);
    assert!(kinds.contains(&"tl.so.tilo.lang.counter.Counter"));
}

/// A planner that records what it was asked and plans nothing.
struct Recording;

impl Planner for Recording {
    fn plan(&self, request: &PlanRequest<'_>) -> Result<DynAction, String> {
        Ok(DynAction {
            app: request.app.to_owned(),
            kind: request.kind.to_owned(),
            action_id: request.action_id.to_owned(),
            args: json!({"resource": request.resource, "args": request.args}),
            authorship: tilolang_tilo::Authorship::default(),
            edits: Vec::new(),
        })
    }
}

#[test]
fn a_published_action_builds_the_planned_dyn_action() {
    let meta = counter();
    let planner: Arc<dyn Planner> = Arc::new(Recording);
    let resource = publish_record(
        &ActionTypes::new(&meta, &planner, &SourceWords),
        "Counter",
        "c1",
        "Counter",
    );
    assert_eq!(
        resource.id.as_str(),
        resource_id("so.tilo.lang.counter", "c1")
    );
    assert_eq!(resource.kind, "tl.so.tilo.lang.counter.Counter");
    let increment = resource
        .actions
        .iter()
        .find(|spec| spec.id == meta.actions[0].name)
        .unwrap();
    assert_eq!(increment.contract.kind, ActionKind::Undoable);
    let built = increment.build_json(&serde_json::Map::new()).unwrap();
    assert_eq!(built.type_id, ACTION_TYPE);
    assert!(
        ActionRegistry::with(register_actions)
            .decode(&built)
            .is_ok()
    );
    assert!(
        publish_package(&meta, &planner, &SourceWords)
            .actions
            .is_empty()
    );
}

/// A parameter of another app's record type (`list: todo.TodoList` in a package that uses the
/// todo app) picks a resource of that kind, as one of the package's own records does: the door
/// takes the catalogue's reference to it. Published as an array, no caller could pass one.
#[test]
fn a_parameter_of_another_apps_record_picks_a_resource_of_its_kind() {
    const BOARD: &str = r#"app so.tilo.board "Board"

use so.tilo.todo as todo

type Board {
  list: Option<todo.TodoList>
}

root Board

default view main for Board

view main(b: Board) {
  page {
    text("Board")
  }
}

action attach(b: Board, list: todo.TodoList) {
  b.list = .some(list)
}
"#;
    let (_, packages) = Replica::new("ada", AutoCommit::new(), &[(TODO, ""), (BOARD, "")]);
    let meta = &packages[1].meta;
    let planner: Arc<dyn Planner> = Arc::new(Recording);
    let resource = publish_record(
        &ActionTypes::new(meta, &planner, &SourceWords),
        "Board",
        "b1",
        "Board",
    );
    let attach = meta.actions.iter().find(|a| a.name == "attach").unwrap();
    let spec = resource
        .actions
        .iter()
        .find(|spec| spec.id == attach.name)
        .unwrap();
    let list = spec
        .params()
        .iter()
        .find(|param| &*param.name == "list")
        .unwrap();
    assert_eq!(
        list.pick(),
        Some(tilo_actions::Pick::Resource {
            kind: "tl.so.tilo.todo.TodoList".to_owned()
        })
    );
}

#[test]
fn the_host_surface_declares_the_commit() {
    let surface = host_surface();
    assert_eq!(surface.kind, HOST_KIND);
    assert_eq!(surface.verb("commit").unwrap().0, ActionKind::Undoable);
}

// USER REQUIREMENT (2026-10-20): scenarios are seed data for shots,
// tests and benchmarks; "they never touch a person's data", so their actions are never offered.
#[test]
fn a_scenarios_action_is_never_published_or_on_a_surface() {
    let source = format!(
        "{TODO}\nscenario \"a full list\" {{\n  let l = root TodoList\n  add(l, \"milk\")\n}}\n"
    );
    let (_, packages) = Replica::new("ada", AutoCommit::new(), &[(&source, "")]);
    let meta = &packages[0].meta;
    let scenario = meta
        .actions
        .iter()
        .find(|action| action.scenario)
        .expect("the scenario's action is in the metadata");
    assert_eq!(meta.scenarios[0].action, scenario.id);
    for surface in package_surfaces(meta) {
        assert!(surface.verb(&scenario.id).is_none(), "{}", surface.kind);
    }
    let planner: Arc<dyn Planner> = Arc::new(Recording);
    let package = publish_package(meta, &planner, &SourceWords);
    assert!(package.actions.iter().all(|spec| spec.id != scenario.id));
}

/// An action's requirement is the app's to answer, and the app answers it for the one resource
/// a caller names: a door planning the action against an item whose rule does not hold is
/// refused with the rule's reason, where before the answer had to be published for every item
/// of every record.
#[test]
fn a_requirement_refuses_the_invocation_whose_rule_does_not_hold() {
    const CHORES: &str = r#"app so.tilo.chores "Chores"

    type Task {
      title: Text merge text
      done: Bool
    }

    type Tasks {
      tasks: Collection<Task>
    }

    action add(t: Tasks, title: Text) { t.tasks.insert(Task(title: title, done: false)) }

    action finish(t: Task) { t.done = true }

    action due(t: Task) (label: "Due") {
      require not t.done "It is done already"
      t.title = t.title
    }

    root Tasks

    default view main for Tasks

    view main(t: Tasks) {
      page {
        for task in t.tasks {
          text(task.title)
        }
      }
    }
    "#;
    let (mut replica, packages) = Replica::new("ada", AutoCommit::new(), &[(CHORES, "")]);
    let meta = packages[0].meta.clone();
    let app = meta.app.clone();
    replica.host.root_of(&app).unwrap();
    replica.commit();
    let root = replica.host.root_of(&app).unwrap().doc;
    let plan = |replica: &mut Replica, name: &str, at: Option<&str>, args: serde_json::Value| {
        let action = meta.actions.iter().find(|a| a.name == name).unwrap();
        let args = args.as_object().cloned().unwrap_or_default();
        replica.host.plan(&PlanRequest {
            app: &app,
            kind: "tl.so.tilo.chores.Tasks",
            action_id: &action.id,
            resource: at,
            args: &args,
            caller: None,
            from: None,
        })
    };
    for title in ["milk", "bread"] {
        let planned = plan(&mut replica, "add", Some(&root), json!({ "title": title })).unwrap();
        replica.apply(&planned);
    }
    let tasks = field_id(&meta, "Tasks", "tasks");
    let tasks_ty = meta.types.iter().find(|t| t.name == "Tasks").unwrap();
    let item_ty = meta.types.iter().find(|t| t.name == "Task").unwrap();
    let ids: Vec<String> = match replica.root(&app).get(&tasks) {
        Some(DynValue::Collection(items)) => {
            items.iter().map(|(id, _)| id.as_str().to_owned()).collect()
        }
        other => panic!("no tasks: {other:?}"),
    };
    let items: Vec<String> = ids
        .iter()
        .map(|id| {
            tilolang_runtime::value::ResourceRef::root(&root, tasks_ty.id.clone())
                .item(id, item_ty.id.clone())
                .id_string()
        })
        .collect();
    let first = items[0].clone();
    let finished = plan(&mut replica, "finish", Some(&first), json!({})).unwrap();
    replica.apply(&finished);
    let due = meta.actions.iter().find(|a| a.name == "due").unwrap();
    assert_eq!(
        replica
            .host
            .availability(&resource_id(&meta.app, &first), &due.name),
        Some(tilo_actions::Availability::Unavailable {
            code: "unmet".to_owned(),
            reason: "It is done already".to_owned(),
        }),
        "the app says of the item that its rule does not hold"
    );
    let refused = plan(&mut replica, "due", Some(&first), json!({}));
    let Err(why) = &refused else {
        panic!("the item is due only while it is not done: {refused:?}");
    };
    assert!(why.contains("It is done already"), "{why}");
    let open = items[1].clone();
    assert_eq!(
        replica
            .host
            .availability(&resource_id(&meta.app, &open), &due.name),
        Some(tilo_actions::Availability::Available)
    );
    plan(&mut replica, "due", Some(&open), json!({}))
        .expect("the rule holds for the task that is not done");
}

/// A requirement over an option field the app leaves unset: `t.due != .none` does not hold for a
/// task with no day, so the action that clears the day is unavailable, and holds once a day is
/// set.
#[test]
fn a_requirement_over_an_unset_option_field_reads_the_field_as_none() {
    const PLAN: &str = r#"app so.tilo.plan "Plan"

    type Task {
      title: Text merge text
      due: Option<Date> = .none
    }

    type Tasks {
      tasks: Collection<Task>
    }

    action add(t: Tasks, title: Text) { t.tasks.insert(Task(title: title, due: .none)) }

    action set_day(t: Task, day: Option<Date>) { t.due = day }

    action clear_day(t: Task) (label: "No date") {
      require t.due != .none "It has no date"
      t.due = .none
    }

    root Tasks

    default view main for Tasks

    view main(t: Tasks) {
      page {
        for task in t.tasks {
          text(task.title)
        }
      }
    }
    "#;
    let (mut replica, packages) = Replica::new("ada", AutoCommit::new(), &[(PLAN, "")]);
    let meta = packages[0].meta.clone();
    let app = meta.app.clone();
    replica.host.root_of(&app).unwrap();
    replica.commit();
    let root = replica.host.root_of(&app).unwrap().doc;
    let clear = meta
        .actions
        .iter()
        .find(|action| action.name == "clear_day")
        .unwrap();
    let plan = |replica: &mut Replica, name: &str, at: &str, args: serde_json::Value| {
        let action = meta.actions.iter().find(|a| a.name == name).unwrap();
        let args = args.as_object().cloned().unwrap_or_default();
        replica.host.plan(&PlanRequest {
            app: &app,
            kind: "tl.so.tilo.plan.Tasks",
            action_id: &action.id,
            resource: Some(at),
            args: &args,
            caller: None,
            from: None,
        })
    };
    let planned = plan(&mut replica, "add", &root, json!({ "title": "milk" })).unwrap();
    replica.apply(&planned);
    let items_field = field_id(&meta, "Tasks", "tasks");
    let root_ty = meta.types.iter().find(|t| t.name == "Tasks").unwrap();
    let task_ty = meta.types.iter().find(|t| t.name == "Task").unwrap();
    let id = match replica.root(&app).get(&items_field) {
        Some(DynValue::Collection(items)) => {
            let (first, _) = items.iter().next().unwrap();
            first.as_str().to_owned()
        }
        other => panic!("no tasks: {other:?}"),
    };
    let item = tilolang_runtime::value::ResourceRef::root(&root, root_ty.id.clone())
        .item(&id, task_ty.id.clone())
        .id_string();
    assert_eq!(
        replica
            .host
            .availability(&resource_id(&meta.app, &item), &clear.name),
        Some(tilo_actions::Availability::Unavailable {
            code: "unmet".to_owned(),
            reason: "It has no date".to_owned(),
        }),
        "a task with no day has none to clear"
    );
    let day = plan(
        &mut replica,
        "set_day",
        &item,
        json!({ "day": "2026-11-10" }),
    )
    .unwrap();
    replica.apply(&day);
    assert_eq!(
        replica
            .host
            .availability(&resource_id(&meta.app, &item), &clear.name),
        Some(tilo_actions::Availability::Available),
        "a task due by a day has one to clear"
    );
}

/// The Tasks app's own clearing action over a task the entry made: a task typed with no day has
/// none to clear, so the app says the action cannot run.
#[test]
fn the_tasks_app_says_a_task_with_no_day_has_none_to_clear() {
    let (mut replica, mut packages) = Replica::new(
        "ada",
        AutoCommit::new(),
        &[
            (
                include_str!("../../../../apps/tl/discussion/app.tl"),
                include_str!("../../../../apps/tl/discussion/app.ids"),
            ),
            (
                include_str!("../../../../apps/tl/chat/app.tl"),
                include_str!("../../../../apps/tl/chat/app.ids"),
            ),
            (
                include_str!("../../../../apps/tl/notes/app.tl"),
                include_str!("../../../../apps/tl/notes/app.ids"),
            ),
            (
                TASKS_SOURCE,
                include_str!("../../../../apps/tl/tasks/app.ids"),
            ),
        ],
    );
    let meta = packages.pop().unwrap().meta;
    let app = meta.app.clone();
    replica.host.root_of(&app).unwrap();
    replica.commit();
    let root = replica.host.root_of(&app).unwrap().doc;
    let plan = |replica: &mut Replica, name: &str, at: &str, args: serde_json::Value| {
        let action = meta.actions.iter().find(|a| a.name == name).unwrap();
        let args = args.as_object().cloned().unwrap_or_default();
        replica.host.plan(&PlanRequest {
            app: &app,
            kind: "tl.so.tilo.tasks.Tasks",
            action_id: &action.id,
            resource: Some(at),
            args: &args,
            caller: None,
            from: None,
        })
    };
    let planned = plan(
        &mut replica,
        "new_task",
        &root,
        json!({ "title": "Water the plants" }),
    )
    .unwrap();
    replica.apply(&planned);
    let items_field = field_id(&meta, "Tasks", "items");
    let root_ty = meta.types.iter().find(|t| t.name == "Tasks").unwrap();
    let task_ty = meta.types.iter().find(|t| t.name == "Task").unwrap();
    let id = match replica.root(&app).get(&items_field) {
        Some(DynValue::Collection(items)) => {
            let (first, _) = items.iter().next().unwrap();
            first.as_str().to_owned()
        }
        other => panic!("no tasks: {other:?}"),
    };
    let item = tilolang_runtime::value::ResourceRef::root(&root, root_ty.id.clone())
        .item(&id, task_ty.id.clone())
        .id_string();
    let clear = meta
        .actions
        .iter()
        .find(|action| action.name == "due_clear")
        .unwrap();
    assert_eq!(
        replica
            .host
            .availability(&resource_id(&meta.app, &item), &clear.name),
        Some(tilo_actions::Availability::Unavailable {
            code: "unmet".to_owned(),
            reason: "It has no date".to_owned(),
        }),
        "a task typed with no day has none to clear"
    );
}

/// A native app's read action is an action of its record in the catalogue,
/// typed by the value the surface publishes it answers with.
#[test]
fn a_read_actions_answer_is_typed_by_the_surface() {
    let text = r#"
[[resources]]
kind = "pdf.document"
label = "PDF"
version = 1
fields = [
  { path = "source", type = "text", readonly = true },
]

[[resources.verbs]]
id = "read-form-fields"
kind = "read"
params = []
returns = { type = { named = "FormFields" }, schema = { type = "object" } }

[[resources.types]]
name = "FormFields"
fields = [
  { name = "file", type = "text" },
  { name = "fields", type = { list = { named = "FormField" } } },
]

[[resources.types]]
name = "FormField"
fields = [{ name = "name", type = "text" }]
"#;
    let meta = tilolang_tilo::catalogue::described("so.tilo.pdf-viewer", "PDF Viewer", text)
        .expect("the surface reads as a package");
    let read = meta
        .actions
        .iter()
        .find(|action| action.name == "read_form_fields")
        .expect("the read action is published");
    assert_eq!(read.result.as_deref(), Some("FormFields"), "{read:?}");
    assert_eq!(read.class, tilolang::types::EffectClass::Read);
    let form = meta
        .types
        .iter()
        .find(|ty| ty.name == "FormFields")
        .expect("the answer's record is a type");
    let fields: Vec<(&str, &str)> = form
        .fields
        .iter()
        .map(|field| (field.name.as_str(), field.ty.as_str()))
        .collect();
    assert_eq!(fields, [("file", "Text"), ("fields", "List<FormField>")]);
}
