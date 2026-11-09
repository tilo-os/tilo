//! The directory as the hosts themselves answer it.
//!
//! A person's workspaces are the ones their hosts govern, and a host lists
//! and creates them as its own operations (`room.list`, `room.create` of
//! provider `tilo.host`), which it offers only to its own person and may
//! refuse. The door asks them as the grant it acts for: it dials the host
//! with the grant's key and shows the certificate the person's root gave
//! it, and the host decides what an agent's grant of its person may do
//! (make workspaces when it reaches all of them, see only the ones it
//! reaches) and checks the person's revocations at the handshake.
//!
//! Which hosts a person has, and where each serves its runtime's agent
//! endpoint ([`crate::endpoint`]: one per runtime, every workspace behind
//! it, the token that proves the caller is the door), come from the
//! [`Registry`] the hosting side keeps. It names places, not authority: the
//! door holds no key of anyone in it. The door reads it again whenever it
//! changes, so a person's new host needs no restart.

use crate::directory::{BoxFuture, Directory, DirectoryError, Host, Route, Workspace};
use crate::grant_key::GrantKey;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, SystemTime},
};
use tilo_actions::remote::{InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_relay_session::RelayClient;
use tilo_relay_session::workspaces::{
    CREATE_ROOM, CreatedWorkspace, HOST_ACTIONS_PROVIDER, HOST_ROOMS_RESOURCE, LIST_REVOCATIONS,
    LIST_ROOMS, WorkspaceListing, ask_host,
};
use tilo_user_identity::{RevocationEntry, RevocationList};

/// How long a host has to answer.
const HOST_TIMEOUT: Duration = Duration::from_secs(30);

/// The hosting side's record of whom the door serves.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    /// By the person's identity root key, hex.
    pub people: HashMap<String, PersonEntry>,
}

/// One person the door serves: where their hosts are.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonEntry {
    pub hosts: Vec<HostEntry>,
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
    /// Where the person's runtime beside it serves its agent endpoint,
    /// `http://<internal address>` ([`crate::endpoint`]).
    pub agents: String,
    /// The file holding the bearer token that endpoint takes from the door.
    pub agents_token_file: PathBuf,
}

/// The registry at a path, read again when the file changes.
pub struct Hosts {
    path: PathBuf,
    cached: Mutex<Option<(SystemTime, Registry)>>,
    http: tilo_net::Client,
}

impl Hosts {
    /// # Errors
    /// The HTTP client for the runtimes could not be built.
    pub fn new(path: PathBuf) -> std::io::Result<Self> {
        let http = tilo_net::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .redirects(tilo_net::Redirects::None)
            .build()
            .map_err(std::io::Error::other)?;
        Ok(Self {
            path,
            cached: Mutex::new(None),
            http,
        })
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

/// Ask `host` its own operation `action` with `argument`, as `grant`.
async fn ask(
    grant: &GrantKey,
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
        .secret_key(iroh::SecretKey::from_bytes(&grant.secret()))
        .bind()
        .await
        .map_err(|error| unavailable(&error))?;
    let asked = async {
        let client = RelayClient::connect_v2_to_own_device(
            endpoint.clone(),
            host.addr.clone(),
            grant.cert().user_public_key,
            host_device,
            grant.cert().clone(),
            grant.key().clone(),
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
    fn token(host: &HostEntry) -> Result<String, DirectoryError> {
        std::fs::read_to_string(&host.agents_token_file)
            .map(|token| token.trim().to_owned())
            .ok()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| {
                DirectoryError::Unavailable(format!("{} has no agent token here", host.name))
            })
    }

    fn route_of(host: &HostEntry) -> Result<Route, DirectoryError> {
        Ok(Route {
            base: host.agents.trim_end_matches('/').to_owned(),
            token: Self::token(host)?,
        })
    }

    /// The workspaces `grant` reaches, each with the host governing it.
    async fn listed(
        &self,
        grant: &GrantKey,
    ) -> Result<Vec<(Workspace, HostEntry)>, DirectoryError> {
        let entry = self.person(&grant.person())?;
        let mut found = Vec::new();
        for host in &entry.hosts {
            let listed = ask(grant, host, LIST_ROOMS, Map::new()).await?;
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
                    // The host shows a grant only what it reaches; so does the door.
                    .filter(|listing| grant.scope().reaches(&listing.workspace))
                    .map(|listing| (workspace(listing, host), host.clone())),
            );
        }
        Ok(found)
    }
}

impl Directory for Hosts {
    fn workspaces<'a>(
        &'a self,
        grant: &'a GrantKey,
    ) -> BoxFuture<'a, Result<Vec<Workspace>, DirectoryError>> {
        Box::pin(async move {
            match self.listed(grant).await {
                Ok(listed) => Ok(listed.into_iter().map(|(workspace, _)| workspace).collect()),
                // A person the door serves no host for has no workspaces yet.
                Err(DirectoryError::NotFound) => Ok(Vec::new()),
                Err(error) => Err(error),
            }
        })
    }

