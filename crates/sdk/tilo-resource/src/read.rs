//! `read`: what a readable resource holds, as typed data. With no `item` it
//! answers the resource itself; with an `item` id, the nested resource of that
//! id; with a `version`, frozen historical content. It is a view: it changes
//! nothing, needs only read permission and is checked like every other action.
//!
//! Every app publishes the same `read`: a framework app's root, and a tilolang
//! host for each of its instances.

use crate::{Id, Model, Resource};

/// The operation type of `read`.
pub const TYPE: &str = "tilo.resource.read";

/// The `read` action, bound to the nested resource `item` when one is named
/// (its own `read`, without an `item` parameter).
#[must_use]
pub fn spec(item: Option<&str>) -> tilo_actions::ActionSpec {
    spec_on(None, item)
}

/// The `read` of `record`, a record an app keeps beside its own (the
/// person's own record of a kind): the read names it, and the app's process
/// answers it from that record.
#[must_use]
pub fn record_spec(record: &str) -> tilo_actions::ActionSpec {
    spec_on(Some(record), None)
}

/// The record a `read` asked as `payload` names, when it is not the app's
/// own ([`record_spec`]).
#[must_use]
pub fn record_of(payload: &[u8]) -> Option<String> {
    let args: serde_json::Value = tilo_actions::decode_json(payload).ok()?;
    args.get("record")?.as_str().map(str::to_owned)
}

fn spec_on(record: Option<&str>, item: Option<&str>) -> tilo_actions::ActionSpec {
    let (record, item) = (record.map(str::to_owned), item.map(str::to_owned));
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
            let mut asked = serde_json::Map::new();
            asked.insert("item".into(), item.as_deref().or_else(|| args.text("item")).into());
            asked.insert("version".into(), args.text("version").into());
            if let Some(record) = &record {
                asked.insert("record".into(), record.as_str().into());
            }
            tilo_actions::EncodedAction::encode_operation(TYPE, &asked)
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

/// What a record holds, as the record's own model's JSON
/// ([`Model::to_json`]): every field it keeps, in the shape its own type
/// gives them. This is the body of the `read_content` of a record whose
/// fields are all it holds, so a caller with no window open — an agent, a
/// script, the SDK — reads it through the framework's `read`, as it reads
/// any app's record:
///
/// ```ignore
/// impl Calculator {
///     pub fn read_document(&self, item: Option<&Id>) -> Result<serde_json::Value, String> {
///         tilo_resource::read::fields(self, item)
///     }
/// }
/// ```
///
/// # Errors
/// A named `item` is not this read's to answer: the record reads whole, and
/// whatever its table reads of an item is answered from the table after this
/// read declines it ([`Resource::read_content`](crate::Resource::read_content)).
/// A record whose items it answers itself says so in a read of its own.
pub fn fields<M: Model>(record: &M, item: Option<&Id>) -> Result<serde_json::Value, String> {
    if item.is_some() {
        return Err(
            "this record reads whole; its items are read from its table, or by the read its own \
             type gives them"
                .into(),
        );
    }
    Ok(record.to_json())
}

/// What `state`, an `R` in `doc`, holds, or the part of it `item` names:
/// through `R`'s own [`Resource::read_content`] where it reads itself, else
/// from the record's model tables. The one way `read` is answered, by the
/// app's process and by a host that reads the record without it.
///
/// # Errors
/// Why the record, or the part named, does not answer reads.
pub fn content<R: Resource>(
    state: &R::State,
    doc: &crate::Doc,
    item: Option<&str>,
) -> Result<serde_json::Value, String> {
    let reads_item = |item: &str| {
        R::RECORD_TABLE.is_some_and(|table| crate::table::reads_item(table, R::KIND, item))
    };
    if R::READABLE {
        tilo_failure::ignored!(
            "the record table answers what the typed read did not",
            match R::read_content(state, doc, item) {
                Ok(content) => return Ok(content),
                Err(error) if !item.is_some_and(reads_item) => return Err(error),
                Err(_) => {}
            }
        )
    }
    let table = R::RECORD_TABLE
        .ok_or_else(|| format!("a {} does not answer reads of its parts", R::LABEL))?;
    crate::table::item_json(table, state, R::KIND, item.unwrap_or_default())
}
