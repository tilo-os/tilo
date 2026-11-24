//! The records this device keeps as its own before a person is founded on it, and after:
//! `devices/<device>/settings/<name>`.
//!
//! A computer nobody has set up yet has no workspace to keep a setting in, but its greeter and
//! launcher still need some: the keymap, how the network is reached. They are kept here, as the
//! device's records, and are read, written and followed through the one record API
//! ([`tilo_actions::remote::ResourceRead`]) rather than a second path of their own: the address's
//! workspace is [`tilo_actions::remote::device_record_space`], which no workspace is, and every
//! reader of a record reads them the way it reads a workspace's.
//!
//! Writes are typed actions on the one dispatch: `settings#set` and `settings#remove` on the
//! `tilo.device` provider, answered by the store here. In its own space the device alone reads
//! them: its operating system, the person whose certificate names it on this computer, and an app
//! on this device. In the person's workspace the rule a machine's controls admit by holds: their
//! person from any of their devices, and the administrators of a workspace this device serves.
//!
//! When the person's services start, the same store is published in the person's own workspace
//! (app [`tilo_actions::remote::DEVICE_SETTINGS`]), so the settings the device kept before it was
//! claimed are read there, by them, through the same read API; nothing is copied and the record's
//! one home stays the device until the settings resource takes it up.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tilo_actions::remote::{
    ActionCallError, Caller, DEVICE_SETTINGS, DEVICE_SETTINGS_REFUSED, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, RemoteActionMessage, ResourceAddress,
    ResourceCopy, ResourceReadRequest, ResourceReads, ResourceWake, device_record_space,
};
use tilo_lock::MutexRecover as _;

use super::ticket::{failed_as, succeeded};

/// The provider the settings actions are published under: the device's own, beside `devices`.
pub(crate) const PROVIDER: &str = tilo_device_identity::login::PROVIDER;

/// The resource the settings actions are on, within the device's provider.
pub(crate) const RESOURCE: &str = DEVICE_SETTINGS;

/// Write the setting `name` to `value`.
pub(crate) const SET: &str = "set";
/// Remove the setting `name`.
pub(crate) const REMOVE: &str = "remove";

/// Where a device keeps its records, below the login's directory.
pub(crate) const RECORDS: &str = "records";

/// The longest a setting's name may be, and the characters it may use: a name is a file below
/// the record directory, so nothing that could leave it.
const MAX_NAME_BYTES: usize = 128;

/// The device's own records, by name: the settings it keeps before anyone claims it.
pub struct DeviceRecords {
    /// The device whose records these are.
    device: [u8; 32],
    /// The space they are read in before anyone claims the device.
    space: String,
    /// Where they are kept.
    dir: PathBuf,
    /// The record spaces their readers reach them in: the device's own, and the person's own
    /// workspace once the person's services start.
    spaces: Mutex<Vec<String>>,
    /// The person whose certificate names the device, once one does: who reads its settings in
    /// their workspace, from any of their devices.
    person: Mutex<Option<[u8; 32]>>,
    /// The workspaces this device keeps, once the person's services run: who administers one
    /// decides who else reads the settings.
    workspaces: std::sync::OnceLock<std::sync::Weak<super::workspaces::ServiceWorkspaces>>,
    /// The records as they stand.
    records: Mutex<BTreeMap<String, Value>>,
    /// Moves on with every change, waking every reader following one.
    revision: tokio::sync::watch::Sender<u64>,
}

impl DeviceRecords {
    /// The records the device whose key is `device` keeps below `dir`, read from disk.
    #[must_use]
    pub fn under(dir: &Path, device: [u8; 32]) -> Self {
        let dir = dir.join(RECORDS);
        let mut records = BTreeMap::new();
        match std::fs::read_dir(&dir) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let Some(name) = path
                        .file_stem()
                        .and_then(std::ffi::OsStr::to_str)
                        .map(str::to_owned)
                    else {
                        continue;
                    };
                    if path.extension().and_then(std::ffi::OsStr::to_str) != Some("json")
                        || !valid_name(&name)
                    {
                        continue;
                    }
                    match std::fs::read(&path)
                        .ok()
                        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                    {
                        Some(value) => {
                            records.insert(name, value);
                        }
                        None => tracing::warn!(
                            path = %path.display(),
                            "a device setting does not read and is skipped"
                        ),
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(
                %error,
                dir = %dir.display(),
                "this device's records could not be read"
            ),
        }
        let space = device_record_space(&device);
        Self {
            device,
            space: space.clone(),
            dir,
            spaces: Mutex::new(vec![space]),
            person: Mutex::default(),
            workspaces: std::sync::OnceLock::new(),
            records: Mutex::new(records),
            revision: tokio::sync::watch::Sender::new(0),
        }
    }