    fn hosts<'a>(
        &'a self,
        grant: &'a GrantKey,
    ) -> BoxFuture<'a, Result<Vec<Host>, DirectoryError>> {
        Box::pin(async move {
            let entry = match self.person(&grant.person()) {
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
        grant: &'a GrantKey,
        name: &'a str,
        host: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Workspace, DirectoryError>> {
        Box::pin(async move {
            let entry = self.person(&grant.person()).map_err(|error| match error {
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
            let route = Self::route_of(chosen)?;
            // The workspace's first invitation is for the person themselves:
            // their runtime joins with it, so the agent acts there next.
            let mut argument = Map::new();
            argument.insert("name".into(), json!(name));
            argument.insert("invite_role".into(), json!("editor"));
            argument.insert("invite_recipient".into(), json!(grant.person()));
            let created = ask(grant, chosen, CREATE_ROOM, argument).await?;
            let created: CreatedWorkspace = serde_json::from_value(created).map_err(|error| {
                DirectoryError::Unavailable(format!(
                    "{} answered a malformed workspace: {error}",
                    chosen.name
                ))
            })?;
            let invitation = created.invitation.ok_or_else(|| {
                DirectoryError::Unavailable(format!(
                    "{} made the workspace but issued no invitation for your runtime",
                    chosen.name
                ))
            })?;
            let made = workspace(created.workspace, chosen);
            crate::endpoint::join(&self.http, &route, grant, &made.id, &invitation.invitation)
                .await
                .map_err(DirectoryError::Unavailable)?;
            Ok(made)
        })
    }

    fn revocations<'a>(
        &'a self,
        grant: &'a GrantKey,
    ) -> BoxFuture<'a, Result<RevocationList, DirectoryError>> {
        Box::pin(async move {
            let person = grant.cert().user_public_key;
            let entry = match self.person(&grant.person()) {
                Ok(entry) => entry,
                // Nobody hosts the person here: no host enforces a list for them.
                Err(DirectoryError::NotFound) => return Ok(RevocationList::new()),
                Err(error) => return Err(error),
            };
            let mut list = RevocationList::new();
            for host in &entry.hosts {
                let answered = ask(grant, host, LIST_REVOCATIONS, Map::new()).await?;
                let entries: Vec<RevocationEntry> =
                    serde_json::from_value(answered).map_err(|error| {
                        DirectoryError::Unavailable(format!(
                            "{} answered malformed revocations: {error}",
                            host.name
                        ))
                    })?;
                for entry in entries
                    .into_iter()
                    .filter(|entry| entry.user_public_key == person)
                {
                    // An entry the person's root did not sign is not theirs.
                    let _ = list.insert(entry);
                }
            }
            Ok(list)
        })
    }

    fn route<'a>(
        &'a self,
        grant: &'a GrantKey,
        workspace: &'a str,
    ) -> BoxFuture<'a, Result<Route, DirectoryError>> {
        Box::pin(async move {
            if !grant.scope().reaches(workspace) {
                return Err(DirectoryError::NotFound);
            }
            let listed = self.listed(grant).await?;
            let (_, host) = listed
                .into_iter()
                .find(|(candidate, _)| candidate.id == workspace)
                .ok_or(DirectoryError::NotFound)?;
            Self::route_of(&host)
        })
    }
}

#[cfg(test)]
#[path = "hosts_tests.rs"]
mod tests;
