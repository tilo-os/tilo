//! Stable document actions and resources for palettes, search and automation.
use crate::{
    doc::ProjectDoc,
    model::{Operation, Project},
};
use tilo_actions::{
    Action, ActionArgs, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, OperationKind, ParamType, Resource, RetryPolicy,
};
use ui::graph_editor::Edge;
pub const ADD_ADJUSTMENT_TYPE: &str = "image-editor.add-adjustment";
pub const REMOVE_ADJUSTMENT_TYPE: &str = "image-editor.remove-adjustment";
pub const UPDATE_ADJUSTMENT_TYPE: &str = "image-editor.update-adjustment";
pub const RECONNECT_INPUT_TYPE: &str = "image-editor.reconnect-input";
pub const OPERATION_TYPE: &str = "image-editor.operation";
pub const PROJECT_RESOURCE_ID: &str = "image-editor.project";
pub const SOURCE_RESOURCE_ID: &str = "image-editor.source";
pub const EXPORT_RESOURCE_ID: &str = "image-editor.export";

pub fn adjustment_resource_id(id: &str) -> String {
    format!("image-editor.adjustment:{id}")
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum EditorOperation {
    Open,
    Export,
    /// Navigation is deliberately a typed view operation. Search, history and
    /// assistive technology can reveal exactly the same adjustment that the
    /// visible stack does without being handed a stale UI callback.
    Select {
        id: String,
    },
}
pub fn register(r: &mut ActionRegistry) {
    r.register::<AddAdjustment>(ADD_ADJUSTMENT_TYPE);
    r.register::<RemoveAdjustment>(REMOVE_ADJUSTMENT_TYPE);
    r.register::<UpdateAdjustment>(UPDATE_ADJUSTMENT_TYPE);
    r.register::<ReconnectInput>(RECONNECT_INPUT_TYPE);
}

/// The normal collaborative graph insertion action.  It names the stable
/// node ID up front, so replays are idempotent and peers never infer identity
/// from presentation order or a toolbar callback.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AddAdjustment {
    pub id: String,
    pub title: String,
    pub operation: Operation,
}

impl Action for AddAdjustment {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let _ = ProjectDoc::add_adjustment(doc, &self.id, &self.title, &self.operation);
        Ok(())
    }
    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Delete is a tombstone rather than a whole-project replacement.  It is
/// idempotent, delete-wins against parameter edits, and preserves a stable
/// resource address for history even after the visible resource disappears.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RemoveAdjustment {
    pub id: String,
}
impl Action for RemoveAdjustment {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let _ = ProjectDoc::remove_adjustment(doc, &self.id);
        Ok(())
    }
    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum AdjustmentUpdate {
    Number { field: String, value: f64 },
    Position { x: f64, y: f64 },
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct UpdateAdjustment {
    pub id: String,
    pub update: AdjustmentUpdate,
}
impl Action for UpdateAdjustment {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let applied = match &self.update {
            AdjustmentUpdate::Number { field, value } => {
                ProjectDoc::set_adjustment_number(doc, &self.id, field, *value)
            }
            AdjustmentUpdate::Position { x, y } => {
                ProjectDoc::set_adjustment_position(doc, &self.id, *x, *y)
            }
        };
        if applied {
            Ok(())
        } else {
            Err(ActionError::Refused(
                "that adjustment or parameter is no longer editable".into(),
            ))
        }
    }
    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Expert graph wiring changes only the chosen target input. This is a
/// first-class collaborative action rather than a replacement of the entire
/// graph, preserving independent layout and adjustment edits from peers.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ReconnectInput {
    pub edge: Edge,
}
impl Action for ReconnectInput {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        if ProjectDoc::set_connection(doc, &self.edge) {
            Ok(())
        } else {
            Err(ActionError::Refused(
                "that graph connection is no longer editable".into(),
            ))
        }
    }
    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}
