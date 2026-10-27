//! The agent surface as a document: one format, one version, and the rows
//! the app declares. What an agent, a script or a tool written in another
//! language reads of a running app, and what this build reads back from it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test assertions over exact documents"
)]

use serde_json::{Value, json};
use tilo_actions::agent_surface::{
    AGENT_SURFACE_FORMAT, AGENT_SURFACE_VERSION, AgentSurface, AgentSurfaceError,
};
use tilo_app::prelude::*;

/// Where a stamp goes: the parameter two actions take, which the surface
/// therefore states once.
#[derive(
    Default,
    Debug,
    Clone,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
pub struct Placement {
    /// Across, from the left edge.
    pub x: i64,
    /// Down, from the top edge.
    pub y: i64,
    /// What the stamp says.
    pub label: String,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Board {
    pub title: String,
}

#[resource(kind = "test.surface", label = "Board")]
impl Board {
    /// Retitle the board.
    #[action]
    pub fn rename(&mut self, title: String) {
        self.title = title;
    }

    /// Put a stamp at a placement.
    #[action]
    pub fn stamp(&mut self, at: Placement, ink: String) {
        let _ = (at, ink);
    }

    /// Draw a stroke from a placement.
    #[action]
    pub fn stroke(&mut self, from: Placement) {
        let _ = from;
    }
}

pub struct BoardApp;

impl ResourceApp for BoardApp {
    type Resource = Board;
    const ID: &'static str = "so.tilo.test-surface";
    const TITLE: &'static str = "Board";

    fn view(cx: &mut ViewCx<Board>) -> Element {
        Box::new(cx.button("Rename", Board::RENAME.with("a board")))
    }

