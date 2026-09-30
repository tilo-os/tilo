//! Durable implementation of `org.freedesktop.impl.portal.PermissionStore`.
//!
//! This is deliberately a small, local database: portal restore data and
//! permission choices are user-private state, never collaborative workspace
//! state.  The D-Bus service that exposes it must still be restricted to the
//! portal frontend/policy controller; this module additionally bounds all
//! untrusted keys and values before committing them.

use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
use zbus::interface;
#[cfg(target_os = "linux")]
use zbus::names::OwnedUniqueName;
#[cfg(target_os = "linux")]
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, Str};

#[cfg(target_os = "linux")]
use super::{PortalContext, auth::authenticate_frontend_call};

const MAX_TABLE: usize = 96;
const MAX_ID: usize = 512;
#[cfg(target_os = "linux")]
const MAX_APP: usize = 255;
#[cfg(target_os = "linux")]
const MAX_PERMISSIONS: usize = 64;
const MAX_VALUE: usize = 16 * 1024;
#[cfg(target_os = "linux")]
const MAX_ENTRIES: usize = 4096;

#[allow(
    dead_code,
    reason = "legacy scalar variants remain readable during store migration"
)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
enum StoredValue {
    Empty,
    /// Complete D-Bus variant encoding. This preserves arbitrary bounded
    /// portal restore tuples/dictionaries instead of narrowing the standard
    /// `v` field to a handful of scalar types.
    Wire(Vec<u8>),
    Bool(bool),
    Unsigned(u32),
    Text(String),
    Texts(Vec<String>),
}

impl Default for StoredValue {
    fn default() -> Self {
        Self::Empty
    }
}

impl StoredValue {
    fn from_value(value: OwnedValue) -> Result<Self, zbus::fdo::Error> {
        if value.value_signature().to_string().contains('h') {
            return Err(invalid(
                "permission-store values cannot retain file descriptors",
            ));
        }
        let context = zbus::zvariant::serialized::Context::new_dbus(zbus::zvariant::LE, 0);
        let encoded = zbus::zvariant::to_bytes(context, &value)
            .map_err(|_| invalid("invalid permission-store value"))?;
        if encoded.len() > MAX_VALUE {
            return Err(invalid("permission-store value is too large"));
        }
        Ok(Self::Wire(encoded.bytes().to_vec()))
    }

    fn to_value(&self) -> OwnedValue {
        match self {
            Self::Empty => OwnedValue::from(Str::from("")),
            Self::Wire(bytes) => {
                let context = zbus::zvariant::serialized::Context::new_dbus(zbus::zvariant::LE, 0);
                let data = zbus::zvariant::serialized::Data::new_borrowed_fds(
                    bytes.as_slice(),
                    context,
                    std::iter::empty::<zbus::zvariant::Fd<'_>>(),
                );
                data.deserialize::<OwnedValue>()
                    .ok()
                    .map(|(value, _)| value)
                    .unwrap_or_else(|| OwnedValue::from(Str::from("")))
            }
            Self::Bool(value) => OwnedValue::from(*value),
            Self::Unsigned(value) => OwnedValue::from(*value),
            Self::Text(value) => OwnedValue::from(Str::from(value.as_str())),
            Self::Texts(values) => OwnedValue::try_from(zbus::zvariant::Value::new(values.clone()))
                .unwrap_or_else(|_| OwnedValue::from(Str::from(""))),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct StoredEntry {
    permissions: BTreeMap<String, Vec<String>>,
    data: StoredValue,
}
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct StoreFile {
    entries: BTreeMap<String, BTreeMap<String, StoredEntry>>,
}

/// The actual durable permission state, independent of the D-Bus authenticator.
#[derive(Clone)]
pub struct PermissionStoreData {
    state: Arc<Mutex<StoreFile>>,
    path: Arc<PathBuf>,
}

#[cfg(target_os = "linux")]
#[derive(Clone)]
pub struct PermissionStoreBackend {
    ctx: PortalContext,
    data: PermissionStoreData,
}
#[cfg(target_os = "linux")]
impl std::ops::Deref for PermissionStoreBackend {
    type Target = PermissionStoreData;
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}
#[cfg(target_os = "linux")]
impl PermissionStoreBackend {
    fn entry(table: &str, id: &str) -> zbus::fdo::Result<(String, String)> {
        PermissionStoreData::entry(table, id)
    }
    pub fn new(ctx: PortalContext) -> Self {
        Self::at_path(ctx, default_path())
    }
    pub fn at_path(ctx: PortalContext, path: impl Into<PathBuf>) -> Self {
        Self {
            ctx,
            data: PermissionStoreData::at_path(path),
        }
    }
    async fn authenticate(&self, sender: Option<&OwnedUniqueName>) -> zbus::fdo::Result<()> {
        authenticate_frontend_call(&self.ctx, sender)
            .await
            .map(|_| ())
            .map_err(|_| zbus::fdo::Error::AccessDenied("untrusted portal caller".into()))
    }
}
impl PermissionStoreData {
    pub fn at_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let state = tilo_files::native::atomic::json::load_or_default(&path);
        Self {
            state: Arc::new(Mutex::new(state)),
            path: Arc::new(path),
        }
    }
    fn commit(&self, state: &StoreFile) -> zbus::fdo::Result<()> {
        tilo_files::native::atomic::json::write_private(&self.path, state)
            .map_err(|e| failed(&format!("cannot commit permission store: {e}")))
    }

    fn with_mutation<T>(
        &self,
        change: impl FnOnce(&mut StoreFile) -> zbus::fdo::Result<T>,
    ) -> zbus::fdo::Result<T> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| failed("permission-store lock poisoned"))?;
        let previous = state.clone();
        let result = change(&mut state)?;
        if let Err(error) = self.commit(&state) {
            *state = previous;
            return Err(error);
        }
        Ok(result)
    }

