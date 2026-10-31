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

/// `read`: what a [readable](tilo_resource::Resource::READABLE) resource holds, as typed
/// data ([`tilo_resource::read::spec`]).
pub(super) fn read_spec() -> tilo_actions::ActionSpec {
    read_spec_for(None)
}

/// Bind a document read to one of the readable resource's own children.
/// These declarations never apply to live surface rows or external resources.
pub(super) fn offer_document_reads(resource: &mut ResourceSpec, eligible: &dyn Fn(&str) -> bool) {
    if eligible(resource.id.as_str()) && !resource.actions.iter().any(|spec| spec.id == "read") {
        resource
            .actions
            .push(read_spec_for(Some(resource.id.as_str())));
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
            .push(read_spec_for(Some(resource.id.as_str())));
    }
    for child in &mut resource.children {
        offer_item_reads(child, table, kind);
    }
}

pub(super) fn read_spec_for(item: Option<&str>) -> tilo_actions::ActionSpec {
    tilo_resource::read::spec(item)
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
    let args: serde_json::Value = if payload.is_empty() {
        serde_json::Value::Null
    } else {
        tilo_actions::decode_json(payload).map_err(|error| error.to_string())?
    };
    let item = args.get("item").and_then(serde_json::Value::as_str);
    let value = match args.get("version").and_then(serde_json::Value::as_str) {
        Some(version) => {
            let heads = ui_core::history_source::steps::resolve(doc, version)?;
            let frozen = doc
                .clone()
                .fork_at(&heads)
                .map_err(|error| error.to_string())?;
            // The type's own adapter over the frozen version: its content,
            // or the exact bytes it holds. A version that does not read as
            // the type says which of the three ways it does not.
            let content = if resource.readable {
                match typed.historical_content(&frozen, item) {
                    Ok(content) => content,
                    Err(gap) if !reads_as_item(resource, item) => return Err(gap.to_string()),
                    Err(_) => {
                        let past = typed.read(&frozen).map_err(|error| error.to_string())?;
                        item_content(resource, &*past, item)?
                    }
                }
            } else {
                let past = typed.read(&frozen).map_err(|error| error.to_string())?;
                item_content(resource, &*past, item)?
            };

            super::super::history::review::bounded(serde_json::json!({
                "version": heads.iter().map(ToString::to_string).collect::<Vec<_>>().join("+"),
                "heads": heads.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "content": content,
            }))?
        }
        None if resource.readable => match typed.read_content(state, doc, item) {
            Ok(content) => content,
            Err(error) if !reads_as_item(resource, item) => return Err(error),
            Err(_) => item_content(resource, state, item)?,
        },
        None => item_content(resource, state, item)?,
    };
    Ok(tilo_actions::ActionOutput {
        value: Some(value),
        resources: Vec::new(),
    })
}

/// Whether `item` names a published part of the record its table reads
/// ([`tilo_resource::table::reads_item`]): what is read from the table when
/// the resource's own read, which comes first, does not answer it.
pub(super) fn reads_as_item(resource: &ResourceType, item: Option<&str>) -> bool {
    item.zip(resource.record_table)
        .is_some_and(|(item, table)| tilo_resource::table::reads_item(table, resource.kind, item))
}

/// The document read of one item the resource's own read does not answer:
/// what the item holds, from the record's table.
pub(super) fn item_content(
    resource: &ResourceType,
    state: &dyn std::any::Any,
    item: Option<&str>,
) -> Result<serde_json::Value, String> {
    let item = item.ok_or_else(|| format!("a {} answers reads of its items", resource.label))?;
    let table = resource
        .record_table
        .ok_or_else(|| format!("a {} does not answer reads", resource.label))?;
    tilo_resource::table::item_json(table, state, resource.kind, item)
}
