//! Edits bound to where someone is in the document — the caret and selection
//! of this window's person, or of an agent working beside them — as typed
//! builders of the document's action. The framework publishes them on the
//! document beside its declared actions, so the palette, agents and scripts
//! invoke them through the same door, permission check and undo as every
//! other edit.
//!
//! They are the document operations of the tree a view's caret offers
//! ([`super::live_presentation_resources`]), whose builders already capture
//! where they apply; each becomes a declaration that builds the action the
//! operation encodes.

use tilo_actions::{
    ActionArgs, ActionInput, ActionSpec, Collects, OperationKind, Resource, RiskLevel,
};
use ui::tilo_resource::ActionDecl;

use super::{RichTextAction, WORD_PROCESSOR_ACTION_TYPE, editing::op};
use crate::framework::Document;

/// Every edit bound to a view that may be published, by its stable id.
const BOUND: &[&str] = &[
    op::TYPE_TEXT,
    op::NEW_PARAGRAPH,
    op::DELETE,
    op::TOGGLE_MARK,
    op::SET_MARK,
    op::CLEAR_MARK,
    op::SET_PARAGRAPH_STYLE,
    op::APPLY_NAMED_STYLE,
    op::PASTE,
    op::APPLY_SPELLING_SUGGESTION,
    "place-image",
    "insert-page-break",
    "align-left",
    "align-center",
    "align-right",
    "align-justify",
    "insert-table",
    "set-hyperlink",
    "append-paragraph",
    "continue-paragraph",
    "replace-selection",
    "add-comment",
];

/// Bound edits that refine the document's declared action of the same id:
/// the append also leaves its writer's caret after the text, for the rest
/// of a streamed paragraph to be typed there.
const REFINES: &[&str] = &["append-paragraph"];

/// The edits the view's tree `resources` offers, as declarations: the
/// document operations of its top-level resources (the caret's and the
/// selection's), each once.
pub(crate) fn declarations(resources: &[Resource]) -> Vec<ActionDecl<Document>> {
    let mut decls: Vec<ActionDecl<Document>> = Vec::new();
    for spec in resources.iter().flat_map(|resource| &resource.actions) {
        if decls.iter().any(|decl| decl.id == spec.id) {
            continue;
        }
        if let Some(decl) = declaration(spec) {
            decls.push(decl);
        }
    }
    decls
}

fn declaration(spec: &ActionSpec) -> Option<ActionDecl<Document>> {
    if spec.kind != OperationKind::Document {
        return None;
    }
    let id = BOUND.iter().copied().find(|id| *id == spec.id)?;
    let names: Vec<String> = spec
        .params()
        .iter()
        .map(|param| param.name.to_string())
        .collect();
    let operation = spec.clone();
    let build = move |args: &ActionArgs, _now: i64| -> Result<RichTextAction, String> {
        let supplied = names
            .iter()
            .filter_map(|name| args.get(name).map(|value| (name.clone(), value.clone())));
        let encoded = operation
            .build_args(supplied)
            .map_err(|error| error.to_string())?;
        if encoded.type_id != WORD_PROCESSOR_ACTION_TYPE {
            return Err(String::from_utf8_lossy(&encoded.payload).into_owned());
        }
        serde_json::from_slice(&encoded.payload).map_err(|error| error.to_string())
    };
    let mut decl = ActionDecl::new(id, spec.label.clone(), spec.params().to_vec(), build)
        .requiring(spec.contract.permission);
    if let ActionInput::Prompted {
        prompt,
        collects: Collects::Content,
        ..
    } = &spec.input
    {
        decl = decl.collecting_content(prompt.clone());
    }
    if let Some(description) = &spec.description {
        decl = decl.describe(description.clone());
    }
    if spec.contract.risk == RiskLevel::Destructive {
        decl = decl.destructive();
    }
    if spec.contract.result != tilo_actions::ResultSpec::None {
        decl = decl.returns(spec.contract.result.clone());
    }
    for tag in &spec.contract.tags {
        decl = decl.tagged(tag.clone());
    }
    if REFINES.contains(&id) {
        decl = decl.refining();
    }
    Some(decl)
}
