//! A tilolang action is published as the catalogue entry a native `#[resource]` verb of the same
//! shape is: an enum parameter as its cases, a record parameter as a pick of a resource of its
//! kind, a destructive action with its risk, kind and confirmation.

use std::sync::Arc;

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use tilo_actions::{ActionSpec, Pick, Target};
use tilo_resource::{JsonSchema, Model, Replica as Record, resource};
use tilolang_runtime::meta::Meta;
use tilolang_tilo::action::DynAction;
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::publish::{ActionTypes, PlanRequest, Planner, action_type, publish_record};
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

action resize(shelf: Shelf, size: Size) { shelf.name = "resized" }

action pin(shelf: Shelf, item: Item) { item.name = "pinned" }

destructive action remove(shelf: Shelf, item: Item) (icon: .trash) { shelf.items.remove(item) }
"#;

const ITEM_KIND: &str = "tl.so.tilo.lang.shapes.Item";

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

#[resource(kind = "test.shapes.shelf", label = "Shelf")]
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
    let (_, packages) = Replica::new("ada", AutoCommit::new(), &[(SHAPES, "")]);
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
    action_type(meta, &meta.kind_of("Shelf"), action, &planner).bind(Target::new("s".to_owned()))
}

fn native_spec(id: &str) -> ActionSpec {
    let shelf = Record::<Shelf>::new();
    tilo_resource::publish::<Shelf>("shelf", "Shelf", &shelf.state().unwrap())
        .actions
        .into_iter()
        .find(|spec| spec.id == id)
        .unwrap()
}

#[test]
fn an_enum_parameter_is_published_as_its_cases() {
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

#[test]
fn a_record_parameter_is_published_as_a_pick_of_its_kind() {
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
        assert_eq!(param.schema, theirs.schema);
    }
}

#[test]
fn a_destructive_action_is_published_as_a_native_destructive_verb() {
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

/// The generated surface describes each parameter as the door binds it, and says which actions
/// are destructive, as a native surface does.
#[test]
fn the_surface_says_what_the_published_spec_says() {
    let meta = shapes();
    let surfaces = package_surfaces(&meta);
    let shelf = surfaces
        .iter()
        .find(|surface| surface.kind == meta.kind_of("Shelf"))
        .unwrap();
    let native = tilo_resource::surface::surface_of::<Shelf>();
    for name in ["resize", "pin", "remove"] {
        let action = meta
            .actions
            .iter()
            .find(|action| action.name == name)
            .unwrap();
        let verb = shelf
            .verbs
            .iter()
            .find(|verb| verb.id == action.id)
            .unwrap();
        let theirs = native.verbs.iter().find(|verb| verb.id == name).unwrap();
        assert_eq!(verb.destructive, theirs.destructive, "{name}");
        let spec = tilolang_spec(&meta, name);
        let param = &spec.params()[0];
        assert_eq!(verb.params[0].required, param.required, "{name}");
        assert_eq!(
            Some(&verb.params[0].schema),
            param.schema.as_deref(),
            "{name}"
        );
    }
}

/// What the door binds is what the planner takes: a case by its name, a record by the
/// catalogue's reference, refused when it names a resource of another kind.
#[test]
fn the_planner_takes_the_arguments_as_published() {
    let (replica, packages) = Replica::new("ada", AutoCommit::new(), &[(SHAPES, "")]);
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
        })
    };
    let resized = plan(&id("resize"), serde_json::json!({"size": "large"})).unwrap();
    assert!(!resized.edits.is_empty());
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

/// A resource says which views show it, so a host that does not run the app can list them and
/// mount one: the package's offered views of that type, each opened through the resource's own
/// `open`, with the type's default view the primary one.
#[test]
fn a_resource_lists_the_views_that_show_it() {
    let meta = shapes();
    let planner: Arc<dyn Planner> = Arc::new(NoPlanner);
    let types = ActionTypes::new(&meta, &planner);
    let shelf = publish_record(&types, "Shelf", "s", "My shelf");
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
    assert!(
        shelf.views.iter().all(|view| view.open_action == "open")
            && shelf.actions.iter().any(|action| action.id == "open"),
        "each view opens through the resource's own open"
    );
    for view in &shelf.views {
        assert_eq!(view.validate(&shelf), Vec::<String>::new(), "{view:?}");
    }
    // A view is listed on the type it shows: an item has none of its own.
    let item = publish_record(&types, "Item", "i", "An item");
    assert!(item.views.is_empty(), "{:?}", item.views);
}
