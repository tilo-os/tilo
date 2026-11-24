//! A tilolang action is published as the catalogue entry a native `#[resource]` verb of the same
//! shape is: an enum parameter as its cases, a document parameter as a pick of a resource of its
//! kind, a destructive action with its risk, kind and confirmation.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::fixtures::Workspace;
use serde::{Deserialize, Serialize};
use tilo_actions::{ActionSpec, Pick, Target};
use tilo_resource::{JsonSchema, Model, Replica as Record, resource};
use tilolang_runtime::SourceWords;
use tilolang_runtime::meta::{Meta, Ty, TypeKey};
use tilolang_runtime::value::{EnumValue, Value};
use tilolang_tilo::OpenEffect;
use tilolang_tilo::action::DynAction;
use tilolang_tilo::host::argument_json;
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::publish::{
    ActionTypes, Moved, PlanRequest, Planner, action_type, open_with_id, publish_resource,
};
use tilolang_tilo::surface::package_surfaces;

use crate::fixtures::Replica;

const SHAPES: &str = r#"app so.tilo.lang.shapes "Shapes"

enum Size {
    small
    large
}

type Item {
    name: Text
}

type Shelf {
    name: Text
    items: Collection<Item>
}

root Shelf

default view main for Shelf

view main(s: Shelf) {
  page {
    text(s.name)
  }
}

/// Its items, as a list of their names.
view compact(s: Shelf) {
  page {
    text(s.name)
  }
}

/// One of its items: shown only with the item, so no resource alone opens it.
view shelved(s: Shelf, item: Item) {
  page {
    text(item.name)
  }
}

action resize(shelf: Shelf, size: Size) { shelf.name = "resized" }

action pin(shelf: Shelf, item: Item) { item.name = "pinned" }

destructive action remove(shelf: Shelf, item: Item) (icon: .trash) { shelf.items.remove(item) }
"#;

const ITEM_KIND: &str = "tl.so.tilo.lang.shapes.Item";

/// A package with one command the person choosing it answers for, and one that runs on the
/// resource alone.
const ASKS: &str = r#"app so.tilo.lang.asks "Asks"

type Pile {
    name: Text
}

root Pile

default view main for Pile

view main(p: Pile) {
    page {
        text(p.name)
    }
}

/// Renames it to what the person types.
action rename(p: Pile, name: Text) {
    p.name = name
}

/// Empties it: there is nothing left to ask.
destructive action empty(p: Pile) (icon: .trash) {
    p.name = ""
}
"#;

/// A package with an action that asks an outside service, and one that asks nothing outside: what
/// the platform tags, so a query naming a kind of that service finds the app's own search.
const SEARCHES: &str = r#"app so.tilo.lang.searches "Searches"

type Video (origin: youtube.video) {
    key: Text
    title: Text
}

type Note {
    title: Text
}

root Video

default view main for Video

view main(v: Video) {
    page {
        text(v.title)
    }
}

/// Searches the service for what the person types.
action search(v: Video, query: Text) {
    outside("youtube.search", query)
}

/// Renames the note.
action rename(n: Note, title: Text) {
    n.title = title
}
"#;

