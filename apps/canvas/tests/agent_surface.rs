//! The agent's view of Canvas, kept as a golden file: every published
//! action, its description and parameter schema. A change to the
//! vocabulary shows up here as a diff to review, with its size in context.
//!
//! Update after an intended change:
//! `TILO_UPDATE_GOLDEN=1 cargo test -p canvas --test agent_surface`.
#![allow(
    clippy::unwrap_used,
    reason = "test assertions over exact, hand-written fixtures"
)]

use canvas::App;
use serde_json::{Value, json};
use ui::prelude::TestApp;
use ui::tilo_actions::AgentSurface;

/// The most the whole surface may weigh, in bytes of JSON: what an agent
/// reads before it can act. Growing past it is a design decision, not drift.
///
/// The document is `tilo_actions::agent_surface::AgentSurface`: `format` and
/// `version` say what a reader is reading, `actions` holds the rows, and
/// `$defs` holds the shapes their parameter schemas repeat, which each of
/// them names with a local `$ref` and which `tilo_actions::params` resolves.
/// So a shape several actions take — a region, a pair of units, the
/// operations a field edit takes — is stated once for all of them, and every
/// constraint, description, example and path is still there: an agent
/// resolves it, it does not read less.
///
/// `apply` and `group` name the actions they repeat; the field-edit listing
/// names each editable path once with its operations and value kind. A
/// parameter's schema does not repeat its description, nor a description
/// its label. Document-scoped definitions keep all 72 actions, their
/// constraints and all 150 editable paths inside the existing budget,
/// including the paths of a mark's style, adjustments and shape.
const BUDGET: usize = 68 * 1024;

#[test]
fn the_agent_surface_matches_its_golden_file_and_budget() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    // The inspector slides in as the window opens; its controls are part of
    // what an agent sees once it has.
    let _ = app.harness().render_rgba();
    assert!(
        app.harness()
            .settle_for_capture(tilo_test_env::hang_bound()),
        "the window settles"
    );
    let surface = app.agent_surface();
    assert_eq!(surface.format, ui::tilo_actions::AGENT_SURFACE_FORMAT);
    assert_eq!(surface.version, ui::tilo_actions::AGENT_SURFACE_VERSION);
    let text = serde_json::to_string_pretty(&surface).unwrap() + "\n";
    let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/agent-surface.json");
    if std::env::var_os("TILO_UPDATE_GOLDEN").is_some() {
        std::fs::write(&golden, &text).unwrap();
    }
    // The current surface beside the build output, to review or promote
    // when the test ran on another machine.
    let current =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/agent-surface");
    std::fs::create_dir_all(&current).unwrap();
    std::fs::write(current.join("canvas.json"), &text).unwrap();
    let compact = serde_json::to_string(&surface).unwrap().len();
    assert!(
        compact <= BUDGET,
        "the agent surface is {compact} bytes, over the {BUDGET}-byte budget"
    );
    let expected = std::fs::read_to_string(&golden).unwrap_or_default();
    assert!(
        expected == text,
        "the agent surface changed ({compact} bytes); review the diff and update with TILO_UPDATE_GOLDEN=1:\n{text}"
    );
    // The committed golden is a document this build reads: an agent handed
    // the file, rather than the app, is handed the same surface, at the
    // version it says it is. A golden a reader refuses would be a snapshot
    // nobody could act on.
    let committed = ui::tilo_actions::AgentSurface::read_json(expected.as_bytes())
        .expect("the golden file is the surface this build writes");
    assert_eq!(committed, surface);
    assert_shapes_are_shared(&surface);
    assert_narrowed_surface_states_only_what_its_rows_name(&surface);
    assert_shapes_admit_what_the_declarations_do(&surface, &mut app);
    assert_header_inventory(&surface, &mut app);
    let actions: Vec<&str> = rows(&surface)
        .iter()
        .filter(|row| row["resource_kind"] == "canvas.artwork")
        .filter_map(|row| row["action"].as_str())
        .collect();
    for action in [
        "paint",
        "draw",
        "edit-marks",
        "look",
        "inspect",
        "media",
        "apply",
        "history",
        "compare-versions",
        "undo",
        "redo",
        "restore",
        "checkpoint",
        "render",
    ] {
        assert!(
            actions.contains(&action),
            "{action} is published: {actions:?}"
        );
    }
    assert!(
        rows(&surface)
            .iter()
            .any(|row| row["resource_kind"] == "tilo.control.segmented"),
        "the tool rail is reachable"
    );
    // The vocabulary is the one it was: nothing was dropped to fit the
    // budget, only said once.
    assert_eq!(
        rows(&surface).len(),
        72,
        "the surface publishes the rows it did"
    );
    let editable: usize = rows(&surface)
        .iter()
        .filter(|row| row["action"] == "field-edit")
        .filter_map(|row| row["params"][0]["schema"]["description"].as_str())
        .map(|listing| {
            listing
                .lines()
                .filter(|line| line.starts_with("- `"))
                .count()
        })
        .sum();
    assert_eq!(editable, 150, "every editable path is still named");
}

