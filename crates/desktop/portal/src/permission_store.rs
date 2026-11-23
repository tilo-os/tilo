//! The small, local database the portal frontend keeps its permission
//! choices and restore data in
//! (`org.freedesktop.impl.portal.PermissionStore`).
//!
//! Which app may use a device of this machine (the camera, the microphone)
//! is not kept here: those entries are a view of the person's device answers
//! ([`DeviceGrants`]), so what the frontend reads is what every other prompt
//! and the person's other devices read, and taking an answer back anywhere
//! takes it back here.
//!
//! Which app may read the microphone, where a dialog was, what an app was
//! granted: those are one person's decisions about the apps installed on
//! this device, so the store is user-private state beside the compositor's
//! and never collaborative workspace state. The portal frontend is the only
//! caller (it answered for the app already), and every key and value it
//! hands over is bounded before it is kept: table names, ids, application
//! ids, permission lists, values, and the entries of one table. A value
//! that would carry a file descriptor is refused, since keeping it is what
//! would make the store a channel for open files, and so is a key that
//! looks like a path: this is a table of records, not a filesystem.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, Str};

use crate::Portal;
use collab_types::caps::DeviceCap;
use tilo_action_policy::{Answer, Lifetime};
use tilo_peer_state::DeviceGrants;

/// The frontend's table of device permissions.
const DEVICES_TABLE: &str = "devices";
/// What the frontend writes for an app it allowed, and one it refused.
const YES: &str = "yes";
const NO: &str = "no";

/// The device entry `id` of `table` is about, when it is one.
fn device_of(table: &str, id: &str) -> Option<DeviceCap> {
    if table != DEVICES_TABLE {
        return None;
    }
    DEVICE_ENTRIES
        .into_iter()
        .find(|device| device.name() == id)
}

/// The devices the frontend's table has an entry for.
const DEVICE_ENTRIES: [DeviceCap; 2] = [DeviceCap::Camera, DeviceCap::Microphone];

const MAX_TABLE: usize = 96;
const MAX_ID: usize = 512;
const MAX_APP: usize = 255;
const MAX_PERMISSIONS: usize = 64;
const MAX_VALUE: usize = 16 * 1024;
const MAX_ENTRIES: usize = 4096;

/// A value's complete D-Bus variant encoding, which keeps bounded portal
/// restore tuples and dictionaries whole; empty for an entry without one.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct StoredValue(Vec<u8>);

impl StoredValue {
    fn from_value(value: OwnedValue) -> zbus::fdo::Result<Self> {
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
        Ok(Self(encoded.bytes().to_vec()))
    }

