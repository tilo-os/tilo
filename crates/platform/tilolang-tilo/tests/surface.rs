use std::sync::Arc;

use serde_json::json;
use tilo_actions::{ActionKind, ActionRegistry};
use tilolang_tilo::action::{ACTION_TYPE, DynAction, register_actions};
use tilolang_tilo::publish::{
    PlanRequest, Planner, host_resource, publish_package, publish_record, resource_id,
};
use tilolang_tilo::surface::{HOST_KIND, package_surfaces, surface_toml};

use crate::fixtures::meta;

#[test]
fn the_surface_names_a_kind_per_type_and_a_verb_per_action() {
    let meta = meta();
    let surfaces = package_surfaces(&meta);
    let kinds: Vec<&str> = surfaces
        .iter()
        .map(|surface| surface.kind.as_str())
        .collect();
    assert_eq!(
        kinds,
        [
            "tl.so.tilo.counter.Counter",
            "tl.so.tilo.counter.Todo",
            "tl.so.tilo.counter.Item",
            "tl.so.tilo.counter"
        ]
    );
    let counter = &surfaces[0];
    let verb = counter.verb("aincr001").unwrap();
    assert_eq!(verb.0, ActionKind::Undoable);
    assert_eq!(counter.verb("alook001").unwrap().0, ActionKind::Read);
    // The record parameter is the resource itself, so the verb takes none.
    assert!(
        counter
            .verbs
            .iter()
            .find(|v| v.id == "aincr001")
            .unwrap()
            .params
            .is_empty()
    );
    // A record-less action lives on the package resource, with its class.
    assert_eq!(surfaces[3].verb("asend001").unwrap().0, ActionKind::Outside);
    // Nested collection fields are listed below their item.
    let todo = &surfaces[1];
    assert!(
        todo.fields
            .iter()
            .any(|f| f.path == "fitems01" && f.stored == "collection")
    );
    assert!(todo.fields.iter().any(|f| f.path == "fitems01.*.ftext001"));
}

#[test]
fn the_surface_file_parses_as_the_receivers_read_it() {
    let meta = meta();
    let bytes = surface_toml(&[&meta]).unwrap();
    let file = collab_types_parse(&bytes);
    assert_eq!(file[0], HOST_KIND);
    assert!(file.contains(&"tl.so.tilo.counter.Counter".to_owned()));
}

/// The kinds of a surface file, read by the receiver's own parser.
fn collab_types_parse(bytes: &[u8]) -> Vec<String> {
    let text = std::str::from_utf8(bytes).unwrap();
    let value: toml::Value = toml::from_str(text).unwrap();
    value["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|resource| resource["kind"].as_str().unwrap().to_owned())
        .collect()
}

/// A planner that records what it was asked and plans one increment.
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
    let meta = meta();
    let planner: Arc<dyn Planner> = Arc::new(Recording);
    let resource = publish_record(&meta, "Todo", "t1", "Shopping", &planner);
    assert_eq!(resource.id.as_str(), resource_id("so.tilo.counter", "t1"));
    assert_eq!(resource.kind, "tl.so.tilo.counter.Todo");
    let add = resource
        .actions
        .iter()
        .find(|spec| spec.id == "aadd00001")
        .unwrap();
    assert_eq!(
        add.params().len(),
        1,
        "the subject is the resource, not a parameter"
    );
    let built = add
        .build_json(json!({"text": "milk"}).as_object().unwrap())
        .unwrap();
    assert_eq!(built.type_id, ACTION_TYPE);
    let registry = ActionRegistry::with(register_actions);
    assert!(registry.decode(&built).is_ok());
    // The contract kind follows the effect class.
    let counter = publish_record(&meta, "Counter", "c1", "Counter", &planner);
    let peek = counter
        .actions
        .iter()
        .find(|spec| spec.id == "alook001")
        .unwrap();
    assert_eq!(peek.contract.kind, ActionKind::Read);
    let package = publish_package(&meta, &planner);
    let announce = package
        .actions
        .iter()
        .find(|spec| spec.id == "asend001")
        .unwrap();
    assert_eq!(announce.contract.kind, ActionKind::Outside);
}

#[test]
fn the_host_publishes_blocks_flows_and_the_catalogue() {
    let host = host_resource();
    let ids: Vec<&str> = host.actions.iter().map(|spec| spec.id.as_str()).collect();
    assert_eq!(ids, ["run-block", "run-flow", "catalogue"]);
    assert_eq!(host.actions[2].contract.kind, ActionKind::Read);
}