    /// The workspaces this device keeps, bound once the person's services run: what decides
    /// whether a caller administers one of them.
    pub fn bind(&self, workspaces: &Arc<super::workspaces::ServiceWorkspaces>) {
        let _ = self.workspaces.set(Arc::downgrade(workspaces));
    }

    /// The records the device whose key is `device` keeps below the login's directory `place`.
    #[must_use]
    pub fn at(place: &Path, device: [u8; 32]) -> Self {
        Self::under(place, device)
    }

    /// Take the records into the person's own workspace, so their reads there answer the same
    /// settings: what the person's services do when they start.
    pub fn carry_into(&self, person: &[u8; 32]) {
        *self.person.lock_recover() = Some(*person);
        let workspace = tilo_server_protocol::servers::person_workspace_id(person);
        let mut spaces = self.spaces.lock_recover();
        if !spaces.contains(&workspace) {
            spaces.push(workspace);
        }
    }

    /// The setting `name` as it is now.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Value> {
        self.records.lock_recover().get(name).cloned()
    }

    /// Write the setting `name` as `value`: kept on disk and handed to every reader following it.
    ///
    /// # Errors
    /// The name is no setting name, or the setting could not be kept.
    pub fn set(&self, name: &str, value: Value) -> Result<(), String> {
        if !valid_name(name) {
            return Err(format!("`{name}` is no setting name"));
        }
        let bytes = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&self.dir).map_err(|error| error.to_string())?;
        let path = self.dir.join(format!("{name}.json"));
        tilo_files::native::atomic::write_with_mode(&path, &bytes, 0o600)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        self.records.lock_recover().insert(name.to_owned(), value);
        self.revision
            .send_modify(|revision| *revision = revision.wrapping_add(1));
        Ok(())
    }

    /// Remove the setting `name`; whether the device kept one.
    ///
    /// # Errors
    /// The name is no setting name, or the setting could not be removed.
    pub fn remove(&self, name: &str) -> Result<bool, String> {
        if !valid_name(name) {
            return Err(format!("`{name}` is no setting name"));
        }
        let kept = self.records.lock_recover().remove(name).is_some();
        if kept {
            let path = self.dir.join(format!("{name}.json"));
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("{}: {error}", path.display())),
            }
            self.revision
                .send_modify(|revision| *revision = revision.wrapping_add(1));
        }
        Ok(kept)
    }

    /// The revision the records stand at.
    fn revision(&self) -> u64 {
        *self.revision.borrow()
    }

    /// Whether the device's records are `caller`'s to read and write in `workspace`.
    ///
    /// Its own space, before anyone claims it, is the device's alone: its operating system, the
    /// person whose certificate names it on this computer, and an app on this device. In the
    /// person's workspace the rule a machine's controls admit by holds: the device's person, from
    /// any of their devices, and the administrators of a workspace this device serves
    /// ([`tilo_workspace_service::workspaces::Workspaces::administers_person`]). No other member, agent,
    /// bot, automation or stranger reads them.
    fn admits(&self, caller: &Caller, workspace: &str) -> Result<(), ActionCallError> {
        let refused = || ActionCallError::Refused {
            reason: DEVICE_SETTINGS_REFUSED.into(),
        };
        if caller.is_system() {
            return Ok(());
        }
        let acting = caller.acting();
        if caller.is_foreign_device()
            || caller.is_session_delegated()
            || acting.is_agent()
            || acting.is_bot()
            || acting.is_automation()
        {
            return Err(refused());
        }
        if caller.device_public_key() == Some(&self.device) {
            return Ok(());
        }
        if workspace == self.space {
            // An app on this device, acting for nobody because nobody is founded here yet.
            return if acting.app().is_some() && caller.user_public_key().is_none() {
                Ok(())
            } else {
                Err(refused())
            };
        }
        let Some(person) = caller.user_public_key() else {
            return Err(refused());
        };
        if self.person.lock_recover().as_ref() == Some(person) {
            return Ok(());
        }
        let Some(workspace) = caller.workspace().map(str::to_owned) else {
            return Err(refused());
        };
        let Some(served) = self.workspaces.get().and_then(std::sync::Weak::upgrade) else {
            return Err(refused());
        };
        let person = *person;
        let administers = super::workspaces::block_on_owned(async move {
            served
                .copies()
                .administers_person(&workspace, &person)
                .await
        });
        if administers { Ok(()) } else { Err(refused()) }
    }

    /// Whether `address` names a record of this device, read in one of the spaces its readers
    /// reach it in.
    fn names(&self, address: &ResourceAddress) -> bool {
        address.app == DEVICE_SETTINGS
            && self
                .spaces
                .lock_recover()
                .iter()
                .any(|space| *space == address.workspace)
    }

    /// Run the setting action `action` with `args` for `caller`: the one write path for the
    /// device's records.
    ///
    /// # Errors
    ///
    /// The caller may not write them, the action takes different arguments, or the setting could
    /// not be kept.
    pub fn act(
        &self,
        caller: &Caller,
        action: &str,
        args: &serde_json::Map<String, Value>,
    ) -> Result<Value, ActionCallError> {
        self.admits(caller, &self.space)?;
        let invalid = |why: String| ActionCallError::InvalidRequest { reason: why };
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("a setting's name is needed".to_owned()))?;
        match action {
            SET => {
                let value = args
                    .get("value")
                    .cloned()
                    .ok_or_else(|| invalid("the setting's value is needed".to_owned()))?;
                self.set(name, value)
                    .map(|()| serde_json::json!({ "name": name }))
                    .map_err(|reason| ActionCallError::Unavailable { reason })
            }
            REMOVE => self
                .remove(name)
                .map(|removed| serde_json::json!({ "name": name, "removed": removed }))
                .map_err(|reason| ActionCallError::Unavailable { reason }),
            other => Err(invalid(format!("`{other}` is no setting action"))),
        }
    }

    /// The receipt of the setting action `request` names, run for `caller`: what either dispatch
    /// answers an invocation of one with.
    pub fn receipt_for(&self, caller: &Caller, request: &InvokeRequest) -> InvocationReceipt {
        let invocation = request.invocation;
        let answer = match &request.argument {
            InvokeArgument::Json(args) => self.act(caller, &request.target.address.action, args),
            _ => Err(ActionCallError::InvalidRequest {
                reason: "a setting action asks with its arguments".to_owned(),
            }),
        };
        let update = match answer {
            Ok(value) => succeeded(invocation, value),
            Err(error) => failed_as(invocation, "device-setting", &error.to_string()),
        };
        InvocationState::new(invocation, update).receipt()
    }
}

