//! The accounts this browser's profiles keep, read for the entry page from
//! the Settings record of a person's accounts each profile's store keeps as
//! its page value `browser-accounts` (`web/browser-accounts.mjs`).
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
/// Refuses invalid identity, oversized or malformed documents, and failed projection.
#[wasm_bindgen]
pub fn project_browser_accounts(root: &str, origin: &str, bytes: &[u8]) -> Result<String, JsValue> {
    if bytes.len() > 1024 * 1024 {
        return Err(error("account projection exceeds its read budget"));
    }
    let kind = ui::resource::ResourceType::new("tilo.accounts");
    if ui::resource::DocumentId::of_member(root, kind.as_str()).is_none() {
        return Err(error("invalid root identity"));
    }
    let doc = ui::automerge::AutoCommit::load_with_options(
        bytes,
        ui::automerge::LoadOptions::new().on_partial_load(ui::automerge::OnPartialLoad::Error),
    )
    .map_err(error)?;
    let id = ui::resource::person::document_id(&kind);
    let home = ui::resource::home(&doc);
    if ui::resource::identity(&doc).is_none_or(|found| found.id != id || found.kind != kind)
        || home.as_deref() != Some(ui::resource::person::PERSONAL_SCOPE)
    {
        return Err(error(
            "account document belongs to another identity or record",
        ));
    }
    let accounts = accounts::person::AccountList::browser_accounts_from_doc(&doc, root, origin)
        .map_err(error)?;
    serde_json::to_string(&accounts).map_err(error)
}
