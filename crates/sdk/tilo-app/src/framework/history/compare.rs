//! Bounded typed comparison through the resource's public content reader.
use serde::Deserialize;
use serde_json::{Value, json};
use tilo_actions::{
    ActionKind, ActionOutput, ActionParam, ActionSpec, EncodedAction, OperationKind, ParamType,
    ResultSpec,
};
use ui_core::automerge::AutoCommit;
use ui_core::automerge::transaction::Transactable;
use ui_core::history_source::steps;

use crate::framework::root::TypedRoot;

pub(crate) const TYPE: &str = "tilo.app.history-compare";
const MAX_ITEMS: usize = 32;

#[derive(Deserialize)]
struct Request {
    from: String,
    to: Option<String>,
    items: Option<Vec<String>>,
}

pub(crate) fn spec() -> ActionSpec {
    let mut spec = ActionSpec::typed_operation(
        "compare", "Compare content", "Compare content", OperationKind::View, TYPE,
        vec![
            ActionParam::required("from", ParamType::Text, super::VERSION),
            ActionParam::optional("to", ParamType::Text, "Later version (default current)").nullable(),
            ActionParam::optional("items", ParamType::Array, "Up to 32 published item ids; omit to compare the resource").nullable()
                .with_schema(json!({"type": "array", "minItems": 1, "maxItems": MAX_ITEMS, "items": {"type": "string", "maxLength": 512}})),
        ],
        |args| EncodedAction::encode_operation(TYPE, &json!({
            "from": args.text("from"), "to": args.text("to"), "items": args.array("items"),
        })).unwrap_or_else(|_| super::invalid("invalid comparison")),
    ).of_kind(ActionKind::Read).reads_document().with_description(
        "Compare frozen typed public content, with exact causal heads. Each side is present with content or explicitly absent; unreadable versions fail. The entire result is limited to 256 KiB.",
    );
    spec.contract.result = ResultSpec::Json {
        schema: json!({"type": "object"}),
    };
    spec
}

pub(crate) fn answer(
    typed: &dyn TypedRoot,
    doc: &AutoCommit,
    payload: &[u8],
) -> Result<ActionOutput, String> {
    let request: Request = tilo_actions::decode_json(payload).map_err(|error| error.to_string())?;
    if let Some(items) = &request.items {
        if items.is_empty()
            || items.len() > MAX_ITEMS
            || items
                .iter()
                .any(|id| id.is_empty() || id.len() > 512 || id.chars().any(char::is_control))
        {
            return Err("compare needs 1 to 32 nonempty item ids of at most 512 bytes".into());
        }
        let unique: std::collections::HashSet<_> = items.iter().collect();
        if unique.len() != items.len() {
            return Err("comparison item ids must be distinct".into());
        }
    }
    // Resolve each frontier once, before reading either side. No live state
    // cache or snapshot JSON participates in the content projection.
    let from = steps::resolve(doc, &request.from)?;
    let to = match request.to {
        Some(to) => steps::resolve(doc, &to)?,
        None => doc.base_heads(),
    };
    let before_doc = doc
        .clone()
        .fork_at(&from)
        .map_err(|error| error.to_string())?;
    let after_doc = doc
        .clone()
        .fork_at(&to)
        .map_err(|error| error.to_string())?;
    let before = typed.read(&before_doc).map_err(|error| error.to_string())?;
    let after = typed.read(&after_doc).map_err(|error| error.to_string())?;
    let ids: Vec<Option<&str>> = request.items.as_ref().map_or_else(
        || vec![None],
        |items| items.iter().map(|id| Some(id.as_str())).collect(),
    );
    let mut comparisons = Vec::new();
    for item in ids {
        let side = |state: &dyn std::any::Any, frozen: &AutoCommit| -> Result<Value, String> {
            Ok(match typed.comparison_content(state, frozen, item)? {
                Some(content) => json!({"status": "present", "content": content}),
                None => json!({"status": "absent"}),
            })
        };
        comparisons.push(json!({"item": item, "before": side(before.as_ref(), &before_doc)?, "after": side(after.as_ref(), &after_doc)?}));
    }
    Ok(ActionOutput {
        value: Some(super::review::bounded(json!({
            "from": super::version_name(&from), "to": super::version_name(&to),
            "from_heads": from.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "to_heads": to.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "comparisons": comparisons,
        }))?),
        resources: Vec::new(),
    })
}
