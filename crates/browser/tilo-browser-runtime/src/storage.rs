//! The page's `IndexedDB` behind `durable_store::MemoryStore::queued`.
//!
//! JavaScript hydrates each profile/device namespace once per storage lease,
//! drains accepted writes, commits them in strict transactions and reports
//! each outcome; a write counts as saved only after that acknowledgement.
//! Keys cross as JSON strings (`DiskKey`); values cross whole, so the page
//! never replays Automerge appends itself.
//!
//! A namespace outlives any one mount: `web/durable-host.mjs` loads it once
//! per lease, is told of each accepted write through the store's write hook,
//! and drains, commits and acknowledges on its own schedule. Rust code that
//! keeps its own state in the profile (peer state, settings) writes to
//! [`store`] under a reserved `StorageKey::Package` id and is persisted the
//! same way.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use durable_store::{DurableStore as _, MemoryStore, Revision, StorageKey, Stored};
use js_sys::{Function, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;

/// The page-visible form of [`StorageKey`]. Changing it orphans stored rows.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
enum DiskKey {
    Record {
        package: String,
        workspace: String,
        record: String,
    },
    Package {
        package: String,
    },
    RoomIndex {
        package: String,
        workspace: String,
    },
    /// Rows of the removed package-global record key. Main dropped the key
    /// without a migration; a page skips these rows instead of failing to load.
    PackageRecord {
        package: String,
        record: String,
    },
    View {
        device: String,
        workspace: String,
        instance: u64,
    },
}

/// The exact Settings personal-record key for a public root identity.
/// Does not open or create a record, storage lease, or certificate.
///
/// # Errors
/// Refuses an invalid root identity or unencodable storage key.
#[wasm_bindgen]
pub fn browser_accounts_key(root: &str) -> Result<String, JsValue> {
    let person_room =
        collab_types::person_room_of(root).ok_or_else(|| error("invalid root identity"))?;
    let kind = ui::record::RecordKind::new("tilo.accounts");
    let key = DiskKey::Record {
        package: accounts::person::ACCOUNTS_OWNER.into(),
        workspace: person_room.to_string(),
        record: ui::record::person::record_id(&kind).storage_key(),
    };
    serde_json::to_string(&key).map_err(error)
}

/// Read public picker metadata from an existing Settings account document.
/// The caller reads only the key returned by `browser_accounts_key`; no
/// record initialization, key custody, or credential store is involved.
///
/// # Errors
/// Refuses invalid identity, oversized or malformed documents, and failed projection.
#[wasm_bindgen]
pub fn project_browser_accounts(root: &str, origin: &str, bytes: &[u8]) -> Result<String, JsValue> {
    if bytes.len() > 1024 * 1024 {
        return Err(error("account projection exceeds its read budget"));
    }
    browser_accounts_key(root)?;
    let doc = ui::automerge::AutoCommit::load_with_options(
        bytes,
        ui::automerge::LoadOptions::new().on_partial_load(ui::automerge::OnPartialLoad::Error),
    )
    .map_err(error)?;
    let person_room =
        collab_types::person_room_of(root).ok_or_else(|| error("invalid root identity"))?;
    let kind = ui::record::RecordKind::new("tilo.accounts");
    let id = ui::record::person::record_id(&kind);
    if ui::record::identity(&doc).is_none_or(|found| found.id != id || found.kind != kind)
        || ui::record::home(&doc).as_deref() != Some(person_room.to_string().as_str())
    {
        return Err(error(
            "account document belongs to another identity or record",
        ));
    }
    let accounts = accounts::person::PersonAccounts::browser_accounts_from_doc(&doc, root, origin)
        .map_err(error)?;
    serde_json::to_string(&accounts).map_err(error)
}

impl From<&StorageKey> for DiskKey {
    fn from(key: &StorageKey) -> Self {
        match key.clone() {
            StorageKey::Record {
                package,
                workspace,
                record,
            } => Self::Record {
                package,
                workspace,
                record,
            },
            StorageKey::Package { package } => Self::Package { package },
            StorageKey::RoomIndex { package, workspace } => Self::RoomIndex { package, workspace },
            StorageKey::View {
                device,
                workspace,
                instance,
            } => Self::View {
                device,
                workspace,
                instance,
            },
        }
    }
}

impl DiskKey {
    /// The store key this row holds, or `None` for a row of a removed key.
    fn into_key(self) -> Option<StorageKey> {
        Some(match self {
            Self::Record {
                package,
                workspace,
                record,
            } => StorageKey::Record {
                package,
                workspace,
                record,
            },
            Self::Package { package } => StorageKey::Package { package },
            Self::RoomIndex { package, workspace } => StorageKey::RoomIndex { package, workspace },
            Self::PackageRecord { .. } => return None,
            Self::View {
                device,
                workspace,
                instance,
            } => StorageKey::View {
                device,
                workspace,
                instance,
            },
        })
    }
}

thread_local! {
    static CACHES: RefCell<HashMap<(String, String), Rc<MemoryStore>>> = RefCell::new(HashMap::new());
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "a map_err adapter receives each error by value"
)]
fn error(e: impl ToString) -> JsValue {
    JsValue::from_str(&e.to_string())
}