pub fn resource(project: &Project) -> Resource {
    let mut r = Resource::new(PROJECT_RESOURCE_ID, "image-editor.project", &project.name);
    if let Ok(open) = ActionSpec::operation(
        "open",
        "Open image",
        OperationKind::Effect,
        OPERATION_TYPE,
        &EditorOperation::Open,
    ) {
        r = r.with_action(
            open.retry_policy(RetryPolicy::Idempotent)
                .with_description("Choose a PNG, JPEG or WebP using the system file picker"),
        );
    }
    if !source_path(project).is_empty()
        && let Ok(export) = ActionSpec::operation(
            "export",
            "Export PNG",
            OperationKind::Effect,
            OPERATION_TYPE,
            &EditorOperation::Export,
        )
    {
        r = r.with_action(
            export
                .retry_policy(RetryPolicy::Idempotent)
                .with_description(
                    "Render the current non-destructive graph beside its source image",
                ),
        );
    }

    // Adding is an ordinary document action, not a private toolbar shortcut:
    // collaborators, history and agents all apply the identical graph edit.
    for (id, label, operation) in [
        (
            "exposure",
            "Add exposure",
            Operation::Exposure { stops: 0.5 },
        ),
        (
            "contrast",
            "Add contrast",
            Operation::Contrast { amount: 0.15 },
        ),
        (
            "colour",
            "Add colour",
            Operation::Saturation { amount: 1.2 },
        ),
        ("mono", "Add black and white", Operation::Grayscale),
        (
            "rotate",
            "Add rotate",
            Operation::Rotate { quarter_turns: 1 },
        ),
        (
            "crop",
            "Add crop",
            Operation::Crop {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            },
        ),
    ] {
        // An action is constructed before it is dispatched, so mint its
        // permanent identity here. The payload carries the same ID on every
        // retry/replay; it is never inferred from a visible list position.
        let node_id = format!("{id}-{}", uuid::Uuid::new_v4());
        if let Ok(spec) = ActionSpec::immediate(
            id,
            label,
            ADD_ADJUSTMENT_TYPE,
            &AddAdjustment {
                id: node_id,
                title: label.strip_prefix("Add ").unwrap_or(label).into(),
                operation,
            },
        ) {
            r = r
                .with_action(spec.reversible().with_description(
                    "Add a non-destructive adjustment to the shared image graph",
                ));
        }
    }

    let mut source = Resource::new(
        SOURCE_RESOURCE_ID,
        "image-editor.source",
        source_label(project),
    );
    if let Ok(select) = ActionSpec::operation(
        "select",
        "Reveal source",
        OperationKind::View,
        OPERATION_TYPE,
        &EditorOperation::Select {
            id: "source".into(),
        },
    ) {
        source = source.with_action(select.with_description("Reveal the original image source"));
    }
    r = r.with_child(source);

    for (id, node) in &project.graph.nodes {
        if id == "source" || id == "output" {
            continue;
        }
        let resource_id = adjustment_resource_id(id);
        let mut adjustment = Resource::new(&resource_id, "image-editor.adjustment", &node.title);
        if let Ok(select) = ActionSpec::operation(
            "select",
            "Reveal adjustment",
            OperationKind::View,
            OPERATION_TYPE,
            &EditorOperation::Select { id: id.clone() },
        ) {
            adjustment = adjustment.with_action(
                select.with_description("Reveal this adjustment and its editable controls"),
            );
        }
        for edit in adjustment_parameter_actions(id, project.operations.get(id)) {
            adjustment = adjustment.with_action(edit.with_description(
                "Set one declared graph parameter without replacing any other adjustment",
            ));
        }
        adjustment = adjustment.with_action(
            position_action(id)
                .with_description("Move this graph node using shared document coordinates"),
        );
        if let Ok(spec) = ActionSpec::immediate(
            "remove",
            "Remove adjustment",
            REMOVE_ADJUSTMENT_TYPE,
            &RemoveAdjustment { id: id.clone() },
        ) {
            adjustment = adjustment.with_action(spec.destructive().with_description(
                "Permanently remove this step while keeping later steps connected",
            ));
        }
        r = r.with_child(adjustment);
    }
    let mut export = Resource::new(EXPORT_RESOURCE_ID, "image-editor.export", "Export PNG");
    if let Ok(select) = ActionSpec::operation(
        "reveal",
        "Reveal export",
        OperationKind::View,
        OPERATION_TYPE,
        &EditorOperation::Select {
            id: "output".into(),
        },
    ) {
        export = export.with_action(select.with_description("Reveal the final export stage"));
    }
    if !source_path(project).is_empty()
        && let Ok(run) = ActionSpec::operation(
            "export",
            "Export PNG",
            OperationKind::Effect,
            OPERATION_TYPE,
            &EditorOperation::Export,
        )
    {
        export = export.with_action(
            run.retry_policy(RetryPolicy::Idempotent)
                .with_description("Render the current graph beside its source image"),
        );
    }
    r.with_child(export)
}