tilo_test_env::scenario! {
    fn an_action_is_published_with_its_parameters;

    phase an_outside_capability_is_published_as_the_actions_tag;
    // An action's `outside(...)` capability is what the platform tags it with
    // (`outside:youtube.search`), so text naming a kind of that outside service finds the app's own
    // action that asks its origin; an action that asks nothing outside carries no such tag.
    {
        let (_, packages) = Replica::new("ada", Workspace::new(), &[(SEARCHES, "")]);
        let meta = &packages[0].meta;
        let spec = |name: &str, kind: &str| {
            let action = meta
                .actions
                .iter()
                .find(|action| action.name == name)
                .unwrap();
            let planner: Arc<dyn Planner> = Arc::new(NoPlanner);
            action_type(meta, &meta.kind_of(kind), action, &planner, &SourceWords)
                .bind(Target::new("v".to_owned()))
        };
        let search = spec("search", "Video");
        assert_eq!(
            search
                .action_type()
                .contract
                .outside_capabilities()
                .collect::<Vec<_>>(),
            vec!["youtube.search"]
        );
        let rename = spec("rename", "Note");
        assert!(
            rename
                .action_type()
                .contract
                .outside_capabilities()
                .next()
                .is_none()
        );
    }

    phase an_action_is_published_by_its_name;
    // A caller at the API lists and calls a tilolang action by its name, as it does a native verb,
    // with its typed parameters and its label; what is stored of it keeps its id.
    {
        let meta = shapes();
        for name in ["resize", "pin", "remove"] {
            let ours = tilolang_spec(&meta, name);
            let native = native_spec(name);
            assert_eq!(ours.id, native.id, "{name}");
            assert_eq!(
                ours.params()
                    .iter()
                    .map(|param| (&param.name, &param.ty))
                    .collect::<Vec<_>>(),
                native
                    .params()
                    .iter()
                    .map(|param| (&param.name, &param.ty))
                    .collect::<Vec<_>>(),
                "{name}"
            );
            assert!(!ours.action_type().label.is_empty(), "{name}");
            let declared = meta.action_published_as(name).unwrap();
            assert_eq!(declared.name, name);
            assert_ne!(declared.id, name, "stored by its id");
        }
    }

    phase an_enum_parameter_is_published_as_its_cases;
    {
        let meta = shapes();
        let ours = tilolang_spec(&meta, "resize");
        let native = native_spec("resize");
        let (param, theirs) = (&ours.params()[0], &native.params()[0]);
        assert_eq!(param.name, "size");
        assert_eq!(param.ty, theirs.ty);
        assert_eq!(param.required, theirs.required);
        let cases = |schema: &serde_json::Value| schema["enum"].clone();
        assert_eq!(
            cases(param.schema.as_deref().unwrap()),
            cases(theirs.schema.as_deref().unwrap())
        );
        // Chosen from, and each case read by its label.
        assert_eq!(param.choices, ["small", "large"]);
        assert_eq!(param.choice_label("large"), Some("Large"));
        assert!(param.validate_declaration().is_empty());
    }

    phase an_enum_argument_travels_as_the_name_of_its_case;
    // What a program gives an enum parameter travels as the name of the case it gave, which the
    // broker reads from the published parameter before it delivers the invocation.
    {
        let meta = shapes();
        let size = meta
            .enums
            .iter()
            .find(|declared| declared.name == "Size")
            .unwrap();
        let large = size
            .variants
            .iter()
            .find(|variant| variant.name == "large")
            .unwrap();
        let ty = Ty::Enum(TypeKey {
            app: meta.app.clone(),
            name: "Size".to_owned(),
        });
        let value = Value::Enum(EnumValue {
            ty: size.id.clone(),
            variant: large.id.clone(),
            fields: BTreeMap::new(),
        });
        let no_package = |app: &str| Err(format!("no package {app}"));
        assert_eq!(
            argument_json(&no_package, &meta, &value, &ty).unwrap(),
            serde_json::json!("large")
        );
    }

    phase a_document_parameter_is_published_as_a_pick_of_its_kind;
    {
        let meta = shapes();
        for (name, id) in [("pin", "pin"), ("remove", "remove")] {
            let ours = tilolang_spec(&meta, name);
            let native = native_spec(id);
            let (param, theirs) = (&ours.params()[0], &native.params()[0]);
            assert_eq!(param.name, "item");
            assert_eq!(param.ty, theirs.ty);
            assert_eq!(
                param.pick(),
                Some(Pick::Resource {
                    kind: ITEM_KIND.to_owned()
                })
            );
            assert_eq!(param.pick(), theirs.pick());
            assert_eq!(untitled(param), untitled(theirs));
        }
    }

    phase a_destructive_action_is_published_as_a_native_destructive_verb;
    {
        let meta = shapes();
        let ours = tilolang_spec(&meta, "remove");
        let native = native_spec("remove");
        let (ours, theirs) = (&ours.action_type().contract, &native.action_type().contract);
        assert_eq!(ours.risk, theirs.risk);
        assert_eq!(ours.kind, theirs.kind);
        assert_eq!(ours.confirmation, theirs.confirmation);
        assert_eq!(
            tilolang_spec(&meta, "remove").action_type().icon,
            native.action_type().icon
        );
        // A plain action is not.
        let resize = tilolang_spec(&meta, "resize");
        assert_eq!(
            resize.action_type().contract.risk,
            native_spec("resize").action_type().contract.risk
        );
    }

    phase a_command_that_asks_for_an_argument_says_so_before_it_opens;
    // A command the person choosing it still has to answer for says so with "…", as the runtime's
    // own menus do: what the published spec is given is the resource, so the parameters after it
    // are the form's. One that acts on what it is given reads plainly.
    {
        let (_, packages) = Replica::new("ada", Workspace::new(), &[(ASKS, "")]);
        let meta = &packages[0].meta;
        let label = |name: &str| {
            let action = meta
                .actions
                .iter()
                .find(|action| action.name == name)
                .unwrap();
            let planner: Arc<dyn Planner> = Arc::new(NoPlanner);
            action_type(meta, &meta.kind_of("Pile"), action, &planner, &SourceWords)
                .bind(Target::new("l".to_owned()))
                .label
                .clone()
        };
        assert_eq!(label("rename"), "Rename…", "the name is the form's");
        assert_eq!(label("empty"), "Empty", "it runs on what it is given");
    }
}

/// The native counterpart of the package's `Size`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Size {
    Small,
    Large,
}

/// A reference to a resource, as a native verb that picks one takes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Picked(serde_json::Value);

impl JsonSchema for Picked {
    fn json_schema() -> serde_json::Value {
        tilo_actions::resource_reference_schema()
    }
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Shelf {
    pub name: String,
}

#[resource(kind = "test.shapes.shelf", catalogue = "tests/locales")]
impl Shelf {
    /// Resize the shelf.
    #[action]
    pub fn resize(&mut self, size: Size) {
        self.name = format!("{size:?}");
    }

    /// Pin an item.
    #[action]
    pub fn pin(
        &mut self,
        #[param("The item", resource = "tl.so.tilo.lang.shapes.Item")] item: Picked,
    ) {
        self.name = format!("{item:?}");
    }