/// The namespace's store, hydrated from `hydrated` (`[{key, bytes, actor}]`)
/// on first use in this lease.
pub(crate) fn load(
    profile: &str,
    device: &str,
    hydrated: &JsValue,
) -> Result<Rc<MemoryStore>, JsValue> {
    let key = (profile.to_owned(), device.to_owned());
    if let Some(store) = CACHES.with_borrow(|caches| caches.get(&key).cloned()) {
        return Ok(store);
    }
    let store = Rc::new(MemoryStore::queued().named(format!("browser:{profile}:{device}")));
    for entry in js_sys::Array::from(hydrated).iter() {
        let disk = Reflect::get(&entry, &"key".into())?
            .as_string()
            .ok_or_else(|| error("stored row has no key"))?;
        let disk: DiskKey = serde_json::from_str(&disk).map_err(error)?;
        let Some(key) = disk.into_key() else {
            continue;
        };
        let bytes = Uint8Array::new(&Reflect::get(&entry, &"bytes".into())?).to_vec();
        let actor = Reflect::get(&entry, &"actor".into())?;
        let writer =
            (!actor.is_null() && !actor.is_undefined()).then(|| Uint8Array::new(&actor).to_vec());
        store
            .hydrate(key, Stored { bytes, writer })
            .map_err(error)?;
    }
    CACHES.with_borrow_mut(|caches| caches.insert(key, Rc::clone(&store)));
    crate::settings::install(&store);
    Ok(store)
}

/// A loaded namespace's store, for Rust state kept in the profile.
///
/// # Errors
///
/// The namespace has not been loaded in this lease.
pub fn store(profile: &str, device: &str) -> Result<Rc<MemoryStore>, JsValue> {
    cached(profile, device)
}

/// Load a namespace from its stored rows (`[{key, bytes, actor}]`) before any
/// mount needs it. A namespace already loaded in this lease is kept.
///
/// # Errors
///
/// Rejects a malformed row.
#[wasm_bindgen]
pub fn load_storage(profile: &str, device: &str, hydrated: &JsValue) -> Result<(), JsValue> {
    load(profile, device, hydrated).map(drop)
}

/// Take the namespace's accepted writes for the page to commit
/// (`[{key, bytes, actor, revision}]`), whichever mount or service made them.
///
/// # Errors
///
/// Rejects an unknown namespace.
#[wasm_bindgen]
pub fn drain_storage(profile: &str, device: &str) -> Result<JsValue, JsValue> {
    drain(&*cached(profile, device)?)
}

/// Queue each key whose commit failed for another attempt with its current
/// value; returns how many. The next drain carries them.
///
/// # Errors
///
/// Rejects an unknown namespace.
#[wasm_bindgen]
pub fn retry_storage(profile: &str, device: &str) -> Result<u32, JsValue> {
    let queued = cached(profile, device)?.retry_failed();
    Ok(u32::try_from(queued).unwrap_or(u32::MAX))
}

/// Call `hook` (with no arguments) after each write the namespace accepts,
/// or stop with `None`. The hook runs inside Rust's write: it must only
/// schedule a drain, never drain synchronously.
///
/// # Errors
///
/// Rejects an unknown namespace.
#[wasm_bindgen]
pub fn storage_write_hook(
    profile: &str,
    device: &str,
    hook: Option<Function>,
) -> Result<(), JsValue> {
    cached(profile, device)?.set_write_hook(hook.map(|hook| {
        Rc::new(move || {
            let _ = hook.call0(&JsValue::NULL);
        }) as Rc<dyn Fn()>
    }));
    Ok(())
}

/// Where page-side values (`profile_value`) live. Changing it orphans them.
const PAGE_VALUES_PACKAGE: &str = "so.tilo.browser-page";