pub fn source_path(project: &Project) -> &str {
    match project.operations.get("source") {
        Some(Operation::Source { path }) => path,
        _ => "",
    }
}

fn source_label(project: &Project) -> String {
    std::path::Path::new(source_path(project))
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("No source image")
        .into()
}

fn adjustment_parameter_actions(id: &str, operation: Option<&Operation>) -> Vec<ActionSpec> {
    let (field, label, minimum, maximum) = match operation {
        None => return Vec::new(),
        Some(Operation::Crop { .. }) => {
            return ["x", "y", "width", "height"]
                .into_iter()
                .map(|field| {
                    numeric_action(id, field, &format!("Set crop {field}"), 0.0, 1.0, false)
                })
                .collect();
        }
        Some(operation) => match operation {
            Operation::Exposure { .. } => ("stops", "Set exposure", -8.0, 8.0),
            Operation::Contrast { .. } => ("amount", "Set contrast", -2.0, 3.0),
            Operation::Saturation { .. } => ("amount", "Set colour", -2.0, 3.0),
            Operation::Rotate { .. } => ("quarter_turns", "Set rotation", 0.0, 3.0),
            Operation::Crop { .. } => unreachable!("crop returned above"),
            Operation::Source { .. } | Operation::Grayscale | Operation::Output => {
                return Vec::new();
            }
        },
    };
    vec![numeric_action(
        id,
        field,
        label,
        minimum,
        maximum,
        field == "quarter_turns",
    )]
}

fn numeric_action(
    id: &str,
    field: &str,
    label: &str,
    minimum: f64,
    maximum: f64,
    integer: bool,
) -> ActionSpec {
    let id = id.to_owned();
    let field = field.to_owned();
    ActionSpec::typed(
        format!("set-{field}"),
        label,
        "Value",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required(
                "value",
                if integer {
                    ParamType::Integer
                } else {
                    ParamType::Number
                },
                "Exact adjustment value",
            )
            .with_schema(
                serde_json::json!({"type":"number", "minimum":minimum, "maximum":maximum}),
            ),
        ],
        move |args: &ActionArgs| {
            encode_update(UpdateAdjustment {
                id: id.clone(),
                update: AdjustmentUpdate::Number {
                    field: field.clone(),
                    value: args.number("value").unwrap_or_default(),
                },
            })
        },
    )
}

fn position_action(id: &str) -> ActionSpec {
    let id = id.to_owned();
    ActionSpec::typed(
        "move",
        "Move node",
        "X coordinate",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required("x", ParamType::Number, "Graph X coordinate").with_schema(
                serde_json::json!({"type":"number","minimum":-10000.0,"maximum":10000.0}),
            ),
            ActionParam::required("y", ParamType::Number, "Graph Y coordinate").with_schema(
                serde_json::json!({"type":"number","minimum":-10000.0,"maximum":10000.0}),
            ),
        ],
        move |args: &ActionArgs| {
            encode_update(UpdateAdjustment {
                id: id.clone(),
                update: AdjustmentUpdate::Position {
                    x: args.number("x").unwrap_or_default(),
                    y: args.number("y").unwrap_or_default(),
                },
            })
        },
    )
}

fn encode_update(update: UpdateAdjustment) -> EncodedAction {
    EncodedAction::encode(UPDATE_ADJUSTMENT_TYPE, &update).unwrap_or_else(|_| EncodedAction {
        type_id: UPDATE_ADJUSTMENT_TYPE.into(),
        payload: Vec::new(),
    })
}

pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<EditorOperation>, String> {
    if type_id != OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;

    #[test]
    fn resource_tree_publishes_each_editable_step_with_canonical_actions() {
        let mut project = Project::blank();
        assert!(project.insert_before_output(
            "exposure-1",
            "Exposure",
            Operation::Exposure { stops: 0.5 },
        ));
        let root = resource(&project);
        assert!(root.actions.iter().any(|action| action.id == "open"));
        assert!(root.actions.iter().any(|action| action.id == "exposure"));
        let adjustment = root
            .children
            .iter()
            .find(|resource| resource.id.as_str() == adjustment_resource_id("exposure-1"))
            .expect("the adjustment is a first-class resource");
        assert!(
            adjustment
                .actions
                .iter()
                .any(|action| action.id == "select")
        );
        assert!(
            adjustment
                .actions
                .iter()
                .any(|action| action.id == "remove")
        );
        let remove = adjustment
            .actions
            .iter()
            .find(|action| action.id == "remove")
            .expect("adjustments publish their permanent removal action");
        assert!(!remove.contract.reversible);
        assert_eq!(remove.contract.risk, tilo_actions::RiskLevel::Destructive);
    }

    #[test]
    fn granular_actions_are_idempotent_and_do_not_replace_a_peer_project() {
        let mut doc = AutoCommit::new();
        let add = AddAdjustment {
            id: "exposure-agent-a".into(),
            title: "Exposure".into(),
            operation: Operation::Exposure { stops: 0.5 },
        };
        add.apply(&mut doc).unwrap();
        let once = ProjectDoc::read(&doc);
        add.apply(&mut doc).unwrap();
        assert_eq!(ProjectDoc::read(&doc), once);
        RemoveAdjustment { id: add.id.clone() }
            .apply(&mut doc)
            .unwrap();
        RemoveAdjustment { id: add.id }.apply(&mut doc).unwrap();
        assert!(
            !ProjectDoc::read(&doc)
                .operations
                .contains_key("exposure-agent-a")
        );
    }

    #[test]
    fn typed_parameter_surface_builds_json_and_touches_only_its_node() {
        let mut project = Project::blank();
        assert!(project.insert_before_output(
            "exposure-a",
            "Exposure",
            Operation::Exposure { stops: 0.5 }
        ));
        assert!(project.insert_before_output(
            "contrast-b",
            "Contrast",
            Operation::Contrast { amount: 0.2 }
        ));
        let root = resource(&project);
        let step = root
            .children
            .iter()
            .find(|child| child.id.as_str() == adjustment_resource_id("exposure-a"))
            .unwrap();
        let set = step
            .actions
            .iter()
            .find(|action| action.id == "set-stops")
            .unwrap();
        let encoded = set
            .build_json(serde_json::json!({"value": 1.25}).as_object().unwrap())
            .unwrap();
        let mut registry = ActionRegistry::new();
        register(&mut registry);
        let mut doc = AutoCommit::new();
        ProjectDoc::write(&mut doc, &project);
        registry.decode(&encoded).unwrap().apply(&mut doc).unwrap();
        let updated = ProjectDoc::read(&doc);
        assert!(
            matches!(updated.operations["exposure-a"], Operation::Exposure { stops } if stops == 1.25)
        );
        assert!(
            matches!(updated.operations["contrast-b"], Operation::Contrast { amount } if amount == 0.2)
        );
    }

    #[test]
    fn crop_action_ids_are_unique_and_rotation_round_trips_as_integer() {
        let mut project = Project::blank();
        assert!(project.insert_before_output(
            "crop-a",
            "Crop",
            Operation::Crop {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0
            }
        ));
        assert!(project.insert_before_output(
            "rotate-a",
            "Rotate",
            Operation::Rotate { quarter_turns: 1 }
        ));
        let root = resource(&project);
        let crop = root
            .children
            .iter()
            .find(|child| child.id.as_str() == adjustment_resource_id("crop-a"))
            .unwrap();
        let ids = crop
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), crop.actions.len());
        for id in ["set-x", "set-y", "set-width", "set-height"] {
            assert!(ids.contains(id));
        }
        let rotate = root
            .children
            .iter()
            .find(|child| child.id.as_str() == adjustment_resource_id("rotate-a"))
            .unwrap();
        let set = rotate
            .actions
            .iter()
            .find(|action| action.id == "set-quarter_turns")
            .unwrap();
        let encoded = set
            .build_json(serde_json::json!({"value": 3}).as_object().unwrap())
            .unwrap();
        let mut registry = ActionRegistry::new();
        register(&mut registry);
        let mut doc = AutoCommit::new();
        ProjectDoc::write(&mut doc, &project);
        registry.decode(&encoded).unwrap().apply(&mut doc).unwrap();
        assert!(matches!(
            ProjectDoc::read(&doc).operations["rotate-a"],
            Operation::Rotate { quarter_turns: 3 }
        ));
    }
}