    fn to_value(&self) -> OwnedValue {
        let context = zbus::zvariant::serialized::Context::new_dbus(zbus::zvariant::LE, 0);
        let data = zbus::zvariant::serialized::Data::new_borrowed_fds(
            self.0.as_slice(),
            context,
            std::iter::empty::<zbus::zvariant::Fd<'_>>(),
        );
        data.deserialize::<OwnedValue>()
            .ok()
            .map_or_else(|| OwnedValue::from(Str::from("")), |(value, _)| value)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct StoredEntry {
    permissions: BTreeMap<String, Vec<String>>,
    data: StoredValue,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    entries: BTreeMap<String, BTreeMap<String, StoredEntry>>,
}

/// An entry's permissions: what each app was answered.
type Permissions = BTreeMap<String, Vec<String>>;

/// What `Changed` carries for an entry: its permissions and its value.
type ChangedEntry = (HashMap<String, Vec<String>>, OwnedValue);

/// The permission choices and restore data kept on this device, by table
/// and id.
#[derive(Clone)]
pub struct PermissionStore {
    state: Arc<Mutex<StoreFile>>,
    path: Option<Arc<PathBuf>>,
    devices: DeviceGrants,
    /// The device entries as the frontend last heard of them.
    announced: Arc<Mutex<BTreeMap<DeviceCap, Permissions>>>,
}

impl PermissionStore {
    /// The store kept at `path`; nothing is kept with `None`.
    #[must_use]
    pub fn load(path: Option<PathBuf>) -> Self {
        let state = path
            .as_deref()
            .map(tilo_files::native::atomic::json::load_or_default::<StoreFile>)
            .unwrap_or_default();
        Self {
            state: Arc::new(Mutex::new(state)),
            path: path.map(Arc::new),
            devices: DeviceGrants::unkept(),
            announced: Arc::default(),
        }
    }

    /// The same, answering its device entries from `devices`.
    #[must_use]
    pub fn answering(mut self, devices: DeviceGrants) -> Self {
        self.devices = devices;
        self
    }

    /// The apps the person answered for `device` on this device, as the frontend spells them.
    fn device_permissions(&self, device: DeviceCap) -> BTreeMap<String, Vec<String>> {
        self.devices
            .kept()
            .into_iter()
            .filter(|kept| kept.this_device && kept.answer.cap == device)
            .map(|kept| {
                let word = if kept.answer.answer == Answer::Allow {
                    YES
                } else {
                    NO
                };
                (kept.answer.app, vec![word.to_owned()])
            })
            .collect()
    }

    /// The device entries whose permissions changed since the frontend last heard of them, however
    /// they changed: here, or by the person on another of their devices.
    pub fn changed_devices(&self) -> Vec<(String, ChangedEntry)> {
        let mut announced = self.announced.lock_recover();
        DEVICE_ENTRIES
            .into_iter()
            .filter_map(|device| {
                let now = self.device_permissions(device);
                (announced.get(&device) != Some(&now)).then(|| {
                    announced.insert(device, now);
                    let entry = self.viewed(DEVICES_TABLE, device.name(), StoredEntry::default());
                    (device.name().to_owned(), changed_of(&entry))
                })
            })
            .collect()
    }

    /// Keep what the frontend answered for `app` of `device` as the person's answer.
    fn answer_device(&self, device: DeviceCap, app: &str, permissions: &[String]) {
        let answer = if permissions.iter().any(|word| word == YES) {
            Answer::Allow
        } else {
            Answer::Deny
        };
        self.devices
            .answer(app, device, answer, Lifetime::UntilRevoked);
    }

    /// Take back what `app` was answered of `device` on this device.
    fn revoke_device(&self, device: DeviceCap, app: &str) -> bool {
        self.devices.revoke_here(app, device)
    }

    /// `entry` with its permissions read from the person's answers when it is a device's.
    fn viewed(&self, table: &str, id: &str, mut entry: StoredEntry) -> StoredEntry {
        if let Some(device) = device_of(table, id) {
            entry.permissions = self.device_permissions(device);
        }
        entry
    }

    /// Entry `id` of `table`: its permissions and its value.
    ///
    /// # Errors
    /// An invalid key, or an entry that does not exist.
    pub fn lookup(&self, table: &str, id: &str) -> zbus::fdo::Result<ChangedEntry> {
        valid_key(table, MAX_TABLE, "table")?;
        valid_key(id, MAX_ID, "id")?;
        let state = self.state.lock_recover();
        let entry = state
            .entries
            .get(table)
            .and_then(|records| records.get(id))
            .cloned()
            .or_else(|| device_of(table, id).map(|_| StoredEntry::default()))
            .ok_or_else(|| failed("permission-store entry does not exist"))?;
        drop(state);
        Ok(changed_of(&self.viewed(table, id, entry)))
    }

    /// Write entry `id` of `table` whole: its permissions and its value.
    ///
    /// # Errors
    /// An invalid or oversized key, permission list or value, an entry that
    /// does not exist without `create`, a full table, or a store that could
    /// not be written.
    pub fn set(
        &self,
        table: &str,
        create: bool,
        id: &str,
        permissions: HashMap<String, Vec<String>>,
        data: OwnedValue,
    ) -> zbus::fdo::Result<ChangedEntry> {
        valid_key(table, MAX_TABLE, "table")?;
        valid_key(id, MAX_ID, "id")?;
        let mut permissions = checked_permissions(permissions)?;
        let data = StoredValue::from_value(data)?;
        if let Some(device) = device_of(table, id) {
            for app in self.device_permissions(device).keys() {
                if !permissions.contains_key(app) {
                    self.revoke_device(device, app);
                }
            }
            for (app, words) in std::mem::take(&mut permissions) {
                self.answer_device(device, &app, &words);
            }
        }
        let viewed = self.with_mutation(|state| {
            let records = state.entries.entry(table.to_owned()).or_default();
            if !records.contains_key(id) {
                if !create {
                    return Err(failed("permission-store entry does not exist"));
                }
                if records.len() >= MAX_ENTRIES {
                    return Err(failed("permission-store table is full"));
                }
            }
            let entry = StoredEntry { permissions, data };
            records.insert(id.to_owned(), entry.clone());
            Ok(entry)
        })?;
        Ok(changed_of(&self.viewed(table, id, viewed)))
    }

    /// Remove entry `id` from `table`, which is dropped with its last entry.
    ///
    /// # Errors
    /// An invalid key, or an entry that does not exist.
    pub fn delete(&self, table: &str, id: &str) -> zbus::fdo::Result<ChangedEntry> {
        valid_key(table, MAX_TABLE, "table")?;
        valid_key(id, MAX_ID, "id")?;
        if let Some(device) = device_of(table, id) {
            let answered = self.device_permissions(device);
            for app in answered.keys() {
                self.revoke_device(device, app);
            }
            let data = self
                .state
                .lock_recover()
                .entries
                .get_mut(table)
                .and_then(|entries| entries.remove(id))
                .unwrap_or_default()
                .data;
            if answered.is_empty() && data == StoredValue::default() {
                return Err(failed("permission-store entry does not exist"));
            }
            return Ok(changed_of(&StoredEntry {
                permissions: answered,
                data,
            }));
        }
        self.with_mutation(|state| {
            let entry = state
                .entries
                .get_mut(table)
                .and_then(|records| records.remove(id))
                .ok_or_else(|| failed("permission-store entry does not exist"))?;
            if state.entries.get(table).is_some_and(BTreeMap::is_empty) {
                state.entries.remove(table);
            }
            Ok(changed_of(&entry))
        })
    }

    /// Write only the value of entry `id` of `table`, keeping its
    /// permissions.
    ///
    /// # Errors
    /// An invalid or oversized key or value, an entry that does not exist
    /// without `create`, a full table, or a store that could not be written.
    pub fn set_value(
        &self,
        table: &str,
        create: bool,
        id: &str,
        data: OwnedValue,
    ) -> zbus::fdo::Result<ChangedEntry> {
        valid_key(table, MAX_TABLE, "table")?;
        valid_key(id, MAX_ID, "id")?;
        let data = StoredValue::from_value(data)?;
        self.with_mutation(|state| {
            let records = state.entries.entry(table.to_owned()).or_default();
            if let Some(entry) = records.get_mut(id) {
                entry.data = data;
                return Ok(changed_of(entry));
            }
            if !create {
                return Err(failed("permission-store entry does not exist"));
            }
            if records.len() >= MAX_ENTRIES {
                return Err(failed("permission-store table is full"));
            }
            let entry = StoredEntry {
                permissions: BTreeMap::new(),
                data,
            };
            records.insert(id.to_owned(), entry.clone());
            Ok(changed_of(&entry))
        })
    }

    /// Set what application `app` may do with entry `id` of `table`.
    ///
    /// # Errors
    /// An invalid or oversized key or permission list, an entry that does
    /// not exist without `create`, a full table, or a store that could not
    /// be written.
    pub fn set_permission(
        &self,
        table: &str,
        create: bool,
        id: &str,
        app: &str,
        permissions: Vec<String>,
    ) -> zbus::fdo::Result<ChangedEntry> {
        valid_key(table, MAX_TABLE, "table")?;
        valid_key(id, MAX_ID, "id")?;
        valid_key(app, MAX_APP, "application id")?;
        let permissions = checked_permission_list(permissions)?;
        if let Some(device) = device_of(table, id) {
            self.answer_device(device, app, &permissions);
            return self.lookup(table, id);
        }
        self.with_mutation(|state| {
            let records = state.entries.entry(table.to_owned()).or_default();
            if !records.contains_key(id) {
                if !create {
                    return Err(failed("permission-store entry does not exist"));
                }
                if records.len() >= MAX_ENTRIES {
                    return Err(failed("permission-store table is full"));
                }
                records.insert(id.to_owned(), StoredEntry::default());
            }
            let entry = records
                .get_mut(id)
                .ok_or_else(|| failed("permission-store entry disappeared"))?;
            entry.permissions.insert(app.to_owned(), permissions);
            Ok(changed_of(entry))
        })
    }

    /// Forget what application `app` may do with entry `id` of `table`.
    ///
    /// # Errors
    /// An invalid key, or an entry or permission that does not exist.
    pub fn delete_permission(
        &self,
        table: &str,
        id: &str,
        app: &str,
    ) -> zbus::fdo::Result<ChangedEntry> {
        valid_key(table, MAX_TABLE, "table")?;
        valid_key(id, MAX_ID, "id")?;
        valid_key(app, MAX_APP, "application id")?;
        if let Some(device) = device_of(table, id) {
            if !self.revoke_device(device, app) {
                return Err(failed("permission-store permission does not exist"));
            }
            return self.lookup(table, id);
        }
        self.with_mutation(|state| {
            let entry = state
                .entries
                .get_mut(table)
                .and_then(|records| records.get_mut(id))
                .ok_or_else(|| failed("permission-store entry does not exist"))?;
            entry
                .permissions
                .remove(app)
                .ok_or_else(|| failed("permission-store permission does not exist"))?;
            Ok(changed_of(entry))
        })
    }

    /// What application `app` may do with entry `id` of `table`.
    ///
    /// # Errors
    /// An invalid key, or an entry or permission that does not exist.
    pub fn get_permission(
        &self,
        table: &str,
        id: &str,
        app: &str,
    ) -> zbus::fdo::Result<Vec<String>> {
        valid_key(table, MAX_TABLE, "table")?;
        valid_key(id, MAX_ID, "id")?;
        valid_key(app, MAX_APP, "application id")?;
        if let Some(device) = device_of(table, id) {
            return self
                .device_permissions(device)
                .remove(app)
                .ok_or_else(|| failed("permission-store permission does not exist"));
        }
        self.state
            .lock_recover()
            .entries
            .get(table)
            .and_then(|records| records.get(id))
            .and_then(|entry| entry.permissions.get(app))
            .cloned()
            .ok_or_else(|| failed("permission-store permission does not exist"))
    }

    /// The ids `table` holds; empty when there is no such table.
    ///
    /// # Errors
    /// An invalid table name.
    pub fn list(&self, table: &str) -> zbus::fdo::Result<Vec<String>> {
        valid_key(table, MAX_TABLE, "table")?;
        Ok(self
            .state
            .lock_recover()
            .entries
            .get(table)
            .map(|records| records.keys().cloned().collect())
            .unwrap_or_default())
    }

    fn commit(&self, state: &StoreFile) -> zbus::fdo::Result<()> {
        let Some(path) = &self.path else {
            return Err(failed(
                "no private state directory to keep the permission in",
            ));
        };
        tilo_files::native::atomic::json::write_private(path, state)
            .map_err(|error| failed(&format!("cannot commit permission store: {error}")))
    }

    /// Run `change` and commit its result, leaving the store as it was when
    /// either step refuses.
    fn with_mutation<T>(
        &self,
        change: impl FnOnce(&mut StoreFile) -> zbus::fdo::Result<T>,
    ) -> zbus::fdo::Result<T> {
        let mut state = self.state.lock_recover();
        let previous = state.clone();
        let result = change(&mut state).and_then(|changed| self.commit(&state).map(|()| changed));
        if result.is_err() {
            *state = previous;
        }
        drop(state);
        result
    }
}

fn changed_of(entry: &StoredEntry) -> ChangedEntry {
    (
        entry.permissions.clone().into_iter().collect(),
        entry.data.to_value(),
    )
}

/// The `org.freedesktop.impl.portal.PermissionStore` interface.
#[derive(Clone)]
pub struct PermissionStoreBackend {
    portal: Portal,
}

impl PermissionStoreBackend {
    #[must_use]
    pub const fn new(portal: Portal) -> Self {
        Self { portal }
    }
}

/// Tell the frontend about every device entry that changed since it last heard: the person's
/// answers change on their other devices too, and the frontend keeps what it read.
pub(crate) async fn announce_devices(connection: &zbus::Connection, store: &PermissionStore) {
    for (id, changed) in store.changed_devices() {
        if let Err(error) = announce(connection, DEVICES_TABLE.to_owned(), id, false, changed).await
        {
            tracing::warn!(%error, "the frontend did not hear of a device answer");
        }
    }
}

/// Tell the frontend that `table` changed, whichever call changed it.
async fn announce(
    connection: &zbus::Connection,
    table: String,
    id: String,
    deleted: bool,
    changed: ChangedEntry,
) -> zbus::fdo::Result<()> {
    let (permissions, data) = changed;
    let emitter = SignalEmitter::new(connection, crate::OBJECT_PATH)
        .map_err(|error| failed(&format!("cannot signal permission change: {error}")))?;
    PermissionStoreBackend::changed(&emitter, table, id, deleted, data, permissions)
        .await
        .map_err(|error| failed(&format!("cannot signal permission change: {error}")))
}

#[interface(name = "org.freedesktop.impl.portal.PermissionStore")]
impl PermissionStoreBackend {
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        2
    }

    async fn lookup(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        table: String,
        id: String,
    ) -> zbus::fdo::Result<ChangedEntry> {
        self.portal.frontend(&header).await?;
        self.portal.permissions.lookup(&table, &id)
    }

    async fn set(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        table: String,
        create: bool,
        id: String,
        permissions: HashMap<String, Vec<String>>,
        data: OwnedValue,
    ) -> zbus::fdo::Result<()> {
        self.portal.frontend(&header).await?;
        let changed = self
            .portal
            .permissions
            .set(&table, create, &id, permissions, data)?;
        announce(connection, table, id, false, changed).await
    }

    async fn delete(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        table: String,
        id: String,
    ) -> zbus::fdo::Result<()> {
        self.portal.frontend(&header).await?;
        let changed = self.portal.permissions.delete(&table, &id)?;
        announce(connection, table, id, true, changed).await
    }

    async fn set_value(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        table: String,
        create: bool,
        id: String,
        data: OwnedValue,
    ) -> zbus::fdo::Result<()> {
        self.portal.frontend(&header).await?;
        let changed = self
            .portal
            .permissions
            .set_value(&table, create, &id, data)?;
        announce(connection, table, id, false, changed).await
    }

    async fn set_permission(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        table: String,
        create: bool,
        id: String,
        app: String,
        permissions: Vec<String>,
    ) -> zbus::fdo::Result<()> {
        self.portal.frontend(&header).await?;
        let changed =
            self.portal
                .permissions
                .set_permission(&table, create, &id, &app, permissions)?;
        announce(connection, table, id, false, changed).await
    }

    async fn delete_permission(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        table: String,
        id: String,
        app: String,
    ) -> zbus::fdo::Result<()> {
        self.portal.frontend(&header).await?;
        let changed = self
            .portal
            .permissions
            .delete_permission(&table, &id, &app)?;
        announce(connection, table, id, false, changed).await
    }

    async fn get_permission(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        table: String,
        id: String,
        app: String,
    ) -> zbus::fdo::Result<Vec<String>> {
        self.portal.frontend(&header).await?;
        self.portal.permissions.get_permission(&table, &id, &app)
    }

    async fn list(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        table: String,
    ) -> zbus::fdo::Result<Vec<String>> {
        self.portal.frontend(&header).await?;
        self.portal.permissions.list(&table)
    }

    #[zbus(signal)]
    async fn changed(
        emitter: &SignalEmitter<'_>,
        table: String,
        id: String,
        deleted: bool,
        data: OwnedValue,
        permissions: HashMap<String, Vec<String>>,
    ) -> zbus::Result<()>;
}

fn failed(message: &str) -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(message.into())
}