    /// Remove an item.
    #[action(destructive, icon = "trash")]
    pub fn remove(
        &mut self,
        #[param("The item", resource = "tl.so.tilo.lang.shapes.Item")] item: Picked,
    ) {
        self.name = format!("{item:?}");
    }
}

struct NoPlanner;

impl Planner for NoPlanner {
    fn plan(&self, _: &PlanRequest<'_>) -> Result<DynAction, String> {
        Err("not planned here".to_owned())
    }
}

fn shapes() -> Meta {
    let (_, packages) = Replica::new("ada", Workspace::new(), &[(SHAPES, "")]);
    packages[0].meta.clone()
}

/// The tilolang spec of the action named `name`, bound to a shelf.
fn tilolang_spec(meta: &Meta, name: &str) -> ActionSpec {
    let action = meta
        .actions
        .iter()
        .find(|action| action.name == name)
        .unwrap();
    let planner: Arc<dyn Planner> = Arc::new(NoPlanner);
    action_type(meta, &meta.kind_of("Shelf"), action, &planner, &SourceWords)
        .bind(Target::new("s".to_owned()))
}

fn native_spec(id: &str) -> ActionSpec {
    let shelf = Record::<Shelf>::new();
    tilo_resource::publish::<Shelf>("shelf", "Shelf", &shelf.state().unwrap())
        .actions
        .into_iter()
        .find(|spec| spec.id == id)
        .unwrap()
}

/// A parameter's schema without what it is called: the reader's words for it, which each
/// catalogue gives its own and the surface, the same in every language, leaves out.
fn untitled(param: &tilo_actions::ActionParam) -> Option<serde_json::Value> {
    let mut schema = param.schema.as_deref().cloned();
    if let Some(serde_json::Value::Object(schema)) = schema.as_mut() {
        schema.remove("title");
    }
    schema
}

tilo_test_env::scenario! {
    fn the_surface_the_planner_and_the_views_read_what_is_published;

    phase the_surface_says_what_the_published_spec_says;
    // The generated surface describes each parameter as the API binds it, and says which actions
    // are destructive, as a native surface does.
    {
        let meta = shapes();
        let surfaces = package_surfaces(&meta, &SourceWords);
        let shelf = surfaces
            .iter()
            .find(|surface| surface.kind == meta.kind_of("Shelf"))
            .unwrap();
        let native = tilo_resource::surface::surface_of::<Shelf>();
        for name in ["resize", "pin", "remove"] {
            // The generated surface names each verb by the action's id, as a change's claim does; a
            // native app's verbs have no other id than the name it publishes them by.
            let id = &meta
                .actions
                .iter()
                .find(|action| action.name == name)
                .unwrap()
                .id;
            let verb = shelf.verbs.iter().find(|verb| verb.id == *id).unwrap();
            let theirs = native.verbs.iter().find(|verb| verb.id == name).unwrap();
            assert_eq!(verb.destructive, theirs.destructive, "{name}");
            let spec = tilolang_spec(&meta, name);
            let param = &spec.params()[0];
            assert_eq!(verb.params[0].required, param.required, "{name}");
            assert_eq!(
                Some(&verb.params[0].schema),
                untitled(param).as_ref(),
                "{name}"
            );
        }
    }

    phase the_planner_takes_the_arguments_as_published;
    // What the API binds is what the planner takes: a case by its name, a document by the
    // catalogue's reference, refused when it names a resource of another kind.
    {
        let (replica, packages) = Replica::new("ada", Workspace::new(), &[(SHAPES, "")]);
        let meta = &packages[0].meta;
        let root = replica.host.root_of(&meta.app).unwrap();
        let id = |name: &str| {
            meta.actions
                .iter()
                .find(|action| action.name == name)
                .unwrap()
                .id
                .clone()
        };
        let plan = |action: &str, args: serde_json::Value| {
            replica.host.plan(&PlanRequest {
                app: &meta.app,
                kind: &meta.kind_of("Shelf"),
                action_id: action,
                resource: Some(&root.id_string()),
                args: args.as_object().unwrap(),
                caller: None,
                from: None,
            })
        };
        let resized = plan(&id("resize"), serde_json::json!({"size": "large"})).unwrap();
        assert_eq!(resized.args.get("size"), Some(&serde_json::json!("large")));
        assert!(plan(&id("resize"), serde_json::json!({"size": "huge"})).is_err());
        let wrong = serde_json::json!({"item": {
            "app": meta.app,
            "kind": meta.kind_of("Shelf"),
            "id": root.id_string(),
            "label": "Shapes",
        }});
        let refused = plan(&id("pin"), wrong).unwrap_err();
        assert!(refused.contains(ITEM_KIND), "{refused}");
    }

    phase a_resource_lists_the_views_that_show_it;
    // A resource says which views show it, so a host that does not run the app can list them and
    // open one: the package's offered views that take the resource alone, the type's default view
    // the primary one opened through the resource's own `open`, every other through an action of
    // its own.
    {
        let meta = shapes();
        let planner: Arc<dyn Planner> = Arc::new(NoPlanner);
        let types = ActionTypes::new(&meta, &planner, &SourceWords);
        let shelf = publish_resource(&types, "Shelf", "s", "My shelf");
        let id = |name: &str| {
            meta.views
                .iter()
                .find(|view| view.name == name)
                .unwrap()
                .id
                .clone()
        };
        let (main, compact) = (id("main"), id("compact"));
        let listed: Vec<String> = shelf.views.iter().map(|view| view.id.clone()).collect();
        assert_eq!(
            listed,
            [main.clone(), compact.clone()],
            "the package's views of the type, as it declares them"
        );
        let by_id = |id: &str| shelf.views.iter().find(|view| view.id == id).unwrap();
        let (first, second) = (by_id(&main), by_id(&compact));
        assert!(
            first.primary && !second.primary,
            "the type's default view is the primary one"
        );
        assert_eq!(first.label, "main");
        assert_eq!(
            second.description.as_deref(),
            Some("Its items, as a list of their names.")
        );
        // A view and the action that opens it name each other: the default view through the
        // resource's own `open`, which shows it; another through an action of its own.
        assert_eq!(first.open_action, "open");
        assert_eq!(
            second.open_action,
            open_with_id("compact"),
            "by the view's name"
        );
        let opens = shelf
            .actions
            .iter()
            .find(|action| action.id == second.open_action)
            .expect("the compact view's open action is published");
        assert_eq!(opens.label, "compact", "labelled as the view is");
        let encoded = opens.encode_immediate().expect("it takes no input");
        let effect = <OpenEffect as tilo_resource::Effect>::from_payload(&encoded.payload)
            .expect("an open effect");
        assert_eq!(
            effect,
            OpenEffect::OpenWith {
                anchor: "s".to_owned(),
                view: compact.clone(),
            },
            "it opens this resource through that view"
        );
        assert_eq!(
            tilo_actions::validate_surface_contracts(
                std::slice::from_ref(&shelf),
                &tilo_actions::ActionRegistry::new()
            ),
            Vec::new(),
            "a surface the host publishes"
        );
        // A view that needs more than the resource has nothing that opens it, so it is not listed.
        let shelved = id("shelved");
        assert!(
            shelf.views.iter().all(|view| view.id != shelved),
            "{:?}",
            shelf.views
        );
        // A view is listed on the type it shows: an item has none of its own.
        let item = publish_resource(&types, "Item", "i", "An item");
        assert!(item.views.is_empty(), "{:?}", item.views);
    }

    phase a_change_reaches_what_read_it;
    // What a change reaches: an answer kept between publications ([`tilolang_tilo::publish`]) stands
    // while nothing it read moved, so a change to a resource it did not read, or to a field of one it
    // did, must not read it again — and one that did, must.
    {
        let field = |resource: &str, field: &str| (resource.to_owned(), field.to_owned());
        let reached = |(resource, changed): (String, String), read: &(String, String)| {
            Moved::These(std::collections::BTreeMap::from([(
                resource,
                std::collections::BTreeSet::from([changed]),
            )]))
            .reaches(read)
        };
        let list = "tl:tasks:task";
        let task = "tl:tasks:task/1";
        // A requirement that read a task's own field is read again when that field moves, or when the
        // task is replaced whole; another field of the same task leaves the answer as it is.
        assert!(reached(field(task, "done"), &field(task, "done")));
        assert!(reached(field(task, "*"), &field(task, "done")));
        assert!(reached(field(task, "done"), &field(task, "*")));
        assert!(!reached(field(task, "title"), &field(task, "done")));
        // A requirement that read the list of tasks read every task in it: a change to one task is
        // read again, while a change to one of the list's own fields is not a change to its tasks.
        assert!(reached(field(task, "done"), &field(list, "done")));
        assert!(!reached(field(list, "order"), &field(task, "title")));
        // The list replaced whole is every task in it changed.
        assert!(reached(field(list, "*"), &field(task, "title")));
        // Another task, or a resource whose id only starts like the list's, is not under it.
        assert!(!reached(
            field("tl:tasks:task/2", "done"),
            &field(task, "done")
        ));
        assert!(!reached(
            field("tl:tasks:tasks/1", "done"),
            &field(list, "done")
        ));
        // An item under the list is found past ids that sort between the list's and its items'.
        let moved = Moved::These(BTreeMap::from([
            (
                "tl:tasks:task-a".to_owned(),
                std::collections::BTreeSet::from(["done".to_owned()]),
            ),
            (
                "tl:tasks:task/2".to_owned(),
                std::collections::BTreeSet::from(["done".to_owned()]),
            ),
        ]));
        assert!(moved.reaches(&field(list, "done")));
        assert!(!Moved::These(BTreeMap::new()).reaches(&field(list, "*")));
    }
}

const TIMER: &str = r#"app so.tilo.lang.timer "Timer"

type Board {
    seconds: Int = 0
    extra: Int = 0
    slot: Int = 0
}

root board: Board

default view main for Board

view main(b: Board) {
    state span: Duration = 30m
    page {
        text("{b.seconds}")
        button("Set 90m", on: set(span, 90m))
        button("Snooze literal", on: snooze(b, 45m))
        button("Snooze state", on: snooze(b, span))
        button("Snooze plus", on: snooze(b, span + 15m))
    }
}

action snooze(b: Board, span: Duration, extra: Option<Duration> = .none) {
    b.seconds = span.seconds()
    b.extra = match extra {
        .some(d) => d.seconds(),
        .none => 0,
    }
}

action defer(b: Board, span: Duration = 30m) {
    b.seconds = span.seconds()
}

action extend(b: Board, span: Duration) -> Duration {
    return span + 15m
}

action try_extend(b: Board, span: Duration) -> Result<Duration, Text> {
    try extend(b, span)
}

action select_slot(b: Board, n: Int) {
    b.slot = n
}

action count(b: Board, n: Int, extra: Option<Int> = .none) {
    b.seconds = n
    b.extra = match extra {
        .some(v) => v,
        .none => 0,
    }
}
"#;

const TIMER_CONTROL: &str = r#"app so.tilo.lang.control "Control"

use so.tilo.lang.timer as timer

view picker(b: timer.Board) {
    page {
        ui.ViewTabs(selected: 0, label: "Preset") {
            ui.Segment("Zero")
            ui.Segment("One")
            ui.Segment("Two")
            on change(i): timer.select_slot(b, i)
        }
    }
}
"#;

fn timer_button(node: &tilolang_runtime::UiNode, label: &str) -> String {
    node.of_kind("ui.Button")
        .into_iter()
        .find(|button| button.props.get("label") == Some(&Value::text(label)))
        .unwrap_or_else(|| panic!("no button labelled {label}"))
        .key
        .clone()
}

tilo_test_env::scenario! {
    fn a_duration_argument_reaches_the_export_across_api_view_form_and_agent;

    phase a_duration_parameter_and_result_publish_whole_seconds_and_chain_binder_to_export;
    {
        use serde_json::json;
        use tilo_actions::surface::FieldType;
        use tilo_actions::{ActionArgs, ParamType};

        let (replica, packages) = Replica::new("ada", Workspace::new(), &[(TIMER, "")]);
        let meta = &packages[0].meta;
        let root = replica.host.root_of(&meta.app).unwrap();
        let snooze = meta
            .actions
            .iter()
            .find(|action| action.name == "snooze")
            .unwrap();
        let extend = meta
            .actions
            .iter()
            .find(|action| action.name == "extend")
            .unwrap();
        let try_extend = meta
            .actions
            .iter()
            .find(|action| action.name == "try_extend")
            .unwrap();
        let planner: Arc<dyn Planner> = Arc::new(NoPlanner);
        let spec = action_type(meta, &meta.kind_of("Board"), snooze, &planner, &SourceWords)
            .bind(Target::new("b".to_owned()));
        let expected_schema = json!({"type": "integer", "description": "Duration in whole seconds"});
        let span = &spec.params()[0];
        let extra = &spec.params()[1];
        assert_eq!(span.name, "span");
        assert_eq!(span.ty, ParamType::Integer);
        assert!(span.required);
        assert!(
            span.description.contains("whole seconds"),
            "parameter description names the unit: {}",
            span.description
        );
        assert_eq!(untitled(span), Some(expected_schema.clone()));
        assert_eq!(extra.name, "extra");
        assert_eq!(extra.ty, ParamType::Integer);
        assert!(!extra.required);
        assert!(
            extra.description.contains("whole seconds"),
            "optional parameter description names the unit: {}",
            extra.description
        );
        assert_eq!(untitled(extra), Some(expected_schema.clone()));

        let surfaces = package_surfaces(meta, &SourceWords);
        let board = surfaces
            .iter()
            .find(|surface| surface.kind == meta.kind_of("Board"))
            .unwrap();
        let verb = board.verbs.iter().find(|v| v.id == snooze.id).unwrap();
        assert_eq!(verb.params[0].ty, FieldType::Duration);
        assert_eq!(verb.params[0].schema, expected_schema);
        assert_eq!(
            verb.params[1].ty,
            FieldType::Option(Box::new(FieldType::Duration))
        );
        assert_eq!(verb.params[1].schema, expected_schema);

        let extend_verb = board.verbs.iter().find(|v| v.id == extend.id).unwrap();
        let returns = extend_verb
            .returns
            .as_ref()
            .expect("extend publishes a Duration result");
        assert_eq!(returns.ty, FieldType::Duration);
        assert_eq!(returns.schema, expected_schema);

        // `-> Result<Duration, Text>` has no `schema_of` form, so `verb` publishes null rather
        // than an empty `{}` object.
        let try_extend_verb = board.verbs.iter().find(|v| v.id == try_extend.id).unwrap();
        let try_returns = try_extend_verb
            .returns
            .as_ref()
            .expect("try_extend publishes a result");
        assert_eq!(try_returns.schema, serde_json::Value::Null);

        // One call chains the API binder into the planner and into the Wasm export.
        let bound = ActionArgs::bind_json(
            spec.params(),
            json!({"span": 5400, "extra": 900}).as_object().unwrap(),
        )
        .expect("whole seconds bind against published Duration parameters");
        assert_eq!(bound.integer("span"), Some(5400));
        assert_eq!(bound.integer("extra"), Some(900));
        let bound_json = bound.to_json();
        replica
            .carry_out(&PlanRequest {
                app: &meta.app,
                kind: &meta.kind_of("Board"),
                action_id: &snooze.id,
                resource: Some(&root.id_string()),
                args: &bound_json,
                caller: None,
                from: None,
            })
            .expect("bound Duration arguments reach the Wasm export through the planner");
        assert_eq!(
            replica.host.runtime().read(&root, "seconds").unwrap(),
            Value::Int(5400)
        );
        assert_eq!(
            replica.host.runtime().read(&root, "extra").unwrap(),
            Value::Int(900)
        );

        for wrong in [
            json!({"span": true}),
            json!({"span": "90m"}),
            json!({"span": 1.5}),
            json!({"span": {}}),
        ] {
            assert!(
                ActionArgs::bind_json(spec.params(), wrong.as_object().unwrap()).is_err(),
                "wrong JSON type {wrong} must not bind as a Duration"
            );
            assert!(
                replica
                    .carry_out(&PlanRequest {
                        app: &meta.app,
                        kind: &meta.kind_of("Board"),
                        action_id: &snooze.id,
                        resource: Some(&root.id_string()),
                        args: wrong.as_object().unwrap(),
                        caller: None,
                        from: None,
                    })
                    .is_err(),
                "planner must refuse {wrong} for Duration"
            );
        }
    }

    phase optional_duration_matches_optional_int_at_the_api_binder_and_planner;
    {
        use serde_json::json;
        use tilo_actions::ActionArgs;

        let (replica, packages) = Replica::new("ada", Workspace::new(), &[(TIMER, "")]);
        let meta = &packages[0].meta;
        let root = replica.host.root_of(&meta.app).unwrap();
        let planner: Arc<dyn Planner> = Arc::new(NoPlanner);
        let spec_for = |name: &str| {
            let action = meta
                .actions
                .iter()
                .find(|action| action.name == name)
                .unwrap();
            let spec = action_type(meta, &meta.kind_of("Board"), action, &planner, &SourceWords)
                .bind(Target::new("b".to_owned()));
            (action.id.clone(), spec)
        };
        let (snooze_id, snooze_spec) = spec_for("snooze");
        let (count_id, count_spec) = spec_for("count");

        let cases = [
            (
                "bare integer",
                json!({"span": 5400, "extra": 900}),
                json!({"n": 5400, "extra": 900}),
                Some(900_i64),
            ),
            (
                "object form",
                json!({"span": 5400, "extra": {"some": 900}}),
                json!({"n": 5400, "extra": {"some": 900}}),
                None,
            ),
            (
                "null",
                json!({"span": 5400, "extra": null}),
                json!({"n": 5400, "extra": null}),
                Some(0_i64),
            ),
            (
                "omitted",
                json!({"span": 5400}),
                json!({"n": 5400}),
                Some(0_i64),
            ),
        ];

        for (label, dur_input, int_input, expected_extra) in cases {
            let dur_bound =
                ActionArgs::bind_json(snooze_spec.params(), dur_input.as_object().unwrap());
            let int_bound =
                ActionArgs::bind_json(count_spec.params(), int_input.as_object().unwrap());
            assert_eq!(
                dur_bound.is_ok(),
                int_bound.is_ok(),
                "Option<Duration> and Option<Int> must agree on {label}: dur={dur_bound:?}, int={int_bound:?}"
            );
            if let Some(expected) = expected_extra {
                let dur_args = dur_bound.expect(label).to_json();
                let int_args = int_bound.expect(label).to_json();
                assert_eq!(
                    dur_args.get("extra"),
                    int_args.get("extra"),
                    "bound JSON for `extra` must agree on {label}"
                );
                replica
                    .carry_out(&PlanRequest {
                        app: &meta.app,
                        kind: &meta.kind_of("Board"),
                        action_id: &snooze_id,
                        resource: Some(&root.id_string()),
                        args: &dur_args,
                        caller: None,
                        from: None,
                    })
                    .unwrap_or_else(|e| panic!("Option<Duration> {label} failed: {e}"));
                let dur_extra = replica.host.runtime().read(&root, "extra").unwrap();
                replica
                    .carry_out(&PlanRequest {
                        app: &meta.app,
                        kind: &meta.kind_of("Board"),
                        action_id: &count_id,
                        resource: Some(&root.id_string()),
                        args: &int_args,
                        caller: None,
                        from: None,
                    })
                    .unwrap_or_else(|e| panic!("Option<Int> {label} failed: {e}"));
                let int_extra = replica.host.runtime().read(&root, "extra").unwrap();
                assert_eq!(dur_extra, Value::Int(expected), "{label}");
                assert_eq!(int_extra, Value::Int(expected), "{label}");
            } else {
                // Both refuse `{"some": 900}` at the binder with the same wrong-type error.
                assert!(dur_bound.is_err() && int_bound.is_err(), "{label}");
            }
        }
    }

    phase a_view_control_hands_a_duration_through_an_action_reference_and_boxed_scalars_match_int;
    {
        use serde_json::json;

        let (replica, packages) = Replica::new("ada", Workspace::new(), &[(TIMER, "")]);
        let meta = &packages[0].meta;
        let root = replica.host.root_of(&meta.app).unwrap();
        let snooze_id = meta
            .actions
            .iter()
            .find(|action| action.name == "snooze")
            .unwrap()
            .id
            .clone();
        let view = replica.host.open_main(&meta.app).unwrap();

        // Literal `Duration` in a view control's ActionRef travels unboxed (`ListPushI`).
        let lit_key = timer_button(view.tree(), "Snooze literal");
        let lit_aref = view.event_action(&lit_key, "press").unwrap();
        assert_eq!(lit_aref.args[1], Value::Int(2700));
        view.press(&lit_key)
            .expect("a button passing a literal Duration runs the action");
        assert_eq!(
            replica.host.runtime().read(&root, "seconds").unwrap(),
            Value::Int(2700)
        );

        // View state of type `Duration` is boxed in the slot by `handle_of` (`SomeI`),
        // referenced as `Value::State { scalar: true, .. }`, and unboxed in
        // `ViewInstance::read_states` (`state_now`) before running the action.
        let state_key = timer_button(view.tree(), "Snooze state");
        let state_aref = view.event_action(&state_key, "press").unwrap();
        assert!(
            matches!(state_aref.args[1], Value::State { scalar: true, .. }),
            "view state Duration arrives as a scalar State placeholder: {:?}",
            state_aref.args[1]
        );
        view.press(&state_key)
            .expect("a button passing a Duration view state runs the action");
        assert_eq!(
            replica.host.runtime().read(&root, "seconds").unwrap(),
            Value::Int(1800)
        );

        // Updating the view state via `set(span, 90m)` boxes 90m (`handle_of`), then firing
        // the action unboxes it to 5400s.
        let set_key = timer_button(view.tree(), "Set 90m");
        view.press(&set_key).expect("set(span, 90m) updates the view state");
        view.press(&state_key)
            .expect("pressing Snooze state reads the updated boxed Duration");
        assert_eq!(
            replica.host.runtime().read(&root, "seconds").unwrap(),
            Value::Int(5400)
        );

        // Computed `Duration` from view state (`span + 15m`) evaluates via `AtEvent` (`call_pure`).
        let plus_key = timer_button(view.tree(), "Snooze plus");
        let plus_aref = view.event_action(&plus_key, "press").unwrap();
        assert!(
            matches!(plus_aref.args[1], Value::AtEvent { scalar: true, .. }),
            "computed Duration arrives as a scalar AtEvent placeholder: {:?}",
            plus_aref.args[1]
        );
        view.press(&plus_key)
            .expect("a button passing a computed Duration runs the action");
        assert_eq!(
            replica.host.runtime().read(&root, "seconds").unwrap(),
            Value::Int(6300)
        );

        // When a boxed scalar reaches `argument_json` (e.g. from a component event placeholder),
        // it encodes as plain integer JSON matching `published_param` and `Int`.
        let no_package = |app: &str| Err(format!("no package {app}"));
        assert_eq!(
            argument_json(&no_package, meta, &Value::Int(5400), &Ty::Duration).unwrap(),
            json!(5400)
        );
        assert_eq!(
            argument_json(
                &no_package,
                meta,
                &Value::some(Value::Int(5400)),
                &Ty::Duration,
            )
            .unwrap(),
            json!(5400)
        );
        assert_eq!(
            argument_json(&no_package, meta, &Value::some(Value::Int(5400)), &Ty::Int).unwrap(),
            json!(5400)
        );

        // Direct `Host::argument` / planner calls also accept the boxed form `{"some": N}` just
        // as `Int` does, while refusing every wrong type inside or outside the box.
        assert_eq!(
            replica.host.argument(meta, &json!(5400), "Duration"),
            Ok(Value::Int(5400))
        );
        assert_eq!(
            replica.host.argument(meta, &json!(-60), "Duration"),
            Ok(Value::Int(-60))
        );
        assert_eq!(
            replica.host.argument(meta, &json!({"some": 5400}), "Duration"),
            replica.host.argument(meta, &json!({"some": 5400}), "Int"),
            "boxed Duration decodes identically to boxed Int"
        );
        assert_eq!(
            replica.host.argument(meta, &json!(900), "Option<Duration>"),
            Ok(Value::some(Value::Int(900)))
        );
        assert_eq!(
            replica.host.argument(meta, &json!(null), "Option<Duration>"),
            Ok(Value::none())
        );
        for wrong in [
            json!(true),
            json!("90m"),
            json!(1.5),
            json!({}),
            json!(null),
            json!({"some": true}),
            json!({"some": "90m"}),
            json!({"some": 1.5}),
            json!({"none": true}),
        ] {
            assert!(
                replica.host.argument(meta, &wrong, "Duration").is_err(),
                "Host::argument must refuse {wrong} for Duration"
            );
        }
        assert!(
            replica
                .host
                .argument(meta, &json!(true), "Option<Duration>")
                .is_err(),
            "Host::argument must refuse boolean for Option<Duration>"
        );
        replica
            .carry_out(&PlanRequest {
                app: &meta.app,
                kind: &meta.kind_of("Board"),
                action_id: &snooze_id,
                resource: Some(&root.id_string()),
                args: json!({"span": {"some": 3600}}).as_object().unwrap(),
                caller: None,
                from: None,
            })
            .expect("boxed Duration reaches the Wasm export as it does for Int");
        assert_eq!(
            replica.host.runtime().read(&root, "seconds").unwrap(),
            Value::Int(3600)
        );
    }

    phase a_form_asks_for_a_duration_with_its_default_and_reaches_the_export;
    {
        use std::cell::RefCell;
        use std::rc::Rc;
        use tilolang_runtime::{ActionRef, InvokeOutcome};

        let (replica, packages) = Replica::new("ada", Workspace::new(), &[(TIMER, "")]);
        let meta = packages[0].meta.clone();
        let root = replica.host.root_of(&meta.app).unwrap();
        let defer_id = meta
            .actions
            .iter()
            .find(|action| action.name == "defer")
            .unwrap()
            .id
            .clone();
        let view = Rc::new(RefCell::new(replica.host.open_main(&meta.app).unwrap()));
        let aref = ActionRef::new(defer_id, vec![Value::resource(&root)]);
        let mut form = tilolang_tilo::instance::form::Pending::new(
            1,
            None,
            Rc::clone(&view),
            aref,
            meta,
        )
        .expect("a form opens for a Duration parameter");

        let shown = form.shown(&replica.host, &tilolang_tilo::names::Installed);
        assert_eq!(
            shown
                .fields
                .iter()
                .find(|(key, _)| key == "form~span")
                .map(|(_, text)| text.as_str()),
            Some("30m"),
            "the Duration default renders in the form"
        );
        form.edit("form~span", "1h 30m".to_owned());
        let completed = form
            .completed(&replica.host)
            .expect("the typed duration reads as whole seconds");
        let outcome = view
            .borrow()
            .invoke(&completed)
            .expect("submitting the form reaches the wasm export");
        assert_eq!(outcome, InvokeOutcome::Done(Value::Unit));
        assert_eq!(
            replica.host.runtime().read(&root, "seconds").unwrap(),
            Value::Int(5400)
        );
    }

    phase an_agent_program_calls_an_action_with_a_duration_through_the_api;
    {
        use serde_json::json;
        use std::rc::Rc;

        let api = Rc::new(crate::task_api::Apps::new(&[&[(TIMER, "")]]));
        let session = api.task();
        let agent = tilolang_tilo::runtime::Actor::from(
            tilolang_tilo::runtime::ids::MemberId::agent("task-1"),
        );
        let answer = session.run("timer.snooze(timer.board, 90m, .some(15m))", &agent);
        assert_eq!(answer["state"], "done", "{answer:#}");
        let call = api.invoked.borrow().last().cloned().unwrap();
        assert_eq!(call["arguments"]["span"], json!(5400), "{call:#}");
        assert_eq!(call["arguments"]["extra"], json!(900), "{call:#}");

        let replica = api.replica("so.tilo.lang.timer");
        let root = replica.host.root_of("so.tilo.lang.timer").unwrap();
        assert_eq!(
            replica.host.runtime().read(&root, "seconds").unwrap(),
            Value::Int(5400)
        );
        assert_eq!(
            replica.host.runtime().read(&root, "extra").unwrap(),
            Value::Int(900)
        );
    }

    phase a_scalar_carrying_control_event_sends_a_bare_integer_through_the_invoker;
    {
        use serde_json::{Map, Value as Json, json};
        use std::cell::RefCell;
        use std::rc::Rc;
        use tilo_actions::ActionArgs;
        use tilolang::IdsLock;
        use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
        use tilolang_runtime::{Actor, InvokeOutcome, ResourceRef};
        use tilolang_tilo::host::Host;

        struct BridgeInvoker {
            target_meta: Meta,
            target_app: Rc<RefCell<Replica>>,
            sent_raw: Rc<RefCell<Vec<(String, Value)>>>,
            sent_json: Rc<RefCell<Vec<Map<String, Json>>>>,
        }

        impl Invoker for BridgeInvoker {
            fn invoke(&self, invocation: &Invocation) -> Invoked {
                let Invocation::Action {
                    action,
                    args,
                    target,
                    ..
                } = invocation
                else {
                    return Invoked::Failed("expected an action invocation".to_owned());
                };
                let declared = self
                    .target_meta
                    .actions
                    .iter()
                    .find(|declared| declared.id == *action)
                    .expect("declared action on target app");
                let no_package = |app: &str| Err(format!("no package {app}"));
                let mut encoded = Map::new();
                for (name, value) in args.iter().skip(usize::from(target.is_some())) {
                    self.sent_raw
                        .borrow_mut()
                        .push((name.clone(), value.clone()));
                    let param = declared
                        .params
                        .iter()
                        .find(|param| param.name == *name)
                        .expect("declared parameter on target action");
                    let ty = tilolang_runtime::meta::parse_ty(&param.ty, &self.target_meta.app)
                        .expect("parameter type parses");
                    let json = match argument_json(&no_package, &self.target_meta, value, &ty) {
                        Ok(json) => json,
                        Err(why) => return Invoked::Failed(why),
                    };
                    encoded.insert(name.clone(), json);
                }
                self.sent_json.borrow_mut().push(encoded.clone());

                let params: Vec<_> = self
                    .target_meta
                    .arguments(&declared.params)
                    .iter()
                    .map(|param| tilolang_tilo::surface::published_param(&self.target_meta, param))
                    .collect();
                let bound = match ActionArgs::bind_json(&params, &encoded) {
                    Ok(bound) => bound,
                    Err(why) => return Invoked::Failed(format!("binder refused: {why}")),
                };
                let bound_json = bound.to_json();
                let resource_id = target.as_ref().map(ResourceRef::id_string);
                match self.target_app.borrow().carry_out(&PlanRequest {
                    app: &self.target_meta.app,
                    kind: &self.target_meta.kind_of("Board"),
                    action_id: action,
                    resource: resource_id.as_deref(),
                    args: &bound_json,
                    caller: None,
                    from: None,
                }) {
                    Ok(_) => Invoked::Done(Value::Unit),
                    Err(why) => Invoked::Failed(why),
                }
            }
        }

        let (replica, target_packages) = Replica::new("ada", Workspace::new(), &[(TIMER, "")]);
        let timer_meta = target_packages[0].meta.clone();
        let board_root = replica.host.root_of(&timer_meta.app).unwrap();
        let target_app = Rc::new(RefCell::new(replica));

        let caller_host = Host::new(
            Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            || None,
        )
        .unwrap();
        caller_host
            .runtime()
            .load_foreign(timer_meta.clone())
            .unwrap();
        let compiled = tilolang::compile_package(
            TIMER_CONTROL,
            &IdsLock::new(),
            std::slice::from_ref(&timer_meta),
        )
        .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
        let control_pkg = caller_host.load_compiled(&compiled).unwrap();
        let picker_id = control_pkg
            .meta
            .views
            .iter()
            .find(|view| view.name == "picker")
            .expect("picker view")
            .id
            .clone();

        let foreign_board = ResourceRef::parse_id(
            &board_root.id_string(),
            format!("{}#Board", timer_meta.app),
        )
        .expect("foreign Board ref");
        caller_host
            .runtime()
            .set_reading(
                &foreign_board,
                &json!({"seconds": 0, "extra": 0, "slot": 0}).to_string(),
            )
            .unwrap();

        let sent_raw = Rc::new(RefCell::new(Vec::new()));
        let sent_json = Rc::new(RefCell::new(Vec::new()));
        caller_host.runtime().set_invoker(BridgeInvoker {
            target_meta: timer_meta,
            target_app: Rc::clone(&target_app),
            sent_raw: Rc::clone(&sent_raw),
            sent_json: Rc::clone(&sent_json),
        });

        let view = caller_host
            .open_resource(&foreign_board, Some(&picker_id))
            .unwrap();
        let tabs_key = view
            .tree()
            .of_kind("ui.ViewTabs")
            .into_iter()
            .next()
            .expect("ui.ViewTabs control")
            .key
            .clone();
        let tabs_aref = view.event_action(&tabs_key, "change").unwrap();
        assert_eq!(
            tabs_aref.args[1],
            Value::some(Value::Event),
            "component compiler boxes scalar event placeholder"
        );

        let outcome = view
            .fire_in(&tabs_key, "change", Some(&Value::Int(2)), None)
            .expect("firing change event invokes target app action through the invoker");
        assert_eq!(outcome, InvokeOutcome::Done(Value::Unit));

        assert_eq!(
            *sent_raw.borrow(),
            vec![("n".to_owned(), Value::some(Value::Int(2)))],
            "fill_event hands the boxed scalar to the invoker"
        );
        let sent_args = sent_json.borrow();
        assert_eq!(sent_args.len(), 1);
        assert_eq!(
            sent_args[0].get("n"),
            Some(&json!(2)),
            "argument_json unboxes scalar event arguments to a bare integer on the wire"
        );
        assert_eq!(
            target_app
                .borrow()
                .host
                .runtime()
                .read(&board_root, "slot")
                .unwrap(),
            Value::Int(2),
            "target app's action ran with the fired integer value"
        );
    }
}

/// A package whose types hold what an outside service sent (`origin: imap.message`) publishes
/// its read action saying that what it answers is untrusted: an agent that reads a message
/// through it is told the words are a stranger's, data to weigh and never instructions. A
/// package of the workspace's own data reads as trusted.
#[test]
fn a_read_action_of_a_package_of_outside_text_carries_untrusted_content() {
    const MAILISH: &str = r#"app so.tilo.lang.mailish "Mailish"

type Message (origin: imap.message, title: subject) {
    subject: Text
    body: Text
}

root Message

default view main for Message

view main(m: Message) {
  page {
    text(m.subject)
  }
}
"#;
    let read_is_untrusted = |source: &str, ty: &str| {
        let (_, packages) = Replica::new("ada", Workspace::new(), &[(source, "")]);
        let meta = &packages[0].meta;
        let planner: Arc<dyn Planner> = Arc::new(NoPlanner);
        let types = ActionTypes::new(meta, &planner, &SourceWords);
        publish_resource(&types, ty, "one", "One")
            .actions
            .into_iter()
            .find(|spec| spec.id == "read")
            .expect("every resource reads")
            .action_type()
            .contract
            .carries_untrusted_content()
    };
    assert!(
        read_is_untrusted(MAILISH, "Message"),
        "a message's words are written by whoever sent it"
    );
    assert!(
        !read_is_untrusted(SHAPES, "Shelf"),
        "the workspace's own data is trusted"
    );
}
