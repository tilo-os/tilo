//! `facts`: the framework's read of what a resource that states facts
//! ([`Resource::HAS_FACTS`](tilo_resource::Resource::HAS_FACTS)) holds, as a
//! compact summary to check it against a request.

use automerge::AutoCommit;

use super::typed::TypedRoot;

/// The operation type of the framework's `facts` view.
pub(crate) const FACTS_TYPE: &str = "tilo.resource.facts";

/// The id agents and their hosts know the read by.
pub(crate) const FACTS: &str = "facts";

/// `facts`: a view, changing nothing and needing only read permission.
pub(crate) fn facts_spec() -> tilo_actions::ActionSpec {
    let mut spec = tilo_actions::ActionSpec::typed_operation(
        FACTS,
        "Read the facts",
        "Read the facts",
        tilo_actions::OperationKind::View,
        FACTS_TYPE,
        vec![
            tilo_actions::ActionParam::optional(
                "version",
                tilo_actions::ParamType::Text,
                "A change id, exact causal frontier, or checkpoint name (default: current)",
            )
            .nullable(),
        ],
        |args| {
            tilo_actions::EncodedAction::encode_operation(
                FACTS_TYPE,
                &serde_json::json!({ "version": args.text("version") }),
            )
            .unwrap_or_else(|_| tilo_actions::EncodedAction {
                type_id: FACTS_TYPE.into(),
                payload: Vec::new(),
            })
        },
    )
    .of_kind(tilo_actions::ActionKind::Read)
    .with_description(
        "What this holds as a compact summary of plain values (names, times, counts, text), to check it against a request or compare two versions. With version, the facts as of that version.",
    );
    spec.contract.result = tilo_actions::ResultSpec::Json {
        schema: serde_json::json!({"type": "object"}),
    };
    spec.reads_document()
}

/// Answer a `facts` request against the committed document of the resource
/// `typed` reads, whose state is `state`.
pub(crate) fn answer(
    typed: &dyn TypedRoot,
    state: &dyn std::any::Any,
    doc: &AutoCommit,
    payload: &[u8],
) -> Result<tilo_actions::ActionOutput, String> {
    let args: serde_json::Value = if payload.is_empty() {
        serde_json::Value::Null
    } else {
        tilo_actions::decode_json(payload).map_err(|error| error.to_string())?
    };
    let value = match args.get("version").and_then(serde_json::Value::as_str) {
        Some(version) => {
            let heads = ui_core::history_source::steps::resolve(doc, version)?;
            let frozen = doc
                .clone()
                .fork_at(&heads)
                .map_err(|error| error.to_string())?;
            let past = typed.read(&frozen).map_err(|error| error.to_string())?;
            typed.facts(past.as_ref(), &frozen)?
        }
        None => typed.facts(state, doc)?,
    };
    Ok(tilo_actions::ActionOutput {
        value: Some(value),
        resources: Vec::new(),
    })
}