    fn label(state: &Board) -> String {
        format!("Board of {}", state.title)
    }
}

fn surface() -> (TestApp<BoardApp>, AgentSurface) {
    let mut app = TestApp::<BoardApp>::new().expect("the app opens");
    let surface = app.agent_surface();
    (app, surface)
}

/// The surface is one document that names its format and version, and a tool
/// that writes it and reads it back is handed the surface it wrote.
#[test]
fn the_surface_is_one_versioned_document_a_tool_reads_back() {
    let (_app, surface) = surface();
    assert_eq!(surface.format, AGENT_SURFACE_FORMAT);
    assert_eq!(surface.version, AGENT_SURFACE_VERSION);
    let bytes = serde_json::to_vec(&surface).expect("the surface is JSON");
    assert_eq!(
        AgentSurface::read_json(&bytes).expect("a tool reads the document it wrote"),
        surface
    );
    assert_eq!(AgentSurface::read(&surface.document()).unwrap(), surface);
    assert_eq!(
        serde_json::to_vec(&AgentSurface::read_json(&bytes).unwrap()).unwrap(),
        bytes,
        "a reader hands back the bytes it read"
    );
    assert!(
        surface.rows().iter().any(|row| row["action"] == "stroke"),
        "the app's own verbs are published: {surface}"
    );
    // The two actions that take a placement state it once: each names the
    // same definition, and every reference either of them writes is one the
    // document carries.
    let placement = |action: &str, param: &str| -> Value {
        surface
            .row("test.surface", action)
            .unwrap_or_else(|| panic!("{action} is published: {surface}"))["params"]
            .as_array()
            .and_then(|params| {
                params
                    .iter()
                    .find(|published| published["name"] == param)
                    .and_then(|published| published.get("schema"))
                    .cloned()
            })
            .unwrap_or_else(|| panic!("{action} takes `{param}`: {surface}"))
    };
    assert_eq!(
        placement("stamp", "at")["$ref"],
        placement("stroke", "from")["$ref"],
        "two actions taking one shape name it once"
    );
    for reference in references(&Value::Array(surface.rows().to_vec())) {
        let name = reference
            .strip_prefix("#/$defs/")
            .unwrap_or_else(|| panic!("the surface names `{reference}`, which is local to it"));
        assert!(
            surface.definitions().contains_key(name),
            "the surface states `{name}`: {surface}"
        );
    }
}

/// A document of another version, or the prealpha bare array of rows, is
/// refused with what it is — never read as a surface with no rows.
#[test]
fn a_document_this_build_does_not_speak_is_refused() {
    let (_app, surface) = surface();
    let mut future = surface.document();
    future["version"] = json!(AGENT_SURFACE_VERSION + 1);
    assert_eq!(
        AgentSurface::read(&future),
        Err(AgentSurfaceError::UnsupportedVersion {
            found: u64::from(AGENT_SURFACE_VERSION) + 1
        })
    );
    let prealpha = json!(surface.rows());
    assert_eq!(
        AgentSurface::read(&prealpha),
        Err(AgentSurfaceError::PrealphaArray)
    );
    let unversioned = json!({
        "actions": surface.rows(),
        "$defs": serde_json::to_value(surface.definitions()).unwrap(),
    });
    assert!(matches!(
        AgentSurface::read(&unversioned),
        Err(AgentSurfaceError::Malformed { .. })
    ));
}

/// Every row is the descriptor the app declares, and a parameter's published
/// schema admits and refuses what the declaration's does: saying a shape once
/// is a representation of the contract, not a different one.
#[test]
fn every_row_is_the_descriptor_the_app_declares() {
    let (mut app, surface) = surface();
    let descriptors = app.actions().expect("the app publishes its actions");
    assert!(!descriptors.is_empty(), "the app publishes its actions");
    let mut compared = 0_usize;
    let mut probes = 0_usize;
    for descriptor in &descriptors {
        let resource = descriptor.address.resource.as_str();
        let action = descriptor.address.action.as_str();
        let row = surface
            .row(resource, action)
            .unwrap_or_else(|| panic!("{resource}#{action} is published: {surface}"));
        assert_eq!(row["resource"], resource);
        assert_eq!(row["label"], descriptor.action_label);
        assert_eq!(
            row["description"],
            serde_json::to_value(&descriptor.description).unwrap(),
            "{resource}#{action} keeps its description"
        );
        assert_eq!(
            row["operation"],
            serde_json::to_value(descriptor.kind).unwrap(),
            "{resource}#{action} keeps its operation"
        );
        let published = row["params"].as_array().cloned().unwrap_or_default();
        let declared = serde_json::to_value(&descriptor.params).unwrap();
        let declared = declared.as_array().cloned().unwrap_or_default();
        assert_eq!(
            published.len(),
            declared.len(),
            "{resource}#{action} publishes every declared parameter"
        );
        for (published, declared) in published.iter().zip(&declared) {
            for key in [
                "name",
                "ty",
                "required",
                "nullable",
                "description",
                "choices",
            ] {
                assert_eq!(
                    published.get(key),
                    declared.get(key),
                    "{resource}#{action} `{}` keeps its {key}",
                    published["name"]
                );
            }
            let (Some(published), Some(declared)) =
                (published.get("schema"), declared.get("schema"))
            else {
                assert_eq!(published.get("schema"), declared.get("schema"));
                continue;
            };
            let published = surface.scoped(published);
            for probe in probes_of(declared) {
                assert_eq!(
                    tilo_actions::validate_json_schema(&probe, &published).is_ok(),
                    tilo_actions::validate_json_schema(&probe, declared).is_ok(),
                    "{resource}#{action} admits {probe} the same way"
                );
                probes = probes.saturating_add(1);
            }
            compared = compared.saturating_add(1);
        }
    }
    assert_eq!(
        compared,
        descriptors
            .iter()
            .map(|descriptor| descriptor
                .params
                .iter()
                .filter(|param| param.schema.is_some())
                .count())
            .sum::<usize>(),
        "every declared parameter with a schema is compared in both representations"
    );
    assert!(
        probes > 0,
        "{probes} probes compare the two representations"
    );
}

/// A surface narrowed to the rows one caller may have states only the
/// definitions those rows name: a shape that only the dropped rows took is
/// gone, and what remains still resolves.
#[test]
fn a_narrowed_surface_states_only_what_its_rows_name() {
    let (_app, surface) = surface();
    let mut renamed = surface.clone();
    renamed.retain(|row| row["action"] == "rename");
    assert_eq!(renamed.rows().len(), 1, "{renamed}");
    assert!(
        renamed.definitions().is_empty(),
        "the placement only the dropped rows took goes with them: {renamed}"
    );

    let mut stroked = surface;
    stroked.retain(|row| row["action"] == "stroke");
    assert_eq!(stroked.definitions().len(), 1, "{stroked}");
    let schema = stroked.rows()[0]["params"][0]["schema"].clone();
    assert_eq!(
        stroked.scoped(&schema)["$defs"],
        serde_json::to_value(stroked.definitions()).unwrap(),
        "the kept row still names the shape the document states"
    );
}

/// Every `$ref` `node` carries.
fn references(node: &Value) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        match node {
            Value::Object(fields) => {
                for (key, value) in fields {
                    if key == "$ref"
                        && let Some(reference) = value.as_str()
                    {
                        found.push(reference.to_owned());
                    }
                    pending.push(value);
                }
            }
            Value::Array(items) => pending.extend(items),
            _ => {}
        }
    }
    found
}

/// Values to hold a published schema and its declaration to: the examples it
/// publishes, some of them broken, and the shapes these actions take.
fn probes_of(schema: &Value) -> Vec<Value> {
    let examples = schema.get("examples").and_then(Value::as_array).cloned();
    let mut probes: Vec<Value> = examples.unwrap_or_default();
    probes.extend([
        Value::Null,
        json!(true),
        json!(0),
        json!(-1),
        json!("text"),
        json!([]),
        json!({}),
        json!({"x": 0, "y": 0, "label": "here"}),
        json!({"x": 0, "y": 0}),
        json!({"x": "no", "y": 0, "label": "here"}),
        json!({"x": 0, "y": 0, "label": "here", "elsewhere": 1}),
    ]);
    probes
}
