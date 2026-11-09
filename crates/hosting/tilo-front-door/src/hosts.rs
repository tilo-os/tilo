//! The directory as the hosts themselves answer it.
//!
//! A person's workspaces are the ones their hosts govern, and a host lists
//! and creates them as its own operations (`room.list`, `room.create` of
//! provider `tilo.host`), which it offers only to devices of its own person
//! and may refuse. The door asks them as such a device: for each person it
//! serves it holds a device key their identity root certified, and opens a
//! session to the host as that device, exactly as their laptop would.
//!
//! Which hosts a person has, and where each host serves its workspaces'
//! agent endpoints (`tilo-runtime`'s HTTP MCP, one per workspace, behind
//! the token that proves the caller is the door), come from the
//! [`Registry`] the hosting side keeps. The door reads it again whenever it
//! changes, so a person's new host needs no restart.

use crate::directory::{BoxFuture, Directory, DirectoryError, Host, Route, Workspace};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime},
};
use tilo_actions::remote::{InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_relay_session::RelayClient;
use tilo_relay_session::workspaces::{
    CREATE_ROOM, CreatedWorkspace, HOST_ACTIONS_PROVIDER, HOST_ROOMS_RESOURCE, LIST_ROOMS,
    WorkspaceListing, ask_host,
};
use tilo_user_identity::{DeviceCert, RevocationList};

/// How long a host has to answer.
const HOST_TIMEOUT: Duration = Duration::from_secs(30);
/// What stands for the workspace in an agent endpoint's URL.
pub const WORKSPACE_PLACEHOLDER: &str = "{workspace}";

/// The hosting side's record of whom the door serves.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    /// By the person's identity root key, hex.
    pub people: HashMap<String, PersonEntry>,
}

/// One person the door serves.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonEntry {
    /// The door's device of this person: its key and the certificate the
    /// person's root gave it, both files the hosting side wrote.
    pub device: DeviceFiles,
    pub hosts: Vec<HostEntry>,
    /// The person's revocation list, as their hosts keep it, when the
    /// hosting side mirrors it here: the devices that may sign nobody in.
    #[serde(default)]
    pub revocations_file: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceFiles {
    /// The device's Ed25519 secret key, 32 raw bytes.
    pub key_file: PathBuf,
    /// Its `DeviceCert`, canonical encoding.
    pub cert_file: PathBuf,
}

/// A host of the person.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostEntry {
    /// The host's name, as people see it (`eu1.tilo.sh`).
    pub name: String,
    /// Whether new workspaces go here when the agent names no host.
    #[serde(default)]
    pub default: bool,
    /// The host's device key, hex: the certificate it shows must be for it.
    pub device: String,
    /// Where the host is reached.
    pub addr: iroh_base::EndpointAddr,
    /// Where it serves a workspace's agent endpoint: a URL with
    /// [`WORKSPACE_PLACEHOLDER`] for the workspace's id.
    pub agents: String,
    /// The file holding the bearer token its agent endpoints take from the
    /// door.
    pub agents_token_file: PathBuf,
}

/// The registry at a path, read again when the file changes.
pub struct Hosts {
    path: PathBuf,
    cached: Mutex<Option<(SystemTime, Registry)>>,
}

impl Hosts {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            cached: Mutex::new(None),
        }
    }

    fn registry(&self) -> Result<Registry, DirectoryError> {
        let unavailable = |error: &dyn std::fmt::Display| {
            DirectoryError::Unavailable(format!("the hosts registry: {error}"))
        };
        let modified = std::fs::metadata(&self.path)
            .and_then(|metadata| metadata.modified())
            .map_err(|error| unavailable(&error))?;
        let mut cached = self
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((at, registry)) = cached.as_ref()
            && *at == modified
        {
            return Ok(registry.clone());
        }
        let bytes = std::fs::read(&self.path).map_err(|error| unavailable(&error))?;
        let registry: Registry =
            serde_json::from_slice(&bytes).map_err(|error| unavailable(&error))?;
        *cached = Some((modified, registry.clone()));
        drop(cached);
        Ok(registry)
    }

    fn person(&self, person: &str) -> Result<PersonEntry, DirectoryError> {
        self.registry()?
            .people
            .remove(person)
            .ok_or(DirectoryError::NotFound)
    }
}

