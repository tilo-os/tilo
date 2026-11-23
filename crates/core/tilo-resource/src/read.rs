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

/// What `state` reads as to a view of another language that does not run its
/// app: the record's JSON ([`Model::to_json`]) with each collection a list of
/// its items, their ids dropped, and each keyed collection a list of its
/// values. A reading names no item by id — the surface's fields are the item's
/// — so what a view reads it as is what the runtime hands it (a runtime's
/// `set_reading`).
#[must_use]
pub fn reading<R: Resource>(state: &R::State) -> serde_json::Value {
    let (Some(shape), Some(json)) = (R::shape(), R::state_json(state)) else {
        return serde_json::Value::Null;
    };
    projected(&shape, &json)
}

/// The key a collection's entry holds its value under in the record's JSON form
/// ([`crate::Model::to_json`]): its document key is [`crate::shape::ENTRY_VALUE_KEY`].
const JSON_ENTRY_VALUE: &str = "value";

/// `json`, the record's JSON form, as [`reading`] reads it of `shape`.
fn projected(shape: &crate::Shape, json: &serde_json::Value) -> serde_json::Value {
    match (shape, json) {
        (crate::Shape::Record(fields), serde_json::Value::Object(map)) => {
            let fields: serde_json::Map<_, _> = fields
                .iter()
                .filter_map(|field| {
                    map.get(field.key)
                        .map(|value| (field.key.to_owned(), projected(&field.shape, value)))
                })
                .collect();
            serde_json::Value::Object(fields)
        }
        (crate::Shape::Collection(inner), serde_json::Value::Array(entries)) => {
            let items = entries
                .iter()
                .map(|entry| {
                    let item = entry.get(JSON_ENTRY_VALUE).unwrap_or(entry);
                    projected(inner, item)
                })
                .collect();
            serde_json::Value::Array(items)
        }
        (crate::Shape::Keyed(item), serde_json::Value::Array(values)) => {
            serde_json::Value::Array(values.iter().map(|value| projected(item, value)).collect())
        }
        (crate::Shape::Map(item), serde_json::Value::Object(entries)) => {
            let entries: serde_json::Map<_, _> = entries
                .iter()
                .map(|(key, value)| (key.clone(), projected(item, value)))
                .collect();
            serde_json::Value::Object(entries)
        }
        (_, json) => json.clone(),
    }
}

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
pub fn resource_spec(record: &str) -> tilo_actions::ActionSpec {
    spec_on(Some(record), None)
}

/// The record a `read` asked as `payload` names, when it is not the app's
/// own ([`resource_spec`]).
#[must_use]
pub fn resource_of(payload: &[u8]) -> Option<String> {
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
            tilo_actions::ActionParam::optional(
                "cursor",
                tilo_actions::ParamType::Text,
                "The next_cursor an earlier read answered, for the next page of its lists",
            )
            .nullable(),
            tilo_actions::ActionParam::optional(
                "pageSize",
                tilo_actions::ParamType::Integer,
                "How many entries of each list a page holds (default 25, at most 200)",
            )
            .nullable(),
            tilo_actions::ActionParam::optional(
                "field",
                tilo_actions::ParamType::Text,
                "The collection to read a page of, by field name (default: the one collection it has)",
            )
            .nullable(),
            tilo_actions::ActionParam::optional(
                "where",
                tilo_actions::ParamType::Object,
                "Only the collection's items whose fields hold these values, by field name, a reference by its id; `from`: the items made from that resource",
            )
            .nullable(),
            tilo_actions::ActionParam::optional(
                "after",
                tilo_actions::ParamType::Text,
                "The page starts after this item: the `next` an earlier page answered",
            )
            .nullable(),
            tilo_actions::ActionParam::optional(
                "limit",
                tilo_actions::ParamType::Integer,
                "How many items of the collection the page holds (default 25, at most 200)",
            )
            .nullable(),
        ]
        .into_iter()
        .filter(|param| item.is_none() || !matches!(param.name.as_ref(), "item" | "cursor" | "pageSize"))
        .collect(),
        move |args| {
            let mut asked = serde_json::Map::new();
            asked.insert("item".into(), item.as_deref().or_else(|| args.text("item")).into());
            asked.insert("version".into(), args.text("version").into());
            asked.insert("cursor".into(), args.text("cursor").into());
            asked.insert("pageSize".into(), args.integer("pageSize").into());
            asked.insert("field".into(), args.text("field").into());
            asked.insert(
                "where".into(),
                match args.get("where") {
                    Some(tilo_actions::ArgValue::Object(matching)) => matching.clone().into(),
                    _ => serde_json::Value::Null,
                },
            );
            asked.insert("after".into(), args.text("after").into());
            asked.insert("limit".into(), args.integer("limit").into());
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
        "What this holds as typed data: with no item, the resource and the ids of what it contains; with an item id, that item's content, each resource with who made it, when and what from (`created`). With field, where, after and limit, a page of one collection's items: those whose fields hold the values `where` names, or that were made from its `from`. With version, read frozen historical content and its exact heads without changing the live resource.",
    )
    // Typed data for a program to reason about; a person reads what the
    // app shows.
    .for_audience(tilo_actions::Audience::Agents);
    spec.type_mut().contract.result = tilo_actions::ResultSpec::Json {
        schema: serde_json::json!({"type": "object"}),
    };
    spec.type_mut().contract.step = Some(tilo_actions::StepKind::Read);
    spec.reads_document()
}

