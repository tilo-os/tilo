use std::sync::Arc;

use crate::fixtures::Workspace;
use serde_json::json;
use tilo_actions::resource::INVALID_ARGUMENTS_TYPE as REFUSED_TYPE;
use tilo_actions::{ActionKind, ActionRegistry};
use tilolang_runtime::store::VersionKind;
use tilolang_runtime::{Meta, SourceWords};
use tilolang_tilo::action::{ACTION_TYPE, DynAction, register_actions};
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::naming::resource_id;
use tilolang_tilo::publish::{
    ActionTypes, PlanRequest, Planner, publish_package, publish_resource,
};
use tilolang_tilo::surface::{HOST_KIND, host_surface, package_surfaces, surface_bytes};

use crate::fixtures::{COUNTER, Replica, TODO, field_id};

fn counter() -> Meta {
    let (_, packages) = Replica::new("ada", Workspace::new(), &[(COUNTER, "")]);
    packages[0].meta.clone()
}

tilo_test_env::scenario! {
    fn the_surface_names_kinds_verbs_and_fields;

    phase the_surface_names_a_kind_per_type_and_a_verb_per_action;
    {
        let meta = counter();
        let surfaces = package_surfaces(&meta, &SourceWords);
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
        // By the action's id, the one its changes' claims name: its name is what the API offers
        // it by, not what a document keeps.
        assert_eq!(counter.verb(&increment.id).unwrap().0, ActionKind::Undoable);
        assert!(counter.verb(&increment.name).is_none());
        // The document parameter is the resource itself, so the verb takes none.
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
        // And the name the API offers the action by, so a caller outside the app's code — a
        // script, an agent, the SDK's typed method — invokes it by the word its author wrote.
        let published = counter
            .verbs
            .iter()
            .find(|verb| verb.id == increment.id)
            .unwrap();
        assert_eq!(published.name, increment.name);
        assert_eq!(published.offered(), increment.name);
        // The host's own verbs are named as their ids are.
        assert_eq!(counter.verbs.iter().find(|verb| verb.id == "edit-text").unwrap().name, "");
    }

    phase a_surfaces_actions_read_back_by_the_names_callers_use;
    // A program compiled against the surface (a written view) reaches an action by the word the
    // app declares, while the change it makes keeps naming the declaration's id.
    {
        let meta = counter();
        let bytes = surface_bytes(&[&meta], &SourceWords).unwrap();
        let read = tilolang_tilo::catalogue::described(
            &meta.app,
            &meta.title,
            std::str::from_utf8(&bytes).unwrap(),
        )
        .expect("the surface reads as a package");
        let increment = &meta.actions[0];
        let action = read
            .actions
            .iter()
            .find(|action| action.id == increment.id)
            .expect("the action is in the package");
        assert_eq!(action.name, increment.name);
    }

    phase the_root_the_host_creates_names_a_verb_the_surface_publishes;
    // A package's root is created by the host on its own, as a change of its own: the package's
    // surface publishes the verb that change names, or every receiver (and the identity service
    // signing it) refuses the app's first change in a real session.
    {
        let (replica, packages) = Replica::new("ada", Workspace::new(), &[(COUNTER, "")]);
        let meta = &packages[0].meta;
        let root = replica.host.root_of(&meta.app).unwrap();
        let versions = replica.versions(&root);
        let made = versions
            .iter()
            .find(|version| version.kind == VersionKind::Created)
            .unwrap_or_else(|| panic!("creating the root is a version: {versions:?}"));
        let surfaces = package_surfaces(meta, &SourceWords);
        let surface = surfaces
            .iter()
            .find(|surface| surface.kind == tilolang_tilo::meta::MetaExt::kind_of(meta, "Counter"))
            .unwrap_or_else(|| panic!("no surface of kind {}", meta.app));
        assert_eq!(
            surface.verb(&made.action).map(|verb| verb.0),
            Some(ActionKind::Undoable),
            "{} publishes no {}",
            surface.kind,
            made.action
        );
    }

    phase a_bound_fields_edit_names_a_verb_the_surface_publishes;
    // Typing into a bound text field writes a change of its own: the package's surface publishes
    // the verb that change names, or the identity service refuses to sign the app's first
    // keystroke and the app's collaboration ends there.
    {
        let (replica, packages) = Replica::new("ada", Workspace::new(), &[(TODO, "")]);
        let meta = packages[0].meta.clone();
        let root = replica.host.root_of(&meta.app).unwrap();
        replica.commit();
        let view = replica.view(&meta.app);
        let title = crate::fixtures::field_id(&meta, "TodoList", "title");
        let input = bound_to(view.tree(), &title).expect("the list's title is bound");
        view.edit(&input, &tilolang_runtime::value::Value::text("Groceries"))
            .unwrap();
        // The keystroke is a version of the list, made by the verb a bound text edit names.
        let versions = replica.versions(&root);
        let edit = versions
            .last()
            .unwrap_or_else(|| panic!("the keystroke wrote a version: {versions:?}"));
        assert_eq!(
            edit.fields,
            std::slice::from_ref(&title),
            "the change names the field it changed: {edit:?}"
        );
        let surfaces = package_surfaces(&meta, &SourceWords);
        let surface = surfaces
            .iter()
            .find(|surface| surface.kind == tilolang_tilo::meta::MetaExt::kind_of(&meta, "TodoList"))
            .unwrap_or_else(|| panic!("no surface of kind {}", meta.app));
        let verb = surface
            .verbs
            .iter()
            .find(|verb| verb.id == edit.action)
            .unwrap_or_else(|| panic!("{} publishes no {}", surface.kind, edit.action));
        assert_eq!(verb.kind, ActionKind::Undoable);
        // Everything its verb requires is there: a text edit is what the person typed, so its
        // arguments name the field alone and nothing more.
        assert!(
            verb.params
                .iter()
                .filter(|param| param.required)
                .all(|param| param.name == "field"),
            "{} requires only the field: {:?}",
            edit.action,
            verb.params
        );
    }

    phase an_edit_verb_names_its_fields_the_app_declares;
    // A note's `edit-text` lists the type's editable fields by their **names** — `title` and
    // `body`, never the internal ids a document stores them under — so an agent reads the field to
    // set from the verb itself.
    {
        let (meta, _) = note_package();
        let surfaces = package_surfaces(&meta, &SourceWords);
        let note = surfaces
            .iter()
            .find(|surface| surface.kind == tilolang_tilo::meta::MetaExt::kind_of(&meta, "Note"))
            .expect("the note type is a surface");
        let text = note
            .verbs
            .iter()
            .find(|verb| verb.id == "edit-text")
            .expect("a note offers edit-text");
        let field = text
            .params
            .iter()
            .find(|param| param.name == "field")
            .expect("edit-text takes a field");
        let choices: Vec<&str> = field.schema["enum"]
            .as_array()
            .expect("the field parameter names its choices")
            .iter()
            .filter_map(|choice| choice.as_str())
            .collect();
        assert_eq!(
            choices,
            ["title", "body"],
            "the fields an agent may set, by the names the app declares"
        );
        assert!(
            !choices.contains(&crate::fixtures::field_id(&meta, "Note", "body").as_str()),
            "never the internal id a document stores the body under"
        );
        // The flag verb offers the flag field the same way.
        let flag = note
            .verbs
            .iter()
            .find(|verb| verb.id == "edit-flag")
            .expect("a note offers edit-flag");
        let flag_choices = flag.params[0].schema["enum"]
            .as_array()
            .expect("the flag names its choices");
        assert_eq!(flag_choices, &[json!("pinned")]);
    }

    phase an_edit_verb_takes_a_field_by_the_name_the_app_declares;
    // `edit-text` takes `body` and writes the note's body: a name the app declares, a control's own
    // internal id, and a refusal with a sentence for a field the type lacks.
    {
        let (meta, replica) = note_package();
        // The verb builds what it is asked into a plan the host makes; the host's own mapping
        // of the field is `edit_action`'s, read below.
        let planner: Arc<dyn Planner> = Arc::new(Recording);
        // The verb's own binding takes the name, and the edit reaches the note's body.
        let bound = publish_resource(
            &ActionTypes::new(&meta, &planner, &SourceWords),
            "Note",
            "anote",
            "Note",
        );
        let edit = bound
            .actions
            .iter()
            .find(|spec| spec.id == "edit-text")
            .expect("the note's resource offers edit-text");
        // A field the type lacks is refused by the verb's own build, with a sentence that names
        // the fields it offers rather than the internal ids.
        let refused = edit
            .build_json(
                &json!({"field": "content", "value": "x"})
                    .as_object()
                    .cloned()
                    .unwrap(),
            )
            .expect("the build encodes a refusal rather than panicking");
        assert_eq!(
            refused.type_id, REFUSED_TYPE,
            "a field the type lacks is refused"
        );
        let why = String::from_utf8_lossy(&refused.payload);
        assert!(
            why.contains("title") && why.contains("body"),
            "the refusal names the fields that are offered: {why}"
        );

        // A bound control's `$edit` hands back the field's internal id: the same verb takes it,
        // so the control's own edits keep binding. The value is the runtime's rich-text encoding
        // as text, the single `Text` argument `edit-text` fixes.
        let rich = json!({"rich": [{"id": "", "kind": "paragraph", "text": "by id",
                                    "marks": [], "attrs": {}, "children": []}]});
        let by_id = edit
            .build_json(
                &json!({
                    "field": crate::fixtures::field_id(&meta, "Note", "body"),
                    "value": rich.to_string(),
                })
                .as_object()
                .cloned()
                .unwrap(),
            )
            .expect("the id binds too");
        assert_eq!(
            by_id.type_id, ACTION_TYPE,
            "an edit by the field's internal id is accepted"
        );

        // `field: "body"` is accepted: it names a field the verb offers, and the edit is the
        // note's body, not a field id.
        let notebook = replica.host.root_of(APP_NOTE).unwrap().id_string();
        let by_name = tilolang_tilo::publish::edit_action(
            replica.host.store(),
            &meta,
            "Note",
            &notebook,
            "body",
            &json!({"rich": [{"id": "", "kind": "paragraph", "text": "milk and eggs",
                              "marks": [], "attrs": {}, "children": []}]}),
            None,
            &|_: &tilolang_runtime::value::ResourceRef| None,
        )
        .expect("`body` names the note's body");
        assert_eq!(
            by_name.args["field"],
            json!(crate::fixtures::field_id(&meta, "Note", "body")),
            "the name is mapped to the id the document stores it under"
        );
    }

    phase collection_fields_are_listed_below_their_item;
    {
        let (_, packages) = Replica::new("ada", Workspace::new(), &[(TODO, "")]);
        let meta = &packages[0].meta;
        let surfaces = package_surfaces(meta, &SourceWords);
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

    phase the_compiled_surface_file_reads;
    {
        let meta = counter();
        let bytes = surface_bytes(&[&meta], &SourceWords).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).expect("the surface is compiled JSON");
        let kinds: Vec<&str> = value["resources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|resource| resource["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds[0], HOST_KIND);
        assert!(kinds.contains(&"tl.so.tilo.lang.counter.Counter"));
    }

    phase the_host_surface_declares_no_verb;
    {
        let surface = host_surface();
        assert_eq!(surface.kind, HOST_KIND);
        // Nothing is committed through the host: what an action writes goes to the store as
        // it runs, so the host's resource offers no verb.
        assert!(surface.verbs.is_empty(), "{:?}", surface.verbs);
    }

    phase a_scenarios_action_is_never_published_or_on_a_surface;
    // USER REQUIREMENT (2026-10-20): scenarios are seed data for shots,
    // tests and benchmarks; "they never touch a person's data", so their actions are never offered.
    {
        let source = format!(
            "{TODO}\nscenario \"a full list\" {{\n  let l = root TodoList\n  add(l, \"milk\")\n}}\n"
        );
        let (_, packages) = Replica::new("ada", Workspace::new(), &[(&source, "")]);
        let meta = &packages[0].meta;
        let scenario = meta
            .actions
            .iter()
            .find(|action| action.scenario)
            .expect("the scenario's action is in the metadata");
        assert_eq!(meta.scenarios[0].action, scenario.id);
        for surface in package_surfaces(meta, &SourceWords) {
            assert!(surface.verb(&scenario.id).is_none(), "{}", surface.kind);
        }
        let planner: Arc<dyn Planner> = Arc::new(Recording);
        let package = publish_package(meta, &planner, &SourceWords);
        assert!(package.actions.iter().all(|spec| spec.id != scenario.id));
    }

    phase a_read_actions_answer_is_typed_by_the_surface;
    // A native app's read action is an action of its document in the catalogue,
    // typed by the value the surface publishes it answers with.
    {
        let text = tilo_surface_compile::compile(
            r#"
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
"#,
        )
        .expect("the surface compiles");
        let meta = tilolang_tilo::catalogue::described("so.tilo.pdf-viewer", "PDF Viewer", &text)
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
            .expect("the answer's document is a type");
        let fields: Vec<(&str, &str)> = form
            .fields
            .iter()
            .map(|field| (field.name.as_str(), field.ty.as_str()))
            .collect();
        assert_eq!(fields, [("file", "Text"), ("fields", "List<FormField>")]);
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

/// A package with a note's shape — a title, a body and a flag — the same fields the Notes app
/// declares; it stands alone so it compiles without another app's documents. What it compiles to,
/// and the planner a surface that only builds its verbs is published with.
fn note_package() -> (Meta, Replica) {
    const NOTE: &str = r#"app so.tilo.lang.note "Note"

type Note {
  title: Text
  body: RichText
  pinned: Bool = false
}

root notebook: Note

default view note for Note

view note(n: Note) {
  page {
    input(bind: n.title, placeholder: "Title")
  }
}
"#;
    let (replica, packages) = Replica::new("ada", Workspace::new(), &[(NOTE, "")]);
    let meta = packages[0].meta.clone();
    replica.host.root_of(&meta.app).unwrap();
    replica.commit();
    (meta, replica)
}

const APP_NOTE: &str = "so.tilo.lang.note";

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
        })
    }
}

