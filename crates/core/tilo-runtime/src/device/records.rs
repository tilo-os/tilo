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
//! on this device. In the person's workspace the one policy decision decides at the setting's own
//! node: their person from any of their devices, and whoever they granted that setting there.
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
    InvocationState, InvokeArgument, InvokeRequest, ReadsAnswer, ResourceAddress, ResourceCopy,
    ResourceReads, ResourceWake, device_record_space, reads_answer,
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
    /// The workspaces this device keeps, once the person's services run: where the policy that
    /// decides who else reads and writes the settings is read.
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
                    let read = std::fs::read(&path)
                        .ok()
                        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
                    if let Some(value) = read {
                        records.insert(name, value);
                    } else {
                        tracing::warn!(
                            path = %path.display(),
                            "a device setting does not read and is skipped"
                        );
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

    /// The workspaces this device keeps, bound once the person's services run: where the policy
    /// that decides who else reads and writes the settings is read.
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

    /// Whether the device's record `name` is `caller`'s to take `kind` at in `workspace`.
    ///
    /// Its own space, before anyone claims it, is the device's alone: its operating system, the
    /// person whose certificate names it on this computer, and an app on this device. In the
    /// person's workspace the one policy decision decides at the setting's own node
    /// (`devices/<device>/settings/<name>`): the device's person reads and writes it from any of
    /// their devices, and anyone else only by a grant naming them there, for that setting. No
    /// workspace's ownership or role reaches it.
    fn allows(
        &self,
        caller: &Caller,
        workspace: &str,
        name: &str,
        kind: tilo_action_policy::ActionKind,
    ) -> Result<(), ActionCallError> {
        let refused = || ActionCallError::Refused {
            reason: DEVICE_SETTINGS_REFUSED.into(),
        };
        if caller.is_system() {
            return Ok(());
        }
        if workspace == self.space {
            if caller.device_public_key() == Some(&self.device) {
                return Ok(());
            }
            // An app on this device, acting for nobody because nobody is founded here yet.
            return if caller.acting().app().is_some() && caller.user_public_key().is_none() {
                Ok(())
            } else {
                Err(refused())
            };
        }
        let Some(person) = *self.person.lock_recover() else {
            return Err(refused());
        };
        let policy = self
            .workspaces
            .get()
            .and_then(std::sync::Weak::upgrade)
            .and_then(|workspaces| {
                tilo_server_protocol::WorkspaceId::parse(workspace)
                    .and_then(|workspace| workspaces.policy_of(workspace)())
            });
        // The person's own workspace keeps the grants an ordinary record is decided by; with no
        // copy here (a device setting read on its own), that decision is still the person's own.
        let policy = policy.unwrap_or_else(|| {
            tilo_action_policy::WorkspacePolicy::unowned(
                workspace,
                &tilo_action_policy::Grantee::person(hex::encode(person)),
            )
        });
        let node = tilo_workspace_service::workspaces::server_controls::settings_node(
            workspace,
            &self.device,
            name,
        );
        if tilo_workspace_service::workspaces::server_controls::caller_may(
            &policy, caller, &node, kind,
        ) {
            Ok(())
        } else {
            Err(refused())
        }
    }

    /// Whether `address` names a record of this device, read in one of the spaces its readers
    /// reach it in.
    fn names(&self, address: &ResourceAddress) -> bool {
        if address.app != DEVICE_SETTINGS {
            return false;
        }
        let spaces = self.spaces.lock_recover();
        let named = spaces.contains(&address.workspace);
        drop(spaces);
        named
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
        let invalid = |why: String| ActionCallError::InvalidRequest { reason: why };
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("a setting's name is needed".to_owned()))?;
        // A caller kept by the device's own space writes there; everyone else writes the person's
        // settings, decided at the setting's node in their own workspace.
        let workspace = match caller.workspace() {
            Some(asked) if asked != self.space => self
                .person
                .lock_recover()
                .map(|person| tilo_server_protocol::servers::person_workspace_id(&person))
                .unwrap_or_else(|| self.space.clone()),
            _ => self.space.clone(),
        };
        self.allows(
            caller,
            &workspace,
            name,
            tilo_action_policy::ActionKind::Undoable,
        )?;
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
    fn read<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a ResourceAddress,
    ) -> ReadsAnswer<'a, ResourceCopy> {
        reads_answer(async move {
            self.allows(
                caller,
                &address.workspace,
                &address.record,
                tilo_action_policy::ActionKind::Read,
            )?;
            if !self.names(address) {
                return Ok(ResourceCopy::Absent);
            }
            let revision = self.revision().to_string();
            Ok(match self.get(&address.record) {
                Some(value) => ResourceCopy::reading(value, Some(revision)),
                None => ResourceCopy::Absent,
            })
        })
    }

    fn may_read<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a ResourceAddress,
    ) -> ReadsAnswer<'a, ()> {
        Box::pin(std::future::ready(
            self.allows(
                caller,
                &address.workspace,
                &address.record,
                tilo_action_policy::ActionKind::Read,
            )
            .and_then(|()| {
                if self.names(address) {
                    Ok(())
                } else {
                    Err(ActionCallError::Unavailable {
                        reason: tilo_actions::remote::RESOURCE_READS_UNAVAILABLE.into(),
                    })
                }
            }),
        ))
    }

    fn list<'a>(
        &'a self,
        caller: &'a Caller,
        workspace: &'a str,
        app: &'a str,
    ) -> ReadsAnswer<'a, Vec<tilo_actions::remote::ListedResource>> {
        reads_answer(async move {
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
            let mut names: Vec<String> = self.records.lock_recover().keys().cloned().collect();
            names.retain(|name| {
                self.allows(
                    caller,
                    workspace,
                    name,
                    tilo_action_policy::ActionKind::Read,
                )
                .is_ok()
            });
            Ok(names
                .into_iter()
                .map(|name| {
                    tilo_actions::remote::ListedResource::document(ResourceAddress {
                        workspace: workspace.to_owned(),
                        app: DEVICE_SETTINGS.to_owned(),
                        record: name,
                    })
                })
                .collect())
        })
    }

    fn watch<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a ResourceAddress,
        wake: &'a ResourceWake,
    ) -> ReadsAnswer<'a, ()> {
        reads_answer(async move {
            self.allows(
                caller,
                &address.workspace,
                &address.record,
                tilo_action_policy::ActionKind::Read,
            )?;
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
        })
    }
}

#[cfg(test)]
#[path = "records_tests.rs"]
mod tests;
