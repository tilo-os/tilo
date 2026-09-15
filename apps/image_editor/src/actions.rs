//! Stable document actions and resources for palettes, search and automation.
use crate::{doc::ProjectDoc, model::Project};
use qos_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, ExecutionLocus, OperationKind, Resource,
    RetryPolicy,
};
pub const TYPE_ID: &str = "image-editor.set-project";
pub const OPERATION_TYPE: &str = "image-editor.operation";
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum EditorOperation {
    Open,
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
    let mut r = Resource::new(
        "image-editor.project",
        "image-editor.project",
        &project.name,
    );
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
    for (id, node) in &project.graph.nodes {
        if id == "source" || id == "output" {
            continue;
        }
        let mut without = project.clone();
        without.graph.remove(id);
        without.operations.remove(id);
        if let Ok(spec) = ActionSpec::immediate(
            &format!("remove-{id}"),
            &format!("Remove {}", node.title),
            TYPE_ID,
            &SetProject(without),
        ) {
            r = r.with_action(spec.reversible());
        }
    }
    r
}
pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<EditorOperation>, String> {
    if type_id != OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|e| e.to_string())
}