/// A surface narrowed to one resource kind — what a caller allowed to see
/// that much is handed — states only the definitions its own rows name, and
/// every `$ref` it still writes is one it carries: filtering a surface never
/// leaves the shapes, names or descriptions of what was filtered out.
fn assert_narrowed_surface_states_only_what_its_rows_name(surface: &AgentSurface) {
    let kinds: std::collections::BTreeSet<&str> = surface
        .rows()
        .iter()
        .filter_map(|row| row["resource_kind"].as_str())
        .collect();
    assert!(kinds.len() > 1, "the surface publishes several kinds");
    for kind in kinds {
        let mut narrowed = surface.clone();
        narrowed.retain(|row| row["resource_kind"] == kind);
        assert!(!narrowed.rows().is_empty(), "{kind} has rows");
        assert!(
            narrowed
                .rows()
                .iter()
                .all(|row| row["resource_kind"] == kind),
            "only {kind}'s rows remain"
        );
        let mut named = std::collections::BTreeSet::new();
        let mut pending: Vec<String> = references_in(&Value::Array(narrowed.rows().to_vec()));
        while let Some(reference) = pending.pop() {
            let name = reference
                .strip_prefix("#/$defs/")
                .unwrap_or_else(|| panic!("{kind} narrowed a schema naming `{reference}`"));
            assert!(
                narrowed.definitions().contains_key(name),
                "{kind} keeps the definition `{name}` its rows name"
            );
            if named.insert(name.to_owned()) {
                pending.extend(references_in(&narrowed.definitions()[name]));
            }
        }
        let unkept: Vec<&String> = narrowed
            .definitions()
            .keys()
            .filter(|name| !named.contains(*name))
            .collect();
        assert!(
            unkept.is_empty(),
            "{kind} states no definition its rows do not name: {unkept:?}"
        );
    }
}

/// The rows of the surface document.
fn rows(surface: &AgentSurface) -> &[Value] {
    surface.rows()
}

/// Every shape a schema names is one the surface states once, and every
/// definition is a schema the validator reads: a reference that a reader
/// cannot resolve would be a contract nobody can hold.
fn assert_shapes_are_shared(surface: &AgentSurface) {
    let definitions = surface.definitions();
    assert!(!definitions.is_empty(), "the surface shares its shapes");
    for (name, definition) in definitions {
        // A definition may name another; the document is what states them.
        let definition = surface.scoped(definition);
        ui::tilo_actions::params::validate_schema_definition(&definition)
            .unwrap_or_else(|reason| panic!("definition `{name}` is a schema: {reason}"));
    }
    let mut references = 0_usize;
    for (name, schema) in schemas(surface) {
        let schema = surface.scoped(&schema);
        ui::tilo_actions::params::validate_schema_definition(&schema)
            .unwrap_or_else(|reason| panic!("`{name}` publishes a schema: {reason}"));
        for reference in references_in(&schema) {
            let definition = reference.strip_prefix("#/$defs/").unwrap_or_else(|| {
                panic!("`{name}` names `{reference}`, which is local to this document")
            });
            assert!(
                definitions.contains_key(definition),
                "`{name}` names `{definition}`, which the surface states"
            );
            references = references.saturating_add(1);
        }
    }
    assert!(references > 0, "the surface names what it states once");
}

