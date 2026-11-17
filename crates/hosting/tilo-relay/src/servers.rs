//! The servers of a person, configured in their admin workspace.
//!
//! The admin workspace is an ordinary workspace whose only member is the
//! person. Each server is one of their devices and takes part in it. Its
//! [`SERVERS`] record says, per server, what that server should be: its name,
//! which workspaces it holds and how (an encrypted store or a key holder that
//! takes work), how much it offers (storage, compute, machines), which update
//! channel it follows and when it may restart for maintenance, how it unlocks
//! after a boot, and whether it was retired. Every change is a typed action on
//! that record ([`Servers`]), so it has history and an author, and the device
//! admin route, an agent or a script change it the same way. A server applies
//! what its entry says; nothing about it lives in a file of its own.
//!
//! The workspace's id follows from the person's root ([`admin_workspace_id`]),
//! so each of their servers finds the same one without being told. A server
//! that does not hold it yet makes it ([`ensure_admin_workspace`]), and a
//! server enrolled into the person's identity enters itself on its first start
//! ([`enroll`]).

#[cfg(feature = "agents")]
use tilo_resource::Invoke;
use tilo_resource::{Collection, Id, Model, Refusal, Replica, resource};
#[cfg(feature = "agents")]
use tilo_user_identity::DeviceCert;

use crate::workspaces::Workspaces;

/// The admin workspace's record of the person's servers.
pub const SERVERS: (&str, &str) = ("so.tilo.servers", "servers");
/// The name the admin workspace is made with.
const ADMIN_WORKSPACE_NAME: &str = "Servers";
/// What a server holds a workspace as: an encrypted store that keeps and
/// forwards what it cannot read.
pub const STORE: &str = "store";
/// What a server holds a workspace as: a key holder, which holds the keys and
/// takes the workspace's work.
pub const KEY_HOLDER: &str = "key-holder";
/// The ways a server unlocks after a boot: an owner's device approves it, its
/// TPM unseals its keys, or someone types a passphrase at it.
pub const UNLOCK_POLICIES: [&str; 3] = ["owner-device", "tpm", "passphrase"];
/// The update channels a server follows.
pub const UPDATE_CHANNELS: [&str; 2] = ["stable", "main"];
const MINUTES_PER_DAY: u32 = 24 * 60;
const MAX_LABEL: usize = 80;

/// The person's servers, by device key (lowercase hex).
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Servers {
    pub servers: Collection<Server>,
}

/// One server's desired configuration.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Server {
    /// The name people see for it.
    pub label: String,
    /// The workspaces it holds, by workspace id.
    pub holds: Collection<Holding>,
    /// Gigabytes of storage it offers its workspaces.
    pub storage_gb: u64,
    /// Whether it runs its workspaces' actions and jobs.
    pub compute: bool,
    /// How many Linux machines it runs for its workspaces at once.
    pub machines: u32,
    /// The update channel it follows ([`UPDATE_CHANNELS`]); empty for the one
    /// its image came with.
    pub update_channel: String,
    /// When its daily maintenance window starts, in minutes after midnight UTC.
    pub maintenance_start: u32,
    /// How long the window lasts in minutes; 0 for no window.
    pub maintenance_minutes: u32,
    /// How it unlocks after a boot ([`UNLOCK_POLICIES`]); empty for its image's
    /// default.
    pub unlock: String,
    /// Retired: the person took it out of service, and it serves nothing.
    pub retired: bool,
}

/// How a server holds one workspace.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Holding {
    /// [`STORE`] or [`KEY_HOLDER`].
    pub mode: String,
}

impl Servers {
    /// What the record holds, read through
    /// [`fields`](tilo_resource::read::fields): each server's name, what it
    /// holds and offers, its update channel, its maintenance window and how
    /// it unlocks. The person's agent or a script reads what their servers
    /// are set to as it reads any record.
    ///
    /// # Errors
    /// A named item is not this read's to answer.
    pub fn read_document(&self, item: Option<&Id>) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }

    /// The entry of the server whose device key is `device`, unless it was
    /// retired.
    #[must_use]
    pub fn active(&self, device: &[u8; 32]) -> Option<&Server> {
        self.servers
            .get(&device_id(device))
            .filter(|server| !server.retired)
    }

    fn server(&mut self, device: &Id) -> Result<&mut Server, Refusal> {
        self.servers
            .get_mut(device)
            .filter(|server| !server.retired)
            .ok_or_else(|| Refusal::new("no such server"))
    }
}