/// Entries of each list a page of a read holds when the caller names none.
pub const PAGE_SIZE: usize = 25;
/// Most entries of each list a page of a read holds.
pub const MAX_PAGE_SIZE: usize = 200;

/// One page of `value`, a read of a whole resource: each list it holds at
/// its top level cut to the `size` entries from `cursor` on (the cursor a
/// page before answered), so a read costs a page however much the resource
/// holds. A page that leaves entries out says how many each list holds
/// (`totals`) and where the next page starts (`next_cursor`); one that cut
/// nothing answers `value` as it was.
///
/// # Errors
/// `cursor` is not one a read answered.
pub fn page(
    value: serde_json::Value,
    cursor: Option<&str>,
    size: Option<i64>,
) -> Result<serde_json::Value, String> {
    let offset = match cursor.filter(|cursor| !cursor.is_empty()) {
        None => 0,
        Some(cursor) => cursor
            .parse::<usize>()
            .map_err(|_| format!("`{cursor}` is not a cursor a read answered"))?,
    };
    let size = size
        .and_then(|size| usize::try_from(size).ok())
        .unwrap_or(PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE);
    let serde_json::Value::Object(mut fields) = value else {
        return Ok(value);
    };
    let end = offset.saturating_add(size);
    let mut totals = serde_json::Map::new();
    let mut more = false;
    for (name, entries) in &mut fields {
        let serde_json::Value::Array(entries) = entries else {
            continue;
        };
        if offset == 0 && entries.len() <= size {
            continue;
        }
        totals.insert(name.clone(), entries.len().into());
        more |= entries.len() > end;
        let kept: Vec<serde_json::Value> = entries.drain(..).skip(offset).take(size).collect();
        *entries = kept;
    }
    if !totals.is_empty() {
        fields.insert("totals".into(), serde_json::Value::Object(totals));
    }
    if more {
        fields.insert("next_cursor".into(), end.to_string().into());
    }
    Ok(serde_json::Value::Object(fields))
}

/// What a record holds, as the record's own model's JSON
/// ([`Model::to_json`]): every field it keeps, in the shape its own type
/// gives them. Name it directly as `read_content` on a record whose fields are
/// all it holds, so a caller with no window open — an agent, a script, the
/// SDK — reads it through the framework's `read`, as it reads any app's
/// record:
///
/// ```ignore
/// #[resource(kind = "calculator.document", read_content = tilo_resource::read::fields)]
/// impl Calculator {
///     // ...
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

/// What a registered read's published action is called: its id, its label and the line
/// that says what it takes.
#[derive(Clone, Copy, Debug)]
pub struct ReadName<'a> {
    pub id: &'a str,
    pub label: &'a str,
    pub description: &'a str,
}

/// The parameters of a read that takes none.
#[derive(Clone, Copy, Debug, Default, serde::Deserialize, crate::JsonSchema)]
pub struct NoReadParams {}

/// A read an app declared: the published action of a registered reader, made
/// by [`read_action`] from its name, parameters and result schema. What a
/// surface publishes as the reads of its items (`Publishes::item_reads`) is
/// one of these, so a row's reads are a declaration's and never a spec the
/// app shaped.
#[derive(Clone)]
pub struct Declared(tilo_actions::ActionSpec);

impl Declared {
    /// The published action, for the framework that merges it into a row.
    #[must_use]
    pub fn into_spec(self) -> tilo_actions::ActionSpec {
        self.0
    }

    /// The read takes the document as its input: the framework answers it from
    /// the committed document a member may read.
    #[must_use]
    pub fn reads_document(mut self) -> Self {
        self.0 = self.0.reads_document();
        self
    }
}

/// The published action of the registered read whose operation is `operation`. Its
/// parameters are the fields of `P` as `P`'s JSON Schema describes them (doc comments,
/// `#[schema(range(..))]` bounds, choices); a caller's arguments, bound strictly to them,
/// become a `P` with serde, and `request` turns that into the reader's request.
#[must_use]
pub fn read_action<P, Q>(
    name: ReadName<'_>,
    operation: &'static str,
    schema: fn() -> &'static serde_json::Value,
    request: impl Fn(P) -> Q + Send + Sync + 'static,
) -> Declared
where
    P: serde::de::DeserializeOwned + crate::JsonSchema,
    Q: serde::Serialize,
{
    use tilo_actions::{ActionContract, EncodedAction, OperationKind, ResultSpec};
    let params = crate::__private::flat_params(&P::json_schema());
    Declared(
        tilo_actions::ActionSpec::typed_operation(
            name.id,
            name.label,
            name.description,
            OperationKind::Effect,
            operation,
            params,
            move |args| {
                serde_json::from_value::<P>(serde_json::Value::Object(args.to_json()))
                    .ok()
                    .and_then(|params| {
                        EncodedAction::encode_operation(operation, &request(params)).ok()
                    })
                    .unwrap_or_else(|| EncodedAction {
                        type_id: operation.into(),
                        payload: Vec::new(),
                    })
            },
        )
        .with_contract(ActionContract {
            result: ResultSpec::Json {
                schema: schema().clone(),
            },
            ..ActionContract::view()
        }),
    )
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
        R::RESOURCE_TABLE.is_some_and(|table| crate::table::reads_item(table, R::KIND, item))
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
    let table = R::RESOURCE_TABLE
        .ok_or_else(|| format!("a {} does not answer reads of its parts", R::LABEL))?;
    crate::table::item_json(table, state, R::KIND, item.unwrap_or_default())
}