/// Every `$ref` a schema carries, in the order it meets them.
fn references_in(schema: &Value) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![schema];
    while let Some(schema) = pending.pop() {
        match schema {
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

/// Each `(resource, action, parameter)` a schema belongs to.
fn schemas(surface: &AgentSurface) -> Vec<(String, Value)> {
    let mut found = Vec::new();
    for row in rows(surface) {
        for param in row["params"].as_array().into_iter().flatten() {
            let Some(schema) = param.get("schema") else {
                continue;
            };
            found.push((
                format!(
                    "{} {} `{}`",
                    row["resource"].as_str().unwrap_or_default(),
                    row["action"].as_str().unwrap_or_default(),
                    param["name"].as_str().unwrap_or_default()
                ),
                schema.clone(),
            ));
        }
    }
    found
}

/// A published schema and the declaration it stands for admit the same
/// values: sharing a shape is a representation of the contract, not a
/// different one.
fn assert_shapes_admit_what_the_declarations_do(surface: &AgentSurface, app: &mut TestApp<App>) {
    let declarations: Vec<Value> = app
        .actions()
        .expect("the app publishes its actions")
        .into_iter()
        .map(|descriptor| {
            (
                descriptor.address.resource.as_str().to_owned(),
                descriptor.address.action.clone(),
                serde_json::to_value(&descriptor.params).unwrap(),
            )
        })
        .collect::<Vec<(String, String, Value)>>()
        .into_iter()
        .map(|(resource, action, params)| {
            json!({"resource": resource, "action": action, "params": params})
        })
        .collect();
    let mut compared = 0_usize;
    let mut compared_actions = 0_usize;
    for row in rows(surface) {
        let resource = row["resource"].as_str().unwrap_or_default();
        let action = row["action"].as_str().unwrap_or_default();
        // A host provider's row is published by the window this one observes,
        // not declared by the app: `actions()` answers the app's own.
        let Some(declared) = declarations
            .iter()
            .find(|declared| declared["resource"] == resource && declared["action"] == action)
        else {
            continue;
        };
        compared_actions = compared_actions.saturating_add(1);
        let published = row["params"].as_array().cloned().unwrap_or_default();
        let declared = declared["params"].as_array().cloned().unwrap_or_default();
        assert_eq!(
            published.len(),
            declared.len(),
            "{resource} {action} publishes every declared parameter"
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
                    "{resource} {action} `{}` keeps its {key}",
                    published["name"]
                );
            }
            let (Some(published_schema), Some(declared_schema)) =
                (published.get("schema"), declared.get("schema"))
            else {
                assert_eq!(
                    published.get("schema"),
                    declared.get("schema"),
                    "{resource} {action} `{}` publishes the schema it declares",
                    published["name"]
                );
                continue;
            };
            // One parameter's schema names what the document states once.
            let published_schema = surface.scoped(published_schema);
            for probe in probes(&published_schema) {
                let here = ui::tilo_actions::validate_json_schema(&probe, &published_schema);
                let there = ui::tilo_actions::validate_json_schema(&probe, declared_schema);
                assert_eq!(
                    here.is_ok(),
                    there.is_ok(),
                    "{resource} {action} `{}` admits {probe} the same way: {here:?} against the published schema, {there:?} against the declaration",
                    published["name"]
                );
                compared = compared.saturating_add(1);
            }
        }
    }
    assert_eq!(
        compared_actions,
        declarations.len(),
        "every action the app declares is compared in both representations"
    );
    assert!(
        compared > 1_000,
        "{compared} probes compare the two representations"
    );
}