    fn entry(table: &str, id: &str) -> zbus::fdo::Result<(String, String)> {
        valid_key(table, MAX_TABLE, "table")?;
        valid_key(id, MAX_ID, "id")?;
        Ok((table.into(), id.into()))
    }
}

#[cfg(target_os = "linux")]
#[interface(name = "org.freedesktop.impl.portal.PermissionStore")]
impl PermissionStoreBackend {
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        2
    }

    async fn lookup(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        table: String,
        id: String,
    ) -> zbus::fdo::Result<(HashMap<String, Vec<String>>, OwnedValue)> {
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        self.authenticate(sender.as_ref()).await?;
        let (table, id) = Self::entry(&table, &id)?;
        let state = self
            .state
            .lock()
            .map_err(|_| failed("permission-store lock poisoned"))?;
        let entry = state
            .entries
            .get(&table)
            .and_then(|table| table.get(&id))
            .ok_or_else(|| failed("permission-store entry does not exist"))?;
        Ok((
            entry.permissions.clone().into_iter().collect(),
            entry.data.to_value(),
        ))
    }

    async fn set(
        &self,
        #[zbus(signal_emitter)] signal: SignalEmitter<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        table: String,
        create: bool,
        id: String,
        permissions: HashMap<String, Vec<String>>,
        data: OwnedValue,
    ) -> zbus::fdo::Result<()> {
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        self.authenticate(sender.as_ref()).await?;
        let (table, id) = Self::entry(&table, &id)?;
        let permissions = checked_permissions(permissions)?;
        let data = StoredValue::from_value(data)?;
        let changed = self.with_mutation(|state| {
            let records = state.entries.entry(table.clone()).or_default();
            if !create && !records.contains_key(&id) {
                return Err(failed("permission-store entry does not exist"));
            }
            if create && !records.contains_key(&id) && records.len() >= MAX_ENTRIES {
                return Err(failed("permission-store table is full"));
            }
            let changed = StoredEntry { permissions, data };
            records.insert(id.clone(), changed.clone());
            Ok(changed)
        })?;
        Self::changed(
            &signal,
            table,
            id,
            false,
            changed.data.to_value(),
            changed.permissions.into_iter().collect(),
        )
        .await
        .map_err(|error| failed(&format!("cannot signal permission change: {error}")))
    }

    async fn delete(
        &self,
        #[zbus(signal_emitter)] signal: SignalEmitter<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        table: String,
        id: String,
    ) -> zbus::fdo::Result<()> {
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        self.authenticate(sender.as_ref()).await?;
        let (table, id) = Self::entry(&table, &id)?;
        let removed = self.with_mutation(|state| {
            state
                .entries
                .get_mut(&table)
                .and_then(|records| records.remove(&id))
                .ok_or_else(|| failed("permission-store entry does not exist"))
        })?;
        Self::changed(
            &signal,
            table,
            id,
            true,
            removed.data.to_value(),
            removed.permissions.into_iter().collect(),
        )
        .await
        .map_err(|error| failed(&format!("cannot signal permission deletion: {error}")))
    }

    async fn set_value(
        &self,
        #[zbus(signal_emitter)] signal: SignalEmitter<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        table: String,
        create: bool,
        id: String,
        data: OwnedValue,
    ) -> zbus::fdo::Result<()> {
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        self.authenticate(sender.as_ref()).await?;
        let (table, id) = Self::entry(&table, &id)?;
        let data = StoredValue::from_value(data)?;
        let changed = self.with_mutation(|state| {
            let records = state.entries.entry(table.clone()).or_default();
            if let Some(entry) = records.get_mut(&id) {
                entry.data = data;
                return Ok(entry.clone());
            }
            if !create {
                return Err(failed("permission-store entry does not exist"));
            }
            if records.len() >= MAX_ENTRIES {
                return Err(failed("permission-store table is full"));
            }
            let changed = StoredEntry {
                permissions: BTreeMap::new(),
                data,
            };
            records.insert(id.clone(), changed.clone());
            Ok(changed)
        })?;
        Self::changed(
            &signal,
            table,
            id,
            false,
            changed.data.to_value(),
            changed.permissions.into_iter().collect(),
        )
        .await
        .map_err(|error| failed(&format!("cannot signal permission change: {error}")))
    }

    async fn set_permission(
        &self,
        #[zbus(signal_emitter)] signal: SignalEmitter<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        table: String,
        create: bool,
        id: String,
        app: String,
        permissions: Vec<String>,
    ) -> zbus::fdo::Result<()> {
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        self.authenticate(sender.as_ref()).await?;
        let (table, id) = Self::entry(&table, &id)?;
        valid_key(&app, MAX_APP, "application id")?;
        let permissions = checked_permission_list(permissions)?;
        let changed = self.with_mutation(|state| {
            let records = state.entries.entry(table.clone()).or_default();
            if !records.contains_key(&id) {
                if !create {
                    return Err(failed("permission-store entry does not exist"));
                }
                if records.len() >= MAX_ENTRIES {
                    return Err(failed("permission-store table is full"));
                }
                records.insert(id.clone(), StoredEntry::default());
            }
            let entry = records
                .get_mut(&id)
                .ok_or_else(|| failed("permission-store entry disappeared"))?;
            entry.permissions.insert(app, permissions);
            Ok(entry.clone())
        })?;
        Self::changed(
            &signal,
            table,
            id,
            false,
            changed.data.to_value(),
            changed.permissions.into_iter().collect(),
        )
        .await
        .map_err(|error| failed(&format!("cannot signal permission change: {error}")))
    }

    async fn delete_permission(
        &self,
        #[zbus(signal_emitter)] signal: SignalEmitter<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        table: String,
        id: String,
        app: String,
    ) -> zbus::fdo::Result<()> {
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        self.authenticate(sender.as_ref()).await?;
        let (table, id) = Self::entry(&table, &id)?;
        valid_key(&app, MAX_APP, "application id")?;
        let changed = self.with_mutation(|state| {
            let entry = state
                .entries
                .get_mut(&table)
                .and_then(|records| records.get_mut(&id))
                .ok_or_else(|| failed("permission-store entry does not exist"))?;
            entry
                .permissions
                .remove(&app)
                .ok_or_else(|| failed("permission-store permission does not exist"))?;
            Ok(entry.clone())
        })?;
        Self::changed(
            &signal,
            table,
            id,
            false,
            changed.data.to_value(),
            changed.permissions.into_iter().collect(),
        )
        .await
        .map_err(|error| failed(&format!("cannot signal permission change: {error}")))
    }

    async fn get_permission(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        table: String,
        id: String,
        app: String,
    ) -> zbus::fdo::Result<Vec<String>> {
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        self.authenticate(sender.as_ref()).await?;
        let (table, id) = Self::entry(&table, &id)?;
        valid_key(&app, MAX_APP, "application id")?;
        self.state
            .lock()
            .map_err(|_| failed("permission-store lock poisoned"))?
            .entries
            .get(&table)
            .and_then(|records| records.get(&id))
            .and_then(|entry| entry.permissions.get(&app))
            .cloned()
            .ok_or_else(|| failed("permission-store permission does not exist"))
    }

    async fn list(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        table: String,
    ) -> zbus::fdo::Result<Vec<String>> {
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        self.authenticate(sender.as_ref()).await?;
        valid_key(&table, MAX_TABLE, "table")?;
        Ok(self
            .state
            .lock()
            .map_err(|_| failed("permission-store lock poisoned"))?
            .entries
            .get(&table)
            .map(|entries| entries.keys().cloned().collect())
            .unwrap_or_default())
    }

    #[zbus(signal)]
    async fn changed(
        ctxt: &SignalEmitter<'_>,
        table: String,
        id: String,
        deleted: bool,
        data: OwnedValue,
        permissions: HashMap<String, Vec<String>>,
    ) -> zbus::Result<()>;
}

