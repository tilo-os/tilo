//! The framework's `read`: what a resource holds, as typed data, through the
//! same door, permissions and audit as every other action. A resource that
//! reads itself answers for what its own read names; any other published item
//! of its document collections, and a field of one, is answered from the
//! record's model tables, and disclosed by that read.

use tilo_actions::ResourceSpec;
use ui_core::automerge::AutoCommit;

use super::typed::{ResourceType, TypedRoot};

/// The operation type of the framework's `read` view.
pub(crate) const READ_TYPE: &str = tilo_resource::read::TYPE;

/// Bind a document read to one of the readable resource's own children.
/// These declarations never apply to live surface rows or external resources.
pub(super) fn offer_document_reads(resource: &mut ResourceSpec, eligible: &dyn Fn(&str) -> bool) {
    if eligible(resource.id.as_str()) && !resource.actions.iter().any(|spec| spec.id == "read") {
        resource
            .actions
            .push(tilo_resource::read::spec(Some(resource.id.as_str())));
    }
    for child in &mut resource.children {
        offer_document_reads(child, eligible);
    }
}

/// Bind the document read of an item to every item of a document collection
/// under `resource` that the record's table `table` reads
/// ([`tilo_resource::table::item_json`]) and that has no read yet: one the
/// app's own document projection names has the app's. An item is document
/// state: it is disclosed by this read, under the caller's ordinary read
/// policy, and a caller reads what it holds through it, where the item's
/// `open` would move the person's window.
pub(super) fn offer_item_reads(
    resource: &mut ResourceSpec,
    table: &'static tilo_resource::table::ModelTable,
    kind: &str,
) {
    if tilo_resource::table::reads_item(table, kind, resource.id.as_str())
        && !resource.actions.iter().any(|spec| spec.id == "read")
    {
        resource
            .actions
            .push(tilo_resource::read::spec(Some(resource.id.as_str())));
    }
    for child in &mut resource.children {
        offer_item_reads(child, table, kind);
    }
}

/// Answer a `read` request against the committed document of the resource
/// `typed` reads, whose state is `state`.
pub(super) fn read(
    typed: &dyn TypedRoot,
    resource: &ResourceType,
    state: &dyn std::any::Any,
    doc: &AutoCommit,
    payload: &[u8],
) -> Result<tilo_actions::ActionOutput, String> {
    let value = answer(
        doc,
        payload,
        |item| typed.read_content(state, doc, item),
        |frozen, item| {
            // The type's own adapter over the frozen version: its content,
            // or the exact bytes it holds. A version that does not read as
            // the type says which of the three ways it does not.
            if resource.readable {
                tilo_failure::ignored!(
                    "the record table answers what the adapter did not",
                    match typed.historical_content(frozen, item) {
                        Ok(content) => return Ok(content),
                        Err(gap) if !reads_as_item(resource, item) => return Err(gap.to_string()),
                        Err(_) => {}
                    }
                )
            }
            let past = typed.read(frozen).map_err(|error| error.to_string())?;
            typed.read_content(&*past, frozen, item)
        },
    )?;
    Ok(tilo_actions::ActionOutput {
        value: Some(value),
        resources: Vec::new(),
    })
}

/// Answer a `read` of a record whose committed document is `doc`, asked as
/// `payload` ([`tilo_resource::read::spec`]): `current` reads the record as
/// it is, `past` a frozen version of it, each the item a read names. The
/// one way every record an app publishes is read: its own, and those it
/// keeps beside it.
pub(crate) fn answer(
    doc: &AutoCommit,
    payload: &[u8],
    current: impl FnOnce(Option<&str>) -> Result<serde_json::Value, String>,
    past: impl FnOnce(&AutoCommit, Option<&str>) -> Result<serde_json::Value, String>,
) -> Result<serde_json::Value, String> {
    let args: serde_json::Value = if payload.is_empty() {
        serde_json::Value::Null
    } else {
        tilo_actions::decode_json(payload).map_err(|error| error.to_string())?
    };
    let item = args.get("item").and_then(serde_json::Value::as_str);
    match args.get("version").and_then(serde_json::Value::as_str) {
        Some(version) => {
            let heads = ui_core::history_source::steps::resolve(doc, version)?;
            let frozen = doc
                .clone()
                .fork_at(&heads)
                .map_err(|error| error.to_string())?;
            let content = past(&frozen, item)?;
            super::super::history::review::bounded(serde_json::json!({
                "version": heads.iter().map(ToString::to_string).collect::<Vec<_>>().join("+"),
                "heads": heads.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "content": content,
            }))
        }
        None => current(item),
    }
}

/// Whether `item` names a published part of the record its table reads
/// ([`tilo_resource::table::reads_item`]): what is read from the table when
/// the resource's own read, which comes first, does not answer it.
pub(super) fn reads_as_item(resource: &ResourceType, item: Option<&str>) -> bool {
    item.zip(resource.record_table)
        .is_some_and(|(item, table)| tilo_resource::table::reads_item(table, resource.kind, item))
}
