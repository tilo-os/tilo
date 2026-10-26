use std::sync::Arc;

use automerge::AutoCommit;
use serde_json::json;
use tilo_actions::{ActionKind, ActionRegistry};
use tilolang_runtime::Meta;
use tilolang_tilo::action::{ACTION_TYPE, DynAction, register_actions};
use tilolang_tilo::publish::{
    ActionTypes, PlanRequest, Planner, publish_package, publish_record, resource_id,
};
use tilolang_tilo::surface::{HOST_KIND, host_surface, package_surfaces, surface_toml};

use crate::fixtures::{COUNTER, Replica, TODO};

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
    assert_eq!(counter.verb(&increment.id).unwrap().0, ActionKind::Undoable);
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
        &ActionTypes::new(&meta, &planner),
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
        .find(|spec| spec.id == meta.actions[0].id)
        .unwrap();
    assert_eq!(increment.contract.kind, ActionKind::Undoable);
    let built = increment.build_json(&serde_json::Map::new()).unwrap();
    assert_eq!(built.type_id, ACTION_TYPE);
    assert!(
        ActionRegistry::with(register_actions)
            .decode(&built)
            .is_ok()
    );
    assert!(publish_package(&meta, &planner).actions.is_empty());
}

#[test]
fn the_host_surface_declares_the_commit() {
    let surface = host_surface();
    assert_eq!(surface.kind, HOST_KIND);
    assert_eq!(surface.verb("commit").unwrap().0, ActionKind::Undoable);
}

// USER REQUIREMENT (2026-10-20, native-apps.md section 5): scenarios are seed data for shots,
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
    let package = publish_package(meta, &planner);
    assert!(package.actions.iter().all(|spec| spec.id != scenario.id));
}