/// The door's device of a person, ready to dial their hosts.
struct Device {
    cert: DeviceCert,
    key: ed25519_dalek::SigningKey,
}

fn device(files: &DeviceFiles) -> Result<Device, DirectoryError> {
    let unavailable = |what: &str, path: &Path| {
        DirectoryError::Unavailable(format!("{what} at {} cannot be read", path.display()))
    };
    let key: [u8; 32] = std::fs::read(&files.key_file)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| unavailable("the device key", &files.key_file))?;
    let cert: DeviceCert = std::fs::read(&files.cert_file)
        .ok()
        .and_then(|bytes| tilo_user_identity::canonical::from_canonical_bytes(&bytes).ok())
        .ok_or_else(|| unavailable("the device certificate", &files.cert_file))?;
    let key = ed25519_dalek::SigningKey::from_bytes(&key);
    if key.verifying_key().to_bytes() != cert.device_public_key {
        return Err(DirectoryError::Unavailable(
            "the device key is not the certified one".into(),
        ));
    }
    Ok(Device { cert, key })
}

/// Ask `host` its own operation `action` with `argument`, as `device`.
async fn ask(
    device: &Device,
    host: &HostEntry,
    action: &str,
    argument: Map<String, Value>,
) -> Result<Value, DirectoryError> {
    let unavailable = |error: &dyn std::fmt::Display| {
        DirectoryError::Unavailable(format!("{} did not answer: {error}", host.name))
    };
    let host_device: [u8; 32] = hex::decode(&host.device)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| unavailable(&"its device key is malformed"))?;
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&device.key.to_bytes()))
        .bind()
        .await
        .map_err(|error| unavailable(&error))?;
    let asked = async {
        let client = RelayClient::connect_v2_to_own_device(
            endpoint.clone(),
            host.addr.clone(),
            device.cert.user_public_key,
            host_device,
            device.cert.clone(),
            device.key.clone(),
            RevocationList::new(),
        )
        .await
        .map_err(|error| unavailable(&error))?;
        let invoke = InvokeRequest {
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: HOST_ACTIONS_PROVIDER.into(),
                address: ActionAddress::new(HOST_ROOMS_RESOURCE.to_owned(), action),
            },
            argument: InvokeArgument::Json(argument),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        };
        ask_host(&client, invoke)
            .await
            .map_err(|error| unavailable(&error))
    };
    let status = tokio::time::timeout(HOST_TIMEOUT, asked)
        .await
        .map_err(|_| unavailable(&"it took too long"))?;
    endpoint.close().await;
    match status? {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
        ReceiptStatus::Failed { error } => Err(DirectoryError::Refused(error.message)),
        ReceiptStatus::Rejected { error } => Err(DirectoryError::Refused(format!("{error:?}"))),
        other => Err(DirectoryError::Refused(format!("{other:?}"))),
    }
}

fn workspace(listing: WorkspaceListing, host: &HostEntry) -> Workspace {
    Workspace {
        id: listing.workspace,
        name: listing.name,
        host: host.name.clone(),
    }
}

impl Hosts {
    async fn listed(&self, person: &str) -> Result<Vec<(Workspace, HostEntry)>, DirectoryError> {
        let entry = self.person(person)?;
        let device = device(&entry.device)?;
        let mut found = Vec::new();
        for host in &entry.hosts {
            let listed = ask(&device, host, LIST_ROOMS, Map::new()).await?;
            let listings: Vec<WorkspaceListing> =
                serde_json::from_value(listed).map_err(|error| {
                    DirectoryError::Unavailable(format!(
                        "{} answered a malformed listing: {error}",
                        host.name
                    ))
                })?;
            found.extend(
                listings
                    .into_iter()
                    .map(|listing| (workspace(listing, host), host.clone())),
            );
        }
        Ok(found)
    }
}