impl Server {
    /// Whether it holds `workspace`, and how.
    #[must_use]
    pub fn holding(&self, workspace: &str) -> Option<&str> {
        self.holds
            .get(&Id::from_raw(workspace))
            .map(|holding| holding.mode.as_str())
    }
}

#[resource(kind = "tilo.servers", read_content = Servers::read_document)]
impl Servers {
    /// Take a server into the person's servers, named `label`. A server
    /// already there keeps its configuration.
    #[action(external)]
    pub fn enroll_server(&mut self, device: String, label: String) -> Result<(), Refusal> {
        let device = Id::from_raw(device);
        validate_device(&device)?;
        let label = validate_label(&label)?;
        if self.servers.get(&device).is_none() {
            self.servers.push(
                device,
                Server {
                    label,
                    ..Server::default()
                },
            );
        }
        Ok(())
    }

    /// Rename a server.
    #[action(external)]
    pub fn rename_server(&mut self, device: Id, label: String) -> Result<(), Refusal> {
        let label = validate_label(&label)?;
        self.server(&device)?.label = label;
        Ok(())
    }

    /// Have a server hold a workspace as `mode` ([`STORE`] or [`KEY_HOLDER`]).
    #[action(external)]
    pub fn hold_workspace(
        &mut self,
        device: Id,
        workspace: String,
        mode: String,
    ) -> Result<(), Refusal> {
        if uuid::Uuid::parse_str(&workspace).is_err() {
            return Err(Refusal::new("not a workspace id"));
        }
        if mode != STORE && mode != KEY_HOLDER {
            return Err(Refusal::new(
                "a server holds a workspace as store or key holder",
            ));
        }
        let server = self.server(&device)?;
        let workspace = Id::from_raw(workspace);
        match server.holds.get_mut(&workspace) {
            Some(holding) => holding.mode = mode,
            None => {
                server.holds.push(workspace, Holding { mode });
            }
        }
        Ok(())
    }

    /// Have a server let a workspace go.
    #[action(external)]
    pub fn release_workspace(&mut self, device: Id, workspace: String) -> Result<(), Refusal> {
        self.server(&device)?.holds.remove(&Id::from_raw(workspace));
        Ok(())
    }

    /// Set how much a server offers its workspaces.
    #[action(external)]
    pub fn set_appetite(
        &mut self,
        device: Id,
        storage_gb: u64,
        compute: bool,
        machines: u32,
    ) -> Result<(), Refusal> {
        let server = self.server(&device)?;
        server.storage_gb = storage_gb;
        server.compute = compute;
        server.machines = machines;
        Ok(())
    }

    /// Set the update channel a server follows.
    #[action(external)]
    pub fn set_update_channel(&mut self, device: Id, channel: String) -> Result<(), Refusal> {
        if !UPDATE_CHANNELS.contains(&channel.as_str()) {
            return Err(Refusal::new("unknown update channel"));
        }
        self.server(&device)?.update_channel = channel;
        Ok(())
    }

    /// Set a server's daily maintenance window: `minutes` from `start`
    /// minutes after midnight UTC; 0 minutes for none.
    #[action(external)]
    pub fn set_maintenance_window(
        &mut self,
        device: Id,
        start: u32,
        minutes: u32,
    ) -> Result<(), Refusal> {
        if start >= MINUTES_PER_DAY || minutes > MINUTES_PER_DAY {
            return Err(Refusal::new(
                "a window starts within a day and lasts at most one",
            ));
        }
        let server = self.server(&device)?;
        server.maintenance_start = start;
        server.maintenance_minutes = minutes;
        Ok(())
    }

    /// Set how a server unlocks after a boot.
    #[action(external)]
    pub fn set_unlock_policy(&mut self, device: Id, policy: String) -> Result<(), Refusal> {
        if !UNLOCK_POLICIES.contains(&policy.as_str()) {
            return Err(Refusal::new("unknown unlock policy"));
        }
        self.server(&device)?.unlock = policy;
        Ok(())
    }

