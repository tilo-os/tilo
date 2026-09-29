//! The browser backend: see `web/secrets.mjs`.
//!
//! Its storage answers only asynchronously. A host that loads the page's
//! secrets first ([`mirror`]) gives apps synchronous access too: reads
//! answer from page memory and writes update it, then reach `IndexedDB` in
//! order ([`flush`] waits for them).

use std::cell::RefCell;
use std::collections::HashMap;

use js_sys::{Array, Promise, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::{Secret, SecretError};

#[wasm_bindgen(module = "/web/secrets.mjs")]
extern "C" {
    #[wasm_bindgen(js_name = getSecret)]
    fn get_secret(app: &str, key: &str) -> Promise;
    #[wasm_bindgen(js_name = putSecret)]
    fn put_secret(app: &str, key: &str, bytes: &[u8]) -> Promise;
    #[wasm_bindgen(js_name = deleteSecret)]
    fn delete_secret(app: &str, key: &str) -> Promise;
    #[wasm_bindgen(js_name = loadSecrets)]
    fn load_secrets() -> Promise;
    #[wasm_bindgen(js_name = queueSecret)]
    fn queue_secret(app: &str, key: &str, bytes: Option<Uint8Array>);
    #[wasm_bindgen(js_name = flushSecrets)]
    fn flush_secrets() -> Promise;
}

type Mirror = HashMap<(String, String), Secret>;

thread_local! {
    /// The page's secrets, once [`mirror`] loaded them.
    static MIRROR: RefCell<Option<Mirror>> = const { RefCell::new(None) };
}

/// Load every secret this page can decrypt into page memory, so the
/// `*_blocking` methods work. Idempotent.
///
/// # Errors
///
/// `IndexedDB` or `WebCrypto` failed.
pub async fn mirror() -> Result<(), SecretError> {
    if MIRROR.with(|m| m.borrow().is_some()) {
        return Ok(());
    }
    let entries = JsFuture::from(load_secrets())
        .await
        .map_err(|e| failed(&e))?;
    let mut mirror = Mirror::new();
    for entry in Array::from(&entries).iter() {
        let entry = Array::from(&entry);
        let (Some(app), Some(key)) = (entry.get(0).as_string(), entry.get(1).as_string()) else {
            continue;
        };
        mirror.insert(
            (app, key),
            Secret::new(Uint8Array::new(&entry.get(2)).to_vec()),
        );
    }
    MIRROR.with(|m| *m.borrow_mut() = Some(mirror));
    Ok(())
}

/// Wait until every write made through the mirror is in `IndexedDB`.
///
/// # Errors
///
/// The latest write the browser refused.
pub async fn flush() -> Result<(), SecretError> {
    JsFuture::from(flush_secrets())
        .await
        .map(drop)
        .map_err(|e| failed(&e))
}

pub(crate) fn mirrored() -> bool {
    MIRROR.with(|m| m.borrow().is_some())
}

/// The synchronous store over the page's mirror.
pub(crate) struct MirrorBackend;

impl crate::SecretBackend for MirrorBackend {
    fn protection(&self) -> crate::Protection {
        crate::Protection::BrowserWrapped
    }

    fn get(&self, app: &str, key: &str) -> Result<Option<Secret>, SecretError> {
        MIRROR.with(|m| {
            m.borrow()
                .as_ref()
                .map(|mirror| mirror.get(&(app.to_owned(), key.to_owned())).cloned())
                .ok_or_else(not_mirrored)
        })
    }

    fn put(&self, app: &str, key: &str, value: &[u8], _label: &str) -> Result<(), SecretError> {
        remember(app, key, Some(value))?;
        queue_secret(app, key, Some(Uint8Array::from(value)));
        Ok(())
    }

    fn delete(&self, app: &str, key: &str) -> Result<(), SecretError> {
        remember(app, key, None)?;
        queue_secret(app, key, None);
        Ok(())
    }
}

fn not_mirrored() -> SecretError {
    SecretError::Unsupported(crate::caps::unsupported(crate::Feature::Blocking))
}

/// Keep the mirror, when there is one, in step with a write.
fn remember(app: &str, key: &str, value: Option<&[u8]>) -> Result<(), SecretError> {
    MIRROR.with(|m| {
        let mut mirror = m.borrow_mut();
        let mirror = mirror.as_mut().ok_or_else(not_mirrored)?;
        let entry = (app.to_owned(), key.to_owned());
        match value {
            Some(value) => mirror.insert(entry, Secret::new(value)),
            None => mirror.remove(&entry),
        };
        Ok(())
    })
}

fn failed(error: &JsValue) -> SecretError {
    let text = error
        .as_string()
        .or_else(|| {
            Reflect::get(error, &"message".into())
                .ok()
                .and_then(|m| m.as_string())
        })
        .unwrap_or_else(|| format!("{error:?}"));
    SecretError::Failed(text)
}

pub(crate) async fn get(app: &str, key: &str) -> Result<Option<Secret>, SecretError> {
    if mirrored() {
        return crate::SecretBackend::get(&MirrorBackend, app, key);
    }
    let value = JsFuture::from(get_secret(app, key))
        .await
        .map_err(|e| failed(&e))?;
    if value.is_null() || value.is_undefined() {
        return Ok(None);
    }
    Ok(Some(Secret::new(Uint8Array::new(&value).to_vec())))
}

pub(crate) async fn put(app: &str, key: &str, value: &[u8]) -> Result<(), SecretError> {
    if mirrored() {
        remember(app, key, Some(value))?;
    }
    JsFuture::from(put_secret(app, key, value))
        .await
        .map(drop)
        .map_err(|e| failed(&e))
}

pub(crate) async fn delete(app: &str, key: &str) -> Result<(), SecretError> {
    if mirrored() {
        remember(app, key, None)?;
    }
    JsFuture::from(delete_secret(app, key))
        .await
        .map(drop)
        .map_err(|e| failed(&e))
}
