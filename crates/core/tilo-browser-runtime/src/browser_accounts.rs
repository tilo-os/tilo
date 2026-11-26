//! The accounts this browser's profiles keep, read for the entry page from
//! the Settings record of a person's accounts in their own workspace, which
//! each profile's store keeps as its page value `browser-accounts`
//! (`web/browser-accounts.mjs`).
use automerge::{AutoCommit, LoadOptions, OnPartialLoad};
use collab_types::document::identity::{home, identity};
use collab_types::document::{DocumentId, ResourceType};
use wasm_bindgen::prelude::*;

#[allow(
    clippy::needless_pass_by_value,
    reason = "a map_err adapter receives each error by value"
)]
fn error(e: impl ToString) -> JsValue {
    JsValue::from_str(&e.to_string())
}

/// Read public picker metadata from an existing Settings account document.
/// No record initialization, key storage, or credential store is involved.
///
/// # Errors
///
/// Refuses invalid identity, oversized or malformed documents, and failed projection.
#[wasm_bindgen]
pub fn project_browser_accounts(root: &str, origin: &str, bytes: &[u8]) -> Result<String, JsValue> {
    if bytes.len() > 1024 * 1024 {
        return Err(error("account projection exceeds its read budget"));
    }
    let kind = ResourceType::new("tilo.accounts");
    let Some(workspace) = DocumentId::of_member(root, kind.as_str())
        .and_then(|_| collab_types::person_workspace_of(root))
    else {
        return Err(error("invalid root identity"));
    };
    let doc = AutoCommit::load_with_options(
        bytes,
        LoadOptions::new().on_partial_load(OnPartialLoad::Error),
    )
    .map_err(error)?;
    // The workspace's record of this kind: one per kind and workspace, kept
    // in the person's own workspace, which their root names alone.
    let id = DocumentId::parse(&collab_types::workspace_document_id(kind.as_str()))
        .ok_or_else(|| error("the accounts record has no id"))?;
    let home = home(&doc);
    if identity(&doc).is_none_or(|found| found.id != id || found.kind != kind)
        || home.as_deref() != Some(workspace.to_string().as_str())
    {
        return Err(error(
            "account document belongs to another identity or record",
        ));
    }
    let accounts = accounts::person::AccountList::browser_accounts_from_doc(&doc, root, origin)
        .map_err(error)?;
    serde_json::to_string(&accounts).map_err(error)
}