    /// Retire a server: it serves nothing from then on. Its entry stays, so
    /// its device is never taken in again as a new server.
    #[action(external)]
    pub fn retire_server(&mut self, device: Id) -> Result<(), Refusal> {
        let server = self.server(&device)?;
        server.retired = true;
        server.holds = Collection::default();
        Ok(())
    }
}

/// A server's id in [`Servers`]: its device key in lowercase hex.
#[must_use]
pub fn device_id(device: &[u8; 32]) -> Id {
    Id::from_raw(hex::encode(device))
}

fn validate_device(device: &Id) -> Result<(), Refusal> {
    let mut key = [0; 32];
    hex::decode_to_slice(device.as_str(), &mut key)
        .ok()
        .filter(|()| device.as_str() == hex::encode(key))
        .ok_or_else(|| Refusal::new("a server is named by its device key"))
}

fn validate_label(label: &str) -> Result<String, Refusal> {
    let label = label.trim();
    if label.is_empty() || label.len() > MAX_LABEL {
        return Err(Refusal::new("a server's name is 1 to 80 bytes"));
    }
    Ok(label.to_owned())
}

/// The id of the admin workspace of the person whose root is `person`: the
/// same on each of their servers, so none has to be told it.
#[must_use]
pub fn admin_workspace_id(person: &[u8; 32]) -> String {
    let hash = blake3::Hasher::new()
        .update(b"tilo admin workspace v1\0")
        .update(person)
        .finalize();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    uuid::Builder::from_random_bytes(bytes)
        .into_uuid()
        .to_string()
}

/// Make the admin workspace of this host's person where this host does not
/// hold it yet, and answer its id.
///
/// # Errors
/// The workspace could not be listed or made.
pub async fn ensure_admin_workspace(workspaces: &Workspaces) -> Result<String, String> {
    let admin = admin_workspace_id(&workspaces.serving_certificate().user_public_key);
    if !workspaces
        .list()
        .await
        .iter()
        .any(|summary| summary.workspace == admin)
    {
        workspaces
            .create_workspace(ADMIN_WORKSPACE_NAME, Some(&admin))
            .await?;
    }
    Ok(admin)
}

/// Apply `action` to the admin workspace's [`SERVERS`] record as this host,
/// which governs the workspace.
///
/// # Errors
/// The action was refused, or the record could not be written.
#[cfg(feature = "agents")]
pub async fn apply(workspaces: &Workspaces, action: Invoke<Servers>) -> Result<(), String> {
    let admin = admin_workspace_id(&workspaces.serving_certificate().user_public_key);
    let cert = workspaces.serving_certificate().clone();
    workspaces
        .author_workspace_document(&admin, &cert, SERVERS, |doc| {
            let mut replica = Replica::<Servers>::from_doc(std::mem::take(doc));
            let applied = replica
                .apply(action)
                .map(drop)
                .map_err(|error| error.to_string());
            std::mem::swap(doc, replica.doc_mut());
            applied
        })
        .await
        .map_err(|error| error.to_string())
}

/// The admin workspace's [`SERVERS`] record as this host reads it.
///
/// # Errors
/// The record could not be read.
pub async fn read(workspaces: &Workspaces) -> Result<Servers, String> {
    let admin = admin_workspace_id(&workspaces.serving_certificate().user_public_key);
    let Some(bytes) = workspaces
        .resource_document(&admin, SERVERS.0, SERVERS.1)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(Servers::default());
    };
    Replica::<Servers>::load(&bytes)
        .and_then(|record| record.state())
        .map_err(|error| error.to_string())
}

/// Enter this host's device into its person's servers, named `label`, making
/// the admin workspace first where this host has none: what a server enrolled
/// into the person's identity does on its first start.
///
/// # Errors
/// The admin workspace could not be made, or the entry written.
#[cfg(feature = "agents")]
pub async fn enroll(workspaces: &Workspaces, cert: &DeviceCert) -> Result<String, String> {
    let admin = ensure_admin_workspace(workspaces).await?;
    let action = Servers::ENROLL_SERVER.with((
        hex::encode(cert.device_public_key),
        cert.device_label.clone(),
    ));
    apply(workspaces, action).await?;
    Ok(admin)
}

#[cfg(test)]
mod tests;