tilo_test_env::scenario! {
    fn a_published_action_plans_and_its_requirements_refuse;

    phase a_published_action_builds_the_planned_dyn_action;
    {
        let meta = counter();
        let planner: Arc<dyn Planner> = Arc::new(Recording);
        let resource = publish_resource(
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
        // The package's own resource offers the actions whose subject is a root too: the app
        // keeps one resource of the type, so a caller names the action at the app
        // (`tl:<app>#<action>`) whether or not a window opened the root, and the run acts on
        // `root-<type>`. A person takes them where they work with the root, so this row is for
        // agents, scripts and devices.
        let package = publish_package(&meta, &planner, &SourceWords);
        let ids: Vec<&str> = package.actions.iter().map(|spec| spec.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                meta.actions[0].name.as_str(),
                meta.actions[1].name.as_str()
            ]
        );
        assert!(
            package.actions.iter().all(|spec| !spec.reaches_people()),
            "a person takes it on the root"
        );
        let built = package.actions[1]
            .build_json(&serde_json::Map::new())
            .unwrap();
        let planned: DynAction = tilo_actions::decode_json(&built.payload).unwrap();
        let root = meta.target_of(&meta.actions[1].params).unwrap().id.clone();
        assert_eq!(
            planned.args,
            json!({"resource": format!("root-{root}"), "args": {}}),
            "it runs on the app's root, which the first change to it makes"
        );
    }

    phase a_parameter_of_another_apps_document_picks_a_resource_of_its_kind;
    // A parameter of another app's record type (`list: todo.TodoList` in a package that uses the
    // todo app) picks a resource of that kind, as one of the package's own documents does: the API
    // takes the catalogue's reference to it. Published as an array, no caller could pass one.
    {
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
        let (_, packages) = Replica::new("ada", Workspace::new(), &[(TODO, ""), (BOARD, "")]);
        let meta = &packages[1].meta;
        let planner: Arc<dyn Planner> = Arc::new(Recording);
        let resource = publish_resource(
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

    phase a_requirement_refuses_the_invocation_whose_rule_does_not_hold;
    // An action's requirement is the app's to answer, and the app answers it for the one resource
    // a caller names: an API planning the action against an item whose rule does not hold is
    // refused with the rule's reason, where before the answer had to be published for every item
    // of every document.
    {
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
        let (replica, packages) = Replica::new("ada", Workspace::new(), &[(CHORES, "")]);
        let meta = packages[0].meta.clone();
        let app = meta.app.clone();
        let root = replica.host.root_of(&app).unwrap();
        replica.commit();
        let root_id = root.id_string();
        let tasks_kind = meta.kind_of("Tasks");
        for title in ["milk", "bread"] {
            carry(&meta, &replica, &app, &tasks_kind, "add", Some(&root_id), &json!({ "title": title }), None)
                .expect("a task is added");
        }
        let tasks = field_id(&meta, "Tasks", "tasks");
        let task_ty = meta.types.iter().find(|t| t.name == "Task").unwrap();
        let items: Vec<String> = ids(&replica, &root, &tasks)
            .into_iter()
            .map(|id| root.item(&id, task_ty.id.clone()).id_string())
            .collect();
        let first = items[0].clone();
        let task_kind = meta.kind_of("Task");
        carry(&meta, &replica, &app, &task_kind, "finish", Some(&first), &json!({}), None)
            .expect("the task is finished");
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
        let refused = plan(&meta, &replica, &app, &task_kind, "due", Some(&first), &json!({}), None);
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
        plan(&meta, &replica, &app, &task_kind, "due", Some(&open), &json!({}), None)
            .expect("the rule holds for the task that is not done");
    }

    phase a_requirement_over_an_unset_option_field_reads_the_field_as_none;
    // A requirement over an option field the app leaves unset: `t.due != .none` does not hold for a
    // task with no day, so the action that clears the day is unavailable, and holds once a day is
    // set.
    {
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
        let (replica, packages) = Replica::new("ada", Workspace::new(), &[(PLAN, "")]);
        let meta = packages[0].meta.clone();
        let app = meta.app.clone();
        let root = replica.host.root_of(&app).unwrap();
        replica.commit();
        let root_id = root.id_string();
        let tasks_kind = meta.kind_of("Tasks");
        carry(&meta, &replica, &app, &tasks_kind, "add", Some(&root_id), &json!({ "title": "milk" }), None)
            .expect("a task is added");
        let items_field = field_id(&meta, "Tasks", "tasks");
        let task_ty = meta.types.iter().find(|t| t.name == "Task").unwrap();
        let task_kind = task_ty.id.clone();
        let id = ids(&replica, &root, &items_field)
            .into_iter()
            .next()
            .expect("the task it added");
        let item = root.item(&id, task_ty.id.clone()).id_string();
        let clear = meta
            .actions
            .iter()
            .find(|action| action.name == "clear_day")
            .unwrap();
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
        carry(
            &meta,
            &replica,
            &app,
            &task_kind,
            "set_day",
            Some(&item),
            &json!({ "day": "2026-11-10" }),
            None,
        )
        .expect("a day is set");
        assert_eq!(
            replica
                .host
                .availability(&resource_id(&meta.app, &item), &clear.name),
            Some(tilo_actions::Availability::Available),
            "a task due by a day has one to clear"
        );
    }

    phase the_tasks_app_says_a_task_with_no_day_has_none_to_clear;
    // The Tasks app's own clearing action over a task the entry made: a task typed with no day has
    // none to clear, so the app says the action cannot run.
    {
        let mut packages = crate::fixtures::TASKS_SET.to_vec();
        packages.insert(0, crate::fixtures::CHAT);
        let (replica, mut packages) = Replica::new("ada", Workspace::new(), &packages);
        let meta = packages.pop().unwrap().meta;
        let app = meta.app.clone();
        let root = replica.host.root_of(&app).unwrap();
        replica.commit();
        let root_id = root.id_string();
        let tasks_kind = meta.kind_of("Tasks");
        carry(
            &meta,
            &replica,
            &app,
            &tasks_kind,
            "new_task",
            Some(&root_id),
            &json!({ "title": "Water the plants" }),
            None,
        )
        .expect("the task is added");
        let items_field = field_id(&meta, "Tasks", "items");
        let task_ty = meta.types.iter().find(|t| t.name == "Task").unwrap();
        let id = ids(&replica, &root, &items_field)
            .into_iter()
            .next()
            .expect("the task it added");
        let item = root.item(&id, task_ty.id.clone()).id_string();
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

    phase the_workspace_admits_what_an_outside_origin_writes;
    // What a service said is written by its origin as one change of its own, into the records of
    // the types the service keeps: the workspace's store judges every write by the verb it names,
    // so a type an origin keeps declares the verb, and its package's own records declare none.
    {
        const INBOX: &str = r#"app so.tilo.lang.inbox "Inbox"

type Mailbox (origin: imap.mailbox) {
  key: Text merge register
  name: Text
  role: Text merge register
  unread: Int
  total: Int
}

type Inbox {
  mailboxes: Collection<Mailbox>
}

root Inbox

default view inbox for Inbox

view inbox(i: Inbox) {
  text("{i.mailboxes.len()}")
}
"#;
        let (_, packages) = Replica::new("ada", Workspace::new(), &[(INBOX, "")]);
        let meta = &packages[0].meta;
        let declared: Vec<String> = package_surfaces(meta, &SourceWords)
            .into_iter()
            .filter(|surface| {
                tilo_resource_store::Schema::from_surface(surface)
                    .verb(tilolang_tilo::action::ORIGIN_ACTION)
                    .is_some()
            })
            .map(|surface| surface.kind)
            .collect();
        assert_eq!(
            declared,
            [meta.kind_of("Mailbox")],
            "the kind an origin keeps names its verb, and the package's own records do not"
        );
        let (_, plain) = Replica::new("ada", Workspace::new(), &[(TODO, "")]);
        assert!(
            package_surfaces(&plain[0].meta, &SourceWords)
                .iter()
                .all(|surface| surface.verb(tilolang_tilo::action::ORIGIN_ACTION).is_none()),
            "a package with no origin declares no origin's writes"
        );
    }
}

/// Plan the action `name` of `meta` on `at` (a resource id, when the action has a target).
fn plan(
    meta: &Meta,
    replica: &Replica,
    app: &str,
    kind: &str,
    name: &str,
    at: Option<&str>,
    args: &serde_json::Value,
    caller: Option<&tilolang_tilo::publish::Caller>,
) -> Result<DynAction, String> {
    let action = meta
        .actions
        .iter()
        .find(|a| a.name == name)
        .ok_or_else(|| format!("no action {name}"))?;
    let args = args.as_object().cloned().unwrap_or_default();
    replica.host.plan(&PlanRequest {
        app,
        kind,
        action_id: &action.id,
        resource: at,
        args: &args,
        caller,
        from: None,
    })
}

/// Carry out the action `name` of `meta` on `at`, as the API carries out an invocation.
fn carry(
    meta: &Meta,
    replica: &Replica,
    app: &str,
    kind: &str,
    name: &str,
    at: Option<&str>,
    args: &serde_json::Value,
    caller: Option<&tilolang_tilo::publish::Caller>,
) -> Result<DynAction, String> {
    let action = meta
        .actions
        .iter()
        .find(|a| a.name == name)
        .ok_or_else(|| format!("no action {name}"))?;
    let args = args.as_object().cloned().unwrap_or_default();
    let request = PlanRequest {
        app,
        kind,
        action_id: &action.id,
        resource: at,
        args: &args,
        caller,
        from: None,
    };
    replica.carry_out(&request)
}

/// The ids of the items the collection field `field` of `at` holds.
fn ids(replica: &Replica, at: &tilolang_runtime::value::ResourceRef, field: &str) -> Vec<String> {
    match replica.read(at, field) {
        tilolang_runtime::Value::List(items) => items
            .into_iter()
            .map(|item| match item {
                tilolang_runtime::Value::Ref(r) => r.target.item.clone().unwrap_or_default(),
                other => panic!("an item: {other:?}"),
            })
            .collect(),
        other => panic!("no items: {other:?}"),
    }
}

/// A German reader's words: the package's own name, read from its catalogue.
struct German;

impl tilolang_runtime::Words for German {
    fn add(&self, _: &[(String, String)]) {}

    fn add_used(&self, _: &[(String, String)]) {}

    fn word(&self, key: &str, _: &[(String, String)], source: &str) -> String {
        match key {
            "d-so-tilo-lang-counter" => "Zähler".to_owned(),
            _ => source.to_owned(),
        }
    }
}

/// The package's own surface, and every surface file generated from it, name the app in the
/// reader's words: `Counter` in the source language, `Zähler` for a German reader.
#[test]
fn a_surface_names_the_app_in_the_readers_words() {
    let meta = counter();
    let package = |words: &dyn tilolang_runtime::Words| {
        package_surfaces(&meta, words)
            .into_iter()
            .find(|surface| surface.kind == meta.package_kind())
            .expect("the package has a surface of its own")
            .label
    };
    assert_eq!(package(&SourceWords), "Counter");
    assert_eq!(package(&German), "Zähler");
}

/// A record an app keeps for its whole workspace (`collab_types::WORKSPACE_DOCUMENTS`) is
/// published beside the package's own types, so a change to it is decided by its app's surface:
/// a native record's surface is read from the file beside its model, not from a model crate this
/// host links.
#[test]
fn a_package_keeping_a_workspace_record_publishes_its_surface() {
    const CHAT: &str = include_str!("../../../../apps/tl/chat/app.tl");
    const CHAT_IDS: &str = include_str!("../../../../apps/tl/chat/app.ids");
    let kinds = |meta: &Meta| -> Vec<String> {
        let bytes = surface_bytes(&[meta], &SourceWords).unwrap();
        serde_json::from_slice::<serde_json::Value>(&bytes).expect("the surface is compiled JSON")
            ["resources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|resource| resource["kind"].as_str().unwrap().to_owned())
            .collect()
    };
    let (_, packages) = Replica::new("ada", Workspace::new(), &[(CHAT, CHAT_IDS)]);
    let chat = kinds(&packages[0].meta);
    assert!(
        chat.iter().any(|kind| kind == "tilo.moderation"),
        "{chat:?}"
    );
    assert!(
        chat.iter().any(|kind| kind == "tilo.workspace.emoji"),
        "{chat:?}"
    );
    let counter = kinds(&counter());
    assert!(
        !counter
            .iter()
            .any(|kind| kind == "tilo.moderation" || kind == "tilo.workspace.emoji"),
        "another app publishes none of Chat's records: {counter:?}"
    );
}

/// A description in which a loose resource and a kind of `instances` offer one verb, each its
/// own action: the layout's `open` beside an app's.
const ONE_VERB_ON_TWO: &str = r#"
[[resources]]
id = "shell:layout"
kind = "shell"
label = ""
version = 0
fields = []

[[resources.verbs]]
id = "open"
kind = "outside"

[[resources.verbs.params]]
name = "link"
required = true
type = "text"

[[resources]]
instances = true
kind = "applications.app"
label = ""
version = 0
fields = []

[[resources.verbs]]
id = "open"
kind = "undoable"
params = []
"#;

/// An action's name belongs to its resource type: a description may offer one verb on two of its
/// resources, each an action of its own, and the loader refuses only one verb declared twice on
/// one, which a call could not tell apart.
#[test]
fn a_description_declares_each_action_once_for_its_resource() {
    let compiled = tilo_surface_compile::compile(ONE_VERB_ON_TWO).expect("the surface compiles");
    let read = tilolang_tilo::catalogue::described("t.session", "Session", &compiled)
        .expect("one verb on two resources is two actions");
    let mut opens: Vec<(&str, &str)> = read
        .actions
        .iter()
        .filter(|action| action.name == "open")
        .map(|action| (action.id.as_str(), action.verb.as_str()))
        .collect();
    opens.sort_unstable();
    assert_eq!(
        opens,
        [("App/open", "open"), ("Shell/open", "open")],
        "{:#?}",
        read.actions
    );
    // Each id names its type and the verb, so the app's `open` keeps its id whether or not the
    // layout offers one too: a grant or a record naming it still does.
    let alone = ONE_VERB_ON_TWO.replacen(
        "[[resources.verbs]]\nid = \"open\"\nkind = \"outside\"\n\n[[resources.verbs.params]]\nname = \"link\"\nrequired = true\ntype = \"text\"\n",
        "",
        1,
    );
    assert_ne!(alone, ONE_VERB_ON_TWO, "the layout's `open` is taken out");
    let alone = tilo_surface_compile::compile(&alone).expect("the surface compiles");
    let ids: Vec<String> = tilolang_tilo::catalogue::described("t.session", "Session", &alone)
        .expect("the app's open alone")
        .actions
        .into_iter()
        .filter(|action| action.name == "open")
        .map(|action| action.id)
        .collect();
    assert_eq!(ids, ["App/open"]);
    // What the session sends a loose call with is the layout's `open`, never the app's.
    let loose = tilolang_tilo::catalogue::loose_verbs(&compiled).unwrap();
    assert_eq!(
        loose["Shell/open"].params.len(),
        1,
        "the layout's `open` takes its link"
    );

    let twice = ONE_VERB_ON_TWO.replace(
        "[[resources]]\ninstances = true",
        "[[resources.verbs]]\nid = \"open\"\nkind = \"outside\"\nparams = []\n\n[[resources]]\ninstances = true",
    );
    let twice = tilo_surface_compile::compile(&twice).expect("the surface compiles");
    let refused = tilolang_tilo::catalogue::described("t.session", "Session", &twice)
        .expect_err("one verb twice on the layout names neither");
    assert!(refused.contains("`Shell/open` twice"), "{refused}");
}

/// A project whose one action runs a check its host approved.
const CHECKED: &str = r#"app so.tilo.lang.checked "Checked" (icon: .hash, category: .utility)

type Project {
    said: Text = ""
}

root Project

default view project for Project

view project(p: Project) {
    ui.Text(p.said)
}

action run_approved_check(p: Project, check_id: Text) uses check (label: "Run check") {
    match engine.check(check_id) {
        .err(why) => {
            p.said = why
        }
        .ok(job) => {
            p.said = job
        }
    }
}
"#;

tilo_test_env::scenario! {
    /// What a published resource says it is, and what the action that runs an approved check
    /// answers: the identity a host binds an approval to, and the receipt it reports.
    fn a_published_resource_and_check_action_say_what_they_are;

    phase the_identity_is_the_kind_it_publishes_as_and_the_id_it_keeps;
    {
        let meta = counter();
        let planner: Arc<dyn Planner> = Arc::new(Recording);
        let resource = publish_resource(
            &ActionTypes::new(&meta, &planner, &SourceWords),
            "Counter",
            "c1",
            "Counter",
        );
        let identity = resource
            .semantic_identity
            .as_ref()
            .expect("a published resource says what it is");
        assert_eq!(identity.kind, resource.kind);
        assert_eq!(identity.key, resource.id.as_str());
    }

    phase a_check_action_answers_the_receipt_of_what_it_ran;
    {
        let (_, packages) = Replica::new("ada", Workspace::new(), &[(CHECKED, "")]);
        let meta = packages[0].meta.clone();
        let planner: Arc<dyn Planner> = Arc::new(Recording);
        let resource = publish_resource(
            &ActionTypes::new(&meta, &planner, &SourceWords),
            "Project",
            "p1",
            "Project",
        );
        let check = resource
            .actions
            .iter()
            .find(|spec| spec.id == "run-approved-check")
            .expect("the app publishes the action that runs a check");
        assert_eq!(check.contract.kind, ActionKind::Undoable);
        assert_eq!(
            check.contract.execution,
            tilo_actions::ExecutionMode::Background { cancellable: true }
        );
        let tilo_actions::ResultSpec::Json { schema } = &check.contract.result else {
            panic!(
                "a check answers the receipt of what it observed, not {:?}",
                check.contract.result
            );
        };
        tilo_actions::ResultSpec::Json { schema: schema.clone() }
            .validate(&tilo_actions::ActionOutput {
                value: Some(json!({
                    "outcome": "exited",
                    "exit_code": 0,
                    "stdout": { "preview": "ok" },
                })),
                resources: Vec::new(),
            })
            .expect("the receipt is what the action declares");
    }
}
