//! The settings document of a browser profile, kept in the profile's durable
//! store beside its records and committed to `IndexedDB` with them.
//!
//! `settings_store` reads and merges through the installed source, so the
//! toolkit's settings readers (theme, text assistance, accessibility) work
//! unchanged. The host may provide the document for a profile that has none
//! stored ([`provide`]); it is written back only once something changes it.
//! A page holds one settings document: the namespace loaded last provides it.
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use durable_store::{DurableStore as _, MemoryStore, StorageKey, Stored, WriteOp};
use serde_json::{Map, Value};

/// Where the document lives in the store. Changing it orphans stored settings.
const SETTINGS_PACKAGE: &str = "so.tilo.settings";

fn key() -> StorageKey {
    StorageKey::package(SETTINGS_PACKAGE)
}

thread_local! {
    static STORE: RefCell<Weak<MemoryStore>> = const { RefCell::new(Weak::new()) };
}

/// Make `store`'s document the page's settings.
pub fn install(store: &Rc<MemoryStore>) {
    STORE.with_borrow_mut(|slot| *slot = Rc::downgrade(store));
    let initial = store
        .read(&key())
        .ok()
        .flatten()
        .map(|stored| settings_store::parse(&String::from_utf8_lossy(&stored.bytes)))
        .unwrap_or_default();
    settings_store::install_source(Some(Arc::new(settings_store::HostDocument::new(
        initial, persist,
    ))));
}

fn persist(doc: &Map<String, Value>) -> std::io::Result<()> {
    let store = STORE
        .with_borrow(Weak::upgrade)
        .ok_or_else(|| std::io::Error::other("the settings store is closed"))?;
    let bytes = serde_json::to_vec(doc).map_err(std::io::Error::other)?;
    store
        .write(&key(), WriteOp::Replace(bytes), None)
        .map(drop)
        .map_err(std::io::Error::other)
}

/// Seed the profile's settings with the host's `json` object unless it has a
/// stored document. `true` when the host's document was taken.
///
/// # Errors
///
/// `json` is not an object, or the store refuses it.
pub fn provide(store: &Rc<MemoryStore>, json: &str) -> Result<bool, String> {
    if store.read(&key()).map_err(|e| e.to_string())?.is_some() {
        return Ok(false);
    }
    let Ok(Value::Object(doc)) = serde_json::from_str::<Value>(json) else {
        return Err("host settings must be a JSON object".to_owned());
    };
    let bytes = serde_json::to_vec(&doc).map_err(|e| e.to_string())?;
    store
        .hydrate(
            key(),
            Stored {
                bytes,
                writer: None,
            },
        )
        .map_err(|e| e.to_string())?;
    install(store);
    Ok(true)
}
