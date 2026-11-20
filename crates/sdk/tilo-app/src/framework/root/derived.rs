//! Reads of an item the app's own resource does not hold, answered by what a
//! chain it keeps derives over its parts: a mail folder, read as the
//! messages of it every part holds.

use tilo_actions::ActionOutput;

use super::super::settings::ViewStorage;

/// The answer the chains `storage` keeps derive for the item a read's
/// `payload` names, when the resource's own read did not answer it: `None`
/// for a read that names no item, or one no chain derives anything of.
pub(super) fn answer(
    storage: &ViewStorage,
    payload: &[u8],
) -> Option<Result<ActionOutput, String>> {
    let item = named_item(payload)?;
    let value = storage.read_over(&item)?;
    Some(value.map(|value| ActionOutput {
        value: Some(value),
        resources: Vec::new(),
    }))
}

/// The item a read's `payload` names, when it names one.
fn named_item(payload: &[u8]) -> Option<String> {
    let args: serde_json::Value = tilo_actions::decode_json(payload).ok()?;
    args.get("item")
        .and_then(serde_json::Value::as_str)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
}