fn invalid(message: &str) -> zbus::fdo::Error {
    zbus::fdo::Error::InvalidArgs(message.into())
}

/// A table name, id or application id the store can be keyed by: no
/// separators and no paths.
fn valid_key(value: &str, max: usize, label: &str) -> zbus::fdo::Result<()> {
    (!value.is_empty() && value.len() <= max && !value.contains('\0') && !value.contains('/'))
        .then_some(())
        .ok_or_else(|| invalid(&format!("invalid {label}")))
}

fn checked_text(value: &str) -> bool {
    value.len() <= MAX_VALUE && !value.contains('\0')
}

fn checked_permission_list(values: Vec<String>) -> zbus::fdo::Result<Vec<String>> {
    (values.len() <= MAX_PERMISSIONS && values.iter().all(|value| checked_text(value)))
        .then_some(values)
        .ok_or_else(|| invalid("invalid permission list"))
}

fn checked_permissions(
    values: HashMap<String, Vec<String>>,
) -> zbus::fdo::Result<BTreeMap<String, Vec<String>>> {
    if values.len() > MAX_ENTRIES {
        return Err(invalid("too many permission actors"));
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

    fn value() -> OwnedValue {
        OwnedValue::from(Str::from("value"))
    }

    fn documents() -> HashMap<String, Vec<String>> {
        HashMap::from([("org.example.App".to_owned(), vec!["yes".to_owned()])])
    }

    tilo_test_env::scenario! {
        fn the_permission_store_keeps_bounded_entries_and_only_in_a_private_directory;

        phase kept_entries_survive_a_restart_and_deletes_and_list_agree;
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("permissions.json");
            let store = PermissionStore::load(Some(path.clone()));
            // An arbitrary bounded variant, as an app's restore data is.
            let restore = OwnedValue::try_from(zbus::zvariant::Value::new((
                7_u32,
                vec!["monitor-1".to_owned(), "window-2".to_owned()],
            )))
            .unwrap();
            store
                .set("documents", true, "one", documents(), restore)
                .unwrap();
            store
                .set_permission(
                    "documents",
                    true,
                    "two",
                    "org.example.Other",
                    vec!["no".to_owned()],
                )
                .unwrap();
            assert_eq!(store.list("documents").unwrap(), ["one", "two"]);
            assert!(store.list("elsewhere").unwrap().is_empty(), "no such table");
            drop(store);

            let restarted = PermissionStore::load(Some(path.clone()));
            let (permissions, data) = restarted.lookup("documents", "one").unwrap();
            assert_eq!(permissions, documents());
            let restored: (u32, Vec<String>) = data.try_into().unwrap();
            assert_eq!(
                restored,
                (7, vec!["monitor-1".to_owned(), "window-2".to_owned()])
            );
            assert_eq!(
                restarted
                    .get_permission("documents", "two", "org.example.Other")
                    .unwrap(),
                ["no"]
            );
            assert_eq!(restarted.list("documents").unwrap(), ["one", "two"]);

            let (forgotten, _) = restarted.delete("documents", "one").unwrap();
            assert_eq!(forgotten, documents());
            assert!(restarted.lookup("documents", "one").is_err());
            assert_eq!(restarted.list("documents").unwrap(), ["two"]);
            let after: PermissionStore = PermissionStore::load(Some(path.clone()));
            assert_eq!(
                after.list("documents").unwrap(),
                ["two"],
                "the delete was kept"
            );
            assert!(after.lookup("documents", "one").is_err());
            // The last entry takes its table with it.
            after.delete("documents", "two").unwrap();
            assert!(after.list("documents").unwrap().is_empty());
            assert_eq!(
                PermissionStore::load(Some(path)).list("documents").unwrap(),
                Vec::<String>::new()
            );
        }

        phase out_of_range_keys_and_values_are_refused_and_change_nothing;
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("permissions.json");
            let store = PermissionStore::load(Some(path.clone()));
            store
                .set("documents", true, "one", documents(), value())
                .unwrap();

            let empty = HashMap::new();
            // A key that looks like a path.
            assert!(
                store
                    .set("documents", true, "../escape", empty.clone(), value())
                    .is_err()
            );
            assert!(
                store
                    .set("../documents", true, "two", empty.clone(), value())
                    .is_err()
            );
            // A table name over the limit.
            assert!(
                store
                    .set(
                        &"t".repeat(MAX_TABLE + 1),
                        true,
                        "two",
                        empty.clone(),
                        value()
                    )
                    .is_err()
            );
            // A value over the limit.
            assert!(
                store
                    .set(
                        "documents",
                        true,
                        "two",
                        empty.clone(),
                        OwnedValue::from(Str::from("x".repeat(MAX_VALUE + 1))),
                    )
                    .is_err()
            );
            // A value that would carry a file descriptor.
            let (read, _write) = std::io::pipe().unwrap();
            let descriptor = OwnedValue::try_from(zbus::zvariant::Value::from(
                zbus::zvariant::Fd::from(std::os::fd::OwnedFd::from(read)),
            ))
            .unwrap();
            assert!(
                store
                    .set("documents", true, "two", empty, descriptor)
                    .is_err()
            );
            // An application id over the limit, on an entry it would otherwise
            // have created.
            assert!(
                store
                    .set_permission(
                        "documents",
                        true,
                        "two",
                        &"a".repeat(MAX_APP + 1),
                        Vec::new(),
                    )
                    .is_err()
            );

            assert!(store.lookup("documents", "two").is_err());
            assert_eq!(store.list("documents").unwrap(), ["one"]);
            assert_eq!(
                PermissionStore::load(Some(path)).list("documents").unwrap(),
                ["one"],
                "a refused call wrote nothing"
            );
        }

        phase a_store_without_a_private_directory_keeps_nothing;
        {
            let store = PermissionStore::load(None);
            assert!(
                store
                    .set("documents", true, "one", documents(), value())
                    .is_err()
            );
            assert!(store.list("documents").unwrap().is_empty());
        }
    }

    #[test]
    fn the_frontends_camera_answer_is_the_persons_device_answer() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("permissions.json");
        let grants = DeviceGrants::unkept();
        let store = PermissionStore::load(Some(path.clone())).answering(grants.clone());
        store
            .set_permission(
                "devices",
                true,
                "camera",
                "org.example.App",
                vec![YES.to_owned()],
            )
            .expect("the frontend's answer is taken");
        assert_eq!(
            grants.decide("org.example.App", None, DeviceCap::Camera),
            Answer::Allow,
            "the camera answer decides as every other device answer does"
        );
        assert!(
            std::fs::read_to_string(&path).map_or(true, |kept| !kept.contains("org.example.App")),
            "no table on this device keeps it"
        );
        store
            .delete_permission("devices", "camera", "org.example.App")
            .expect("the frontend takes it back");
        assert_eq!(
            grants.decide("org.example.App", None, DeviceCap::Camera),
            Answer::Ask
        );
    }

    /// Each device entry is told to the frontend once until it changes.
    #[test]
    fn a_device_entry_is_announced_once_until_it_changes() {
        let store = PermissionStore::load(None);
        let first: Vec<String> = store
            .changed_devices()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            first,
            ["camera", "microphone"],
            "the frontend hears each entry once"
        );
        assert!(store.changed_devices().is_empty());
    }
}
