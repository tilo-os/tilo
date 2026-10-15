//! Apps' own files in the page: the profile's OPFS tree, mirrored so app
//! code that loads and saves synchronously works as it does natively.
//!
//! The host loads the mirror before it mounts anything
//! ([`mirror_files`]) and waits for its writes before it lets a mount go
//! ([`flush_files`]). A page holds one profile's app files.
use wasm_bindgen::prelude::*;

fn error(error: &tilo_files::FsError) -> JsValue {
    JsValue::from_str(&error.to_string())
}

/// Resolve apps' paths in `profile` and load its app files (each app's data,
/// settings and state) and the page's secrets into page memory. Idempotent.
///
/// # Errors
///
/// The browser has no OPFS, or the tree could not be read.
#[wasm_bindgen]
pub async fn mirror_files(profile: String) -> Result<(), JsValue> {
    tilo_paths::set_default_profile(&profile);
    tilo_files::web::mirror(&tilo_paths::default_profile())
        .await
        .map_err(|e| error(&e))?;
    // Apps' secrets too, so vault keys load synchronously. Without IndexedDB
    // or WebCrypto the secret store stays asynchronous and apps say so.
    if let Err(error) = tilo_secrets::web::mirror().await {
        web_sys::console::warn_1(&JsValue::from_str(&format!(
            "secrets are not mirrored; synchronous access stays unavailable: {error}"
        )));
    }
    Ok(())
}

/// Wait until every change apps made to their files is in OPFS, and every
/// secret they stored is in `IndexedDB`.
///
/// # Errors
///
/// The latest change OPFS refused.
#[wasm_bindgen]
pub async fn flush_files() -> Result<(), JsValue> {
    // A file may publish a credential reference. Custody must commit first,
    // and a failed secret write must prevent the dependent file flush.
    tilo_secrets::web::flush()
        .await
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    tilo_files::web::flush(&tilo_paths::default_profile())
        .await
        .map_err(|e| error(&e))
}
