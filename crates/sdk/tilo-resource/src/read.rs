//! `read`: what a readable resource holds, as typed data. With no `item` it
//! answers the resource itself; with an `item` id, the nested resource of that
//! id; with a `version`, frozen historical content. It is a view: it changes
//! nothing, needs only read permission and is checked like every other action.
//!
//! Every app publishes the same `read`: a framework app's root, and a tilolang
//! host for each of its instances.

/// The operation type of `read`.
pub const TYPE: &str = "tilo.resource.read";

/// The `read` action, bound to the nested resource `item` when one is named
/// (its own `read`, without an `item` parameter).
#[must_use]
pub fn spec(item: Option<&str>) -> tilo_actions::ActionSpec {
    let item = item.map(str::to_owned);
    let mut spec = tilo_actions::ActionSpec::typed_operation(
        "read",
        "Read the content",
        "Read the content",
        tilo_actions::OperationKind::View,
        TYPE,
        vec![
            tilo_actions::ActionParam::optional(
                "item",
                tilo_actions::ParamType::Text,
                "A nested resource's id, as the first read lists it (default: the whole resource)",
            )
            .nullable(),
            tilo_actions::ActionParam::optional(
                "version",
                tilo_actions::ParamType::Text,
                "A change id, exact causal frontier, or checkpoint name (default: current)",
            )
            .nullable(),
        ]
        .into_iter()
        .filter(|param| item.is_none() || param.name != "item")
        .collect(),
        move |args| {
            tilo_actions::EncodedAction::encode_operation(
                TYPE,
                &serde_json::json!({ "item": item.as_deref().or_else(|| args.text("item")), "version": args.text("version") }),
            )
            .unwrap_or_else(|_| tilo_actions::EncodedAction {
                type_id: TYPE.into(),
                payload: Vec::new(),
            })
        },
    )
    .of_kind(tilo_actions::ActionKind::Read)
    .with_description(
        "What this holds as typed data: with no item, the resource and the ids of what it contains; with an item id, that item's content. With version, read frozen historical content and its exact heads without changing the live resource.",
    )
    // Typed data for a program to reason about; a person reads what the
    // app shows.
    .for_audience(tilo_actions::Audience::Agents);
    spec.type_mut().contract.result = tilo_actions::ResultSpec::Json {
        schema: serde_json::json!({"type": "object"}),
    };
    spec.reads_document()
}