/// The workspace under which a profile keeps state that belongs to no room.
pub(crate) const PROFILE_WORKSPACE: &str = "profile";

/// A small page-side value kept in the profile (for example a peer's public
/// certificates), or `None` when absent. Empty bytes mean deleted.
///
/// # Errors
///
/// Rejects an unknown namespace.
#[wasm_bindgen]
pub fn profile_value(profile: &str, device: &str, name: &str) -> Result<Option<Vec<u8>>, JsValue> {
    let stored = cached(profile, device)?
        .read(&StorageKey::record(
            PAGE_VALUES_PACKAGE,
            PROFILE_WORKSPACE,
            name,
        ))
        .map_err(error)?;
    Ok(stored
        .map(|stored| stored.bytes)
        .filter(|bytes| !bytes.is_empty()))
}

/// Keep (or with `None`, delete) a page-side value in the profile; committed
/// with the profile's other writes.
///
/// # Errors
///
/// Rejects an unknown namespace or a refused write.
#[wasm_bindgen]
#[allow(
    clippy::needless_pass_by_value,
    reason = "wasm-bindgen receives optional JavaScript bytes only as an owned value"
)]
pub fn set_profile_value(
    profile: &str,
    device: &str,
    name: &str,
    bytes: Option<Vec<u8>>,
) -> Result<(), JsValue> {
    cached(profile, device)?
        .write(
            &StorageKey::record(PAGE_VALUES_PACKAGE, PROFILE_WORKSPACE, name),
            durable_store::WriteOp::Replace(bytes.unwrap_or_default()),
            None,
        )
        .map(drop)
        .map_err(error)
}

/// Seed the namespace's settings with the host's JSON object unless it has
/// stored settings. `true` when the host's document was taken.
///
/// # Errors
///
/// Rejects an unknown namespace or a document that is not an object.
#[wasm_bindgen]
pub fn provide_settings(profile: &str, device: &str, settings: &str) -> Result<bool, JsValue> {
    crate::settings::provide(&cached(profile, device)?, settings).map_err(error)
}

/// Take every accepted write as `[{key, bytes, actor, revision}]` with the
/// key's whole current value.
pub(crate) fn drain(store: &MemoryStore) -> Result<JsValue, JsValue> {
    let rows = js_sys::Array::new();
    for write in store.drain() {
        let current = store.read(&write.key).map_err(error)?.unwrap_or_default();
        let row = js_sys::Object::new();
        let key = serde_json::to_string(&DiskKey::from(&write.key)).map_err(error)?;
        Reflect::set(&row, &"key".into(), &key.into())?;
        Reflect::set(
            &row,
            &"bytes".into(),
            &Uint8Array::from(current.bytes.as_slice()),
        )?;
        let actor = current
            .writer
            .as_deref()
            .map_or(JsValue::NULL, |writer| Uint8Array::from(writer).into());
        Reflect::set(&row, &"actor".into(), &actor)?;
        Reflect::set(
            &row,
            &"revision".into(),
            &write.revision.0.to_string().into(),
        )?;
        rows.push(&row);
    }
    Ok(rows.into())
}

fn cached(profile: &str, device: &str) -> Result<Rc<MemoryStore>, JsValue> {
    CACHES
        .with_borrow(|caches| {
            caches
                .get(&(profile.to_owned(), device.to_owned()))
                .cloned()
        })
        .ok_or_else(|| error("unknown storage namespace"))
}

/// Report the page's outcome for a drained write.
///
/// # Errors
///
/// Rejects an unknown namespace or a revision not awaiting acknowledgement.
#[wasm_bindgen]
pub fn acknowledge_storage(
    profile: &str,
    device: &str,
    revision: u64,
    failure: Option<String>,
) -> Result<(), JsValue> {
    cached(profile, device)?
        .acknowledge(Revision(revision), failure.map_or(Ok(()), Err))
        .map_err(error)
}

/// A newly acquired cross-tab lease must hydrate durable bytes afresh.
///
/// # Errors
///
/// Refuses while the namespace still holds writes the page has not committed.
#[wasm_bindgen]
pub fn refresh_storage_lease(profile: &str, device: &str) -> Result<(), JsValue> {
    let key = (profile.to_owned(), device.to_owned());
    CACHES.with_borrow_mut(|caches| {
        if let Some(store) = caches.get(&key)
            && store.flush(None).is_err()
        {
            return Err(error(
                "unsaved writes retain custody; retry persistence first",
            ));
        }
        caches.remove(&key);
        Ok(())
    })
}