/// Whether `name` is one setting's name: letters, digits, `.`, `_` and `-`, starting with a letter
/// or digit, so it is one file below the record directory and never leaves it.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && name
            .bytes()
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric())
}

impl ResourceReads for DeviceRecords {
    fn read(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, ActionCallError> {
        self.admits(caller, &address.workspace)?;
        if !self.names(address) {
            return Ok(ResourceCopy::Absent);
        }
        let revision = self.revision().to_string();
        Ok(match self.get(&address.record) {
            Some(value) => ResourceCopy::reading(value, Some(revision)),
            None => ResourceCopy::Absent,
        })
    }

    fn may_read(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(self.admits(caller, &address.workspace).and_then(|()| {
            if self.names(address) {
                Ok(())
            } else {
                Err(ActionCallError::Unavailable {
                    reason: tilo_actions::remote::RESOURCE_READS_UNAVAILABLE.into(),
                })
            }
        }))
    }

    fn list(
        &self,
        caller: &Caller,
        workspace: &str,
        app: &str,
    ) -> Result<Vec<ResourceAddress>, ActionCallError> {
        self.admits(caller, workspace)?;
        if app != DEVICE_SETTINGS
            || !self
                .spaces
                .lock_recover()
                .iter()
                .any(|space| space == workspace)
        {
            return Err(ActionCallError::Unavailable {
                reason: tilo_actions::remote::RESOURCE_READS_UNAVAILABLE.into(),
            });
        }
        Ok(self
            .records
            .lock_recover()
            .keys()
            .map(|name| ResourceAddress {
                workspace: workspace.to_owned(),
                app: DEVICE_SETTINGS.to_owned(),
                record: name.clone(),
            })
            .collect())
    }

    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: ResourceReadRequest,
    ) -> Pending<RemoteActionMessage> {
        tilo_actions::remote::answer_read(self, caller, correlation, read)
    }

    fn watch(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
        wake: &ResourceWake,
    ) -> Result<(), ActionCallError> {
        self.admits(caller, &address.workspace)?;
        if !self.names(address) {
            return Ok(());
        }
        let mut seen = self.revision.subscribe();
        let wake = wake.clone();
        tilo_rt::app().spawn(async move {
            if seen.changed().await.is_ok() {
                wake.wake();
            }
        });
        Ok(())
    }
}

#[cfg(test)]
#[path = "records_tests.rs"]
mod tests;