impl Directory for Hosts {
    fn workspaces<'a>(
        &'a self,
        person: &'a str,
    ) -> BoxFuture<'a, Result<Vec<Workspace>, DirectoryError>> {
        Box::pin(async move {
            match self.listed(person).await {
                Ok(listed) => Ok(listed.into_iter().map(|(workspace, _)| workspace).collect()),
                // A person the door serves no host for has no workspaces yet.
                Err(DirectoryError::NotFound) => Ok(Vec::new()),
                Err(error) => Err(error),
            }
        })
    }

    fn hosts<'a>(&'a self, person: &'a str) -> BoxFuture<'a, Result<Vec<Host>, DirectoryError>> {
        Box::pin(async move {
            let entry = match self.person(person) {
                Ok(entry) => entry,
                Err(DirectoryError::NotFound) => return Ok(Vec::new()),
                Err(error) => return Err(error),
            };
            Ok(entry
                .hosts
                .iter()
                .map(|host| Host {
                    name: host.name.clone(),
                    default: host.default,
                })
                .collect())
        })
    }

    fn create<'a>(
        &'a self,
        person: &'a str,
        name: &'a str,
        host: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Workspace, DirectoryError>> {
        Box::pin(async move {
            let entry = self.person(person).map_err(|error| match error {
                DirectoryError::NotFound => {
                    DirectoryError::Refused("you have no host to create a workspace on yet".into())
                }
                other => other,
            })?;
            let chosen = match host {
                Some(name) => entry.hosts.iter().find(|candidate| candidate.name == name),
                None => entry
                    .hosts
                    .iter()
                    .find(|candidate| candidate.default)
                    .or_else(|| entry.hosts.first()),
            }
            .ok_or_else(|| DirectoryError::Refused("no such host of yours".into()))?;
            let device = device(&entry.device)?;
            let mut argument = Map::new();
            argument.insert("name".into(), json!(name));
            let created = ask(&device, chosen, CREATE_ROOM, argument).await?;
            let created: CreatedWorkspace = serde_json::from_value(created).map_err(|error| {
                DirectoryError::Unavailable(format!(
                    "{} answered a malformed workspace: {error}",
                    chosen.name
                ))
            })?;
            Ok(workspace(created.workspace, chosen))
        })
    }

    fn revocations<'a>(
        &'a self,
        person: &'a [u8; 32],
    ) -> BoxFuture<'a, Result<RevocationList, DirectoryError>> {
        Box::pin(async move {
            let path = match self.person(&hex::encode(person)) {
                Ok(entry) => entry.revocations_file,
                Err(DirectoryError::NotFound) => None,
                Err(error) => return Err(error),
            };
            let Some(path) = path else {
                return Ok(RevocationList::new());
            };
            let bytes = std::fs::read(&path).map_err(|error| {
                DirectoryError::Unavailable(format!("{}: {error}", path.display()))
            })?;
            let entries: Vec<tilo_user_identity::RevocationEntry> =
                tilo_user_identity::canonical::from_canonical_bytes(&bytes).map_err(|error| {
                    DirectoryError::Unavailable(format!("{}: {error}", path.display()))
                })?;
            let mut list = RevocationList::new();
            for entry in entries
                .into_iter()
                .filter(|entry| &entry.user_public_key == person)
            {
                // An entry the person's root did not sign is not theirs.
                let _ = list.insert(entry);
            }
            Ok(list)
        })
    }

    fn route<'a>(
        &'a self,
        person: &'a str,
        workspace: &'a str,
    ) -> BoxFuture<'a, Result<Route, DirectoryError>> {
        Box::pin(async move {
            let listed = self.listed(person).await?;
            let (_, host) = listed
                .into_iter()
                .find(|(candidate, _)| candidate.id == workspace)
                .ok_or(DirectoryError::NotFound)?;
            let token = std::fs::read_to_string(&host.agents_token_file)
                .map(|token| token.trim().to_owned())
                .ok()
                .filter(|token| !token.is_empty())
                .ok_or_else(|| {
                    DirectoryError::Unavailable(format!("{} has no agent token here", host.name))
                })?;
            Ok(Route {
                url: host.agents.replace(WORKSPACE_PLACEHOLDER, workspace),
                token,
            })
        })
    }
}

#[cfg(test)]
#[path = "hosts_tests.rs"]
mod tests;
