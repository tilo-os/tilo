//! Stable document actions and resources for palettes, search and automation.
use crate::{
    doc::ProjectDoc,
    model::{Operation, Project},
};
use qos_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, ExecutionLocus, OperationKind, Resource,
    RetryPolicy,
};
pub const TYPE_ID: &str = "image-editor.set-project";
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
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SetProject(pub Project);
impl Action for SetProject {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        ProjectDoc::write(doc, &self.0);
        Ok(())
    }
    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}
pub fn register(r: &mut ActionRegistry) {
    r.register::<SetProject>(TYPE_ID);
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
        let mut next = project.clone();
        let node_id = next_operation_id(&next, id);
        if next.insert_before_output(
            &node_id,
            label.strip_prefix("Add ").unwrap_or(label),
            operation,
        ) && let Ok(spec) = ActionSpec::immediate(id, label, TYPE_ID, &SetProject(next))
        {
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
        let mut without = project.clone();
        if !without.remove_adjustment(id) {
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
        if let Ok(spec) =
            ActionSpec::immediate("remove", "Remove adjustment", TYPE_ID, &SetProject(without))
        {
            adjustment = adjustment.with_action(
                spec.reversible()
                    .with_description("Remove this step while keeping later steps connected"),
            );
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

fn next_operation_id(project: &Project, stem: &str) -> String {
    let mut index = 1_u64;
    loop {
        let id = format!("{stem}-{index}");
        if !project.graph.nodes.contains_key(&id) {
            return id;
        }
        index = index.saturating_add(1);
    }
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
    }
}