/// Values to hold a published schema and its declaration to: the examples
/// it publishes, some of them broken, and the shapes every schema here has a
/// case for — a nested array, an alternative, a bound, a required key, an
/// unknown property.
fn probes(schema: &Value) -> Vec<Value> {
    let mut probes: Vec<Value> = schema
        .get("examples")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .flat_map(|example| {
            let broken = match &example {
                Value::Object(fields) => {
                    let mut missing = fields.clone();
                    missing.shift_remove(fields.keys().next().unwrap_or(&String::new()));
                    let mut unknown = fields.clone();
                    unknown.insert("unexpected".into(), json!(true));
                    vec![Value::Object(missing), Value::Object(unknown)]
                }
                _ => Vec::new(),
            };
            std::iter::once(example).chain(broken)
        })
        .collect();
    probes.extend([
        Value::Null,
        json!(true),
        json!(0),
        json!(1),
        json!(-1),
        json!(1.5),
        json!(""),
        json!("text"),
        json!([]),
        json!([1, 2]),
        json!([1, 2, 3]),
        json!([1, 2, 3, 4]),
        json!([[1, 2], [3, 4]]),
        json!([[[1, 2]], [[3, 4]]]),
        json!({}),
        json!({"field": "title", "op": "set", "value": "x"}),
        json!({"field": "title", "op": "unknown"}),
        json!({"kind": "rect"}),
        json!({"kind": "unknown"}),
        json!({"points": [[0, 0], [1, 1]], "size": 2}),
        json!({"points": [[0, 0, 1]], "size": 2}),
        json!({"x": 0, "y": 0, "width": 4, "height": 4}),
        json!({"x": "no", "y": 0, "width": 4, "height": 4}),
    ]);
    probes
}

fn assert_header_inventory(surface: &AgentSurface, app: &mut TestApp<App>) {
    // Header controls remain real AT controls even where the artwork also
    // exposes their canonical commands. Suppressing their semantic leaves
    // creates orphan bindings; their public UI door is chiefly native focus.
    let header = [
        "canvas%2Dadd%2Dpicture",
        "canvas%2Dexport%2Dtitle",
        "canvas%2Dmenu",
        "canvas%2Dzoom%2Dfit",
        "canvas%2Dzoom%2Din",
        "canvas%2Dzoom%2Dout",
        "undo",
        "redo",
    ];
    let rows = rows(surface);
    let controls: std::collections::BTreeSet<_> = rows
        .iter()
        .filter(|row| row["resource_kind"] == "tilo.ui.control")
        .filter_map(|row| row["resource"].as_str())
        .filter(|resource| !resource.contains("nav%2Dshell%2D"))
        .collect();
    let expected: std::collections::BTreeSet<_> = header
        .iter()
        .map(|id| format!("ui:root%7Ek%3Acanvas%2Dshell%2Ewindow%7Ek%3A{id}"))
        .collect();
    assert_eq!(
        controls,
        expected
            .iter()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        "exact native header inventory, with no unrelated controls"
    );
    for resource in &controls {
        assert_eq!(
            rows.iter()
                .filter(|row| row["resource"] == *resource
                    && row["action"] == "focus"
                    && row["operation"] == "View")
                .count(),
            1,
            "each header control has exactly one actual focus door: {resource}"
        );
    }
    let documents: Vec<_> = rows
        .iter()
        .filter(|row| row["operation"] == "Document")
        .collect();
    let addresses: std::collections::BTreeSet<_> = documents
        .iter()
        .map(|row| {
            (
                row["resource"].as_str().unwrap(),
                row["action"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        addresses.len(),
        documents.len(),
        "document commands are never duplicated"
    );
    assert!(
        documents
            .iter()
            .all(|row| row["resource_kind"] != "tilo.ui.control"),
        "native header controls never mint alternate document mutation doors"
    );
    let bindings = ui::widget::controls::control_action_bindings(app.harness().root_widget());
    for command in ["undo", "redo"] {
        assert!(bindings.iter().any(|binding| binding.control_id == command && matches!(
            &binding.kind, ui::widget::ControlBindingKind::Action(address) if address.action == command
        )), "the actual header retains canonical {command}: {bindings:#?}");
    }
    let audit = app.harness().native_app_audit();
    assert!(
        audit.is_certified(),
        "the actual semantic leaves match their bindings: {audit:#?}"
    );
}