#[cfg(target_os = "linux")]
fn default_path() -> PathBuf {
    let base = super::private_state_root().unwrap_or_else(|| PathBuf::from("/dev/null"));
    base.join("tilo/portal/permissions.json")
}
fn failed(message: &str) -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(message.into())
}
fn invalid(message: &str) -> zbus::fdo::Error {
    zbus::fdo::Error::InvalidArgs(message.into())
}
#[cfg(target_os = "linux")]
fn checked_text(value: &str) -> bool {
    value.len() <= MAX_VALUE && !value.contains('\0')
}
fn valid_key(value: &str, max: usize, label: &str) -> zbus::fdo::Result<()> {
    (!value.is_empty() && value.len() <= max && !value.contains('\0') && !value.contains('/'))
        .then_some(())
        .ok_or_else(|| invalid(&format!("invalid {label}")))
}
#[cfg(target_os = "linux")]
fn checked_permission_list(values: Vec<String>) -> zbus::fdo::Result<Vec<String>> {
    (values.len() <= MAX_PERMISSIONS && values.iter().all(|v| checked_text(v)))
        .then_some(values)
        .ok_or_else(|| invalid("invalid permission list"))
}
#[cfg(target_os = "linux")]
fn checked_permissions(
    values: HashMap<String, Vec<String>>,
) -> zbus::fdo::Result<BTreeMap<String, Vec<String>>> {
    if values.len() > MAX_ENTRIES {
        return Err(invalid("too many permission principals"));
    }
    values
        .into_iter()
        .map(|(app, permissions)| {
            valid_key(&app, MAX_APP, "application id")?;
            Ok((app, checked_permission_list(permissions)?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn permission_store_persists_bounded_entries_and_rejects_path_keys() {
        // Phase `store_survives_reopen`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("permissions.json");
            let store = PermissionStoreData::at_path(&path);
            store
                .with_mutation(|state| {
                    state.entries.entry("documents".into()).or_default().insert(
                        "one".into(),
                        StoredEntry {
                            permissions: BTreeMap::from([(
                                "org.example.App".into(),
                                vec!["yes".into()],
                            )]),
                            data: StoredValue::Text("x".into()),
                        },
                    );
                    Ok(())
                })
                .unwrap();
            let reopened = PermissionStoreData::at_path(path);
            assert!(
                reopened
                    .state
                    .lock()
                    .unwrap()
                    .entries
                    .contains_key("documents")
            );
        }
        // Phase `rejects_path_like_keys`.
        assert!(PermissionStoreData::entry("table", "../escape").is_err());
        // Phase `arbitrary_bounded_variant_round_trips`.
        {
            let input = OwnedValue::try_from(zbus::zvariant::Value::new((
                7_u32,
                vec!["monitor-1".to_string(), "window-2".to_string()],
            )))
            .unwrap();
            let stored = StoredValue::from_value(input).unwrap();
            let restored: (u32, Vec<String>) = stored.to_value().try_into().unwrap();
            assert_eq!(restored, (7, vec!["monitor-1".into(), "window-2".into()]));
        }
    }
}
