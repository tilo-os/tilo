//! The servers of a person, configured in their admin workspace.
//!
//! The admin workspace is an ordinary workspace whose only member is the
//! person. Each server is one of their devices and takes part in it. Its
//! [`SERVERS`] record says, per server, what that server should be: its name,
//! which workspaces it holds and how (an encrypted store or a key holder that
//! takes work), how much it offers (storage, compute, machines), which updates
//! it follows and when it may restart for maintenance, how it unlocks
//! after a boot, whether it was retired, and the machine itself: its hostname,
//! the drives it uses for what and how its network interfaces are set up
//! ([`device`]). Every change is a typed action on
//! that record ([`Servers`]), so it has history and an author, and the device
//! admin route, an agent or a script change it the same way. A server applies
//! what its entry says; nothing about it lives in a file of its own.
//!
//! The workspace's id follows from the person's root ([`admin_workspace_id`]),
//! so each of their servers finds the same one without being told. It is
//! made once, by the person's first server ([`found`]): a server enrolled when
//! the device approving it knew of no earlier server of the person. Every
//! later server knows the earlier ones from its enrollment and asks one of
//! them to take it in ([`admit`]), which enters it in the document; the holders
//! then hand it the workspace as they hand a server any workspace its entry
//! names. A desktop never makes it. Once it holds the workspace, a server
//! keeps its own entry's address current ([`enroll`]).
//!
//! A device of the person that holds a workspace attaches the servers its
//! document names as holders, and only those: one action on the document
//! chooses the holder and names it in the workspace
//! ([`applied::reconcile`]). Each server applies its own entry's offer to what
//! it answers devices, its update policy to what it follows, and a retired
//! server lets go of everything it holds.

use tilo_resource::{Collection, Id, Invoke, Model, Refusal, Replica, resource};
use tilo_user_identity::DeviceCert;

use crate::workspaces::Workspaces;

pub mod applied;
pub mod device;
pub mod policy;

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
/// The updates a server follows: the releases (`stable`), or the images built
/// from the development branch (`dev`).
pub const UPDATES: [&str; 2] = ["stable", "dev"];
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
    /// Where it is dialled, as its own endpoint advertises it (JSON), kept so
    /// a device of its person can hand it a workspace. Empty until it
    /// enrolled one.
    pub endpoint: String,
    /// The workspaces it holds, by workspace id.
    pub holds: Collection<Holding>,
    /// Gigabytes of storage it offers its workspaces.
    pub storage_gb: u64,
    /// Whether it runs its workspaces' actions and jobs.
    pub compute: bool,
    /// How many Linux machines it runs for its workspaces at once.
    pub machines: u32,
    /// The updates it follows ([`UPDATES`]); empty for the ones its image
    /// came with.
    pub updates: String,
    /// When its daily maintenance window starts, in minutes after midnight UTC.
    pub maintenance_start: u32,
    /// How long the window lasts in minutes; 0 for no window.
    pub maintenance_minutes: u32,
    /// How it unlocks after a boot ([`UNLOCK_POLICIES`]); empty for its image's
    /// default.
    pub unlock: String,
    /// Retired: the person took it out of service, and it serves nothing.
    pub retired: bool,
    /// The public half (PEM) of the key its TPM's rollback floor takes
    /// approvals from, as the server says; empty until it said.
    pub floor_key: String,
    /// The machine itself: its hostname, the drives it uses and how its
    /// network interfaces are set up ([`device::DeviceConfig`]).
    pub device: device::DeviceConfig,
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
    /// holds and offers, the updates it follows, its maintenance window and how
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
    /// Take a server into the person's servers, named `label` and dialled at
    /// `endpoint` (its own address, JSON). A server already there keeps its
    /// configuration and takes its new address, which changes as it is
    /// reached.
    #[action(external)]
    pub fn enroll_server(
        &mut self,
        device: String,
        label: String,
        endpoint: String,
    ) -> Result<(), Refusal> {
        let device = Id::from_raw(device);
        validate_device(&device)?;
        let label = validate_label(&label)?;
        let endpoint = validate_endpoint(&endpoint)?;
        match self.servers.get_mut(&device) {
            Some(server) => server.endpoint = endpoint,
            None => {
                self.servers.push(
                    device,
                    Server {
                        label,
                        endpoint,
                        ..Server::default()
                    },
                );
            }
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

    /// Set the updates a server follows.
    #[action(external)]
    pub fn set_updates(&mut self, device: Id, updates: String) -> Result<(), Refusal> {
        if !UPDATES.contains(&updates.as_str()) {
            return Err(Refusal::new("unknown updates a server follows"));
        }
        self.server(&device)?.updates = updates;
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

    /// Say which key (public half, PEM) a server's rollback floor takes
    /// approvals from.
    #[action(external)]
    pub fn set_floor_key(&mut self, device: Id, key: String) -> Result<(), Refusal> {
        self.server(&device)?.floor_key = key;
        Ok(())
    }

    /// Set the machine's hostname.
    #[action(external)]
    pub fn set_hostname(&mut self, device: Id, hostname: String) -> Result<(), Refusal> {
        let hostname = device::valid_hostname(&hostname)?;
        self.server(&device)?.device.hostname = hostname;
        Ok(())
    }

    /// Use the drive `identity` (its serial or WWN) as `role`
    /// ([`device::DISK_ROLES`]).
    ///
    /// `confirm` names the drive again to confirm erasing what it holds: a
    /// drive used for something else already is erased to become this one, so
    /// changing its role needs it. A machine has one system disk, and the disk
    /// boots from is never repurposed.
    #[action(external)]
    pub fn use_disk(
        &mut self,
        device: Id,
        identity: String,
        role: String,
        confirm: String,
    ) -> Result<(), Refusal> {
        let identity = device::valid_identity(&identity)?;
        device::valid_role(&role)?;
        let confirm = confirm.trim().to_owned();
        if !confirm.is_empty() && confirm != identity {
            return Err(Refusal::new(
                "confirming a drive is naming its own serial or WWN",
            ));
        }
        let erase = confirm == identity;
        let disk = Id::from_raw(identity);
        let server = self.server(&device)?;
        if let Some(known) = server.device.disks.get(&disk)
            && known.role != role
        {
            if known.role == device::SYSTEM {
                return Err(Refusal::new(
                    "a machine does not repurpose the disk it boots from",
                ));
            }
            if !erase {
                return Err(Refusal::new(
                    "another role erases the drive; confirm it by naming its serial or WWN",
                ));
            }
        }
        if role == device::SYSTEM
            && server
                .device
                .disks
                .iter()
                .any(|(known, held)| held.role == device::SYSTEM && known != &disk)
        {
            return Err(Refusal::new(
                "a machine has one system disk; release the other first",
            ));
        }
        match server.device.disks.get_mut(&disk) {
            Some(held) => {
                held.role = role;
                held.erase = erase;
            }
            None => {
                server.device.disks.push(disk, device::Disk { role, erase });
            }
        }
        Ok(())
    }

    /// Stop using the drive `identity`; the disk boots from stays.
    #[action(external)]
    pub fn release_disk(&mut self, device: Id, identity: String) -> Result<(), Refusal> {
        let disk = Id::from_raw(identity);
        let server = self.server(&device)?;
        if server
            .device
            .disks
            .get(&disk)
            .is_some_and(|held| held.role == device::SYSTEM)
        {
            return Err(Refusal::new("a machine keeps the disk it boots from"));
        }
        server.device.disks.remove(&disk);
        Ok(())
    }

    /// Set up the interface `name`: addressed by the `addresses` it names
    /// (`address/prefix`, comma separated), or by DHCP when it names none;
    /// reached through `gateway`; using `dns` servers (comma separated); a
    /// member of `bond` (empty for none; a member takes its addressing from the
    /// bond). A name ending in `.<vlan>` tags the interface with it.
    #[action(external)]
    pub fn set_interface(
        &mut self,
        device: Id,
        name: String,
        addressing: String,
        addresses: String,
        gateway: String,
        dns: String,
        bond: String,
    ) -> Result<(), Refusal> {
        let name = name.trim().to_owned();
        let interface =
            device::valid_interface(&name, &addressing, &addresses, &gateway, &dns, &bond)?;
        let interface_id = Id::from_raw(name);
        let server = self.server(&device)?;
        if !interface.bond.is_empty()
            && server
                .device
                .network
                .get(&Id::from_raw(interface.bond.clone()))
                .is_none()
        {
            return Err(Refusal::new(
                "a bond member names the interface its bond is set up on",
            ));
        }
        if !interface.bond.is_empty()
            && server
                .device
                .network
                .iter()
                .any(|(known, held)| known != &interface_id && held.bond == interface_id.as_str())
        {
            return Err(Refusal::new(
                "an interface others are bonded to is no member",
            ));
        }
        match server.device.network.get_mut(&interface_id) {
            Some(held) => *held = interface,
            None => {
                server.device.network.push(interface_id, interface);
            }
        }
        Ok(())
    }

    /// Stop configuring the interface `name`; the interfaces bonded to it stay
    /// until they are let go.
    #[action(external)]
    pub fn release_interface(&mut self, device: Id, name: String) -> Result<(), Refusal> {
        let name = name.trim().to_owned();
        let interface = Id::from_raw(name);
        let server = self.server(&device)?;
        if server
            .device
            .network
            .iter()
            .any(|(known, held)| known != &interface && held.bond == interface.as_str())
        {
            return Err(Refusal::new(
                "interfaces are bonded to it; let them go first",
            ));
        }
        server.device.network.remove(&interface);
        Ok(())
    }

    /// Say what a server's machine reported applying its configuration: the
    /// `revision` of the request, an [`REPORT_STATES`](device::REPORT_STATES)
    /// state, and what it applied or why not.
    #[action(external)]
    pub fn report_device_config(
        &mut self,
        device: Id,
        revision: String,
        state: String,
        detail: String,
    ) -> Result<(), Refusal> {
        let status = device::valid_status(&revision, &state, &detail)?;
        self.server(&device)?.device.status = status;
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

/// `endpoint` as a server's address, spelled as its own endpoint advertises
/// it: parsed here so nothing later reads a dial address out of arbitrary
/// text.
fn validate_endpoint(endpoint: &str) -> Result<String, Refusal> {
    let wrong = || Refusal::new("a server's endpoint is its own dial address");
    let address = serde_json::from_str::<iroh_base::EndpointAddr>(endpoint).map_err(|_| wrong())?;
    serde_json::to_string(&address).map_err(|_| wrong())
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

/// Whether this host holds its person's admin workspace: it made it, or the
/// workspace names it among its holders, so it writes the document as its own.
pub async fn holds_admin(workspaces: &Workspaces) -> bool {
    let own = workspaces.serving_certificate();
    let Some(state) = workspaces
        .state(&admin_workspace_id(&own.user_public_key))
        .await
    else {
        return false;
    };
    let governance = state.governance();
    governance.founder() == Some(own.device_public_key)
        || governance
            .keeping_devices()
            .iter()
            .any(|hint| hint.host.device_public_key == own.device_public_key)
}

/// Make the admin workspace of this host's person, as their first server, and
/// enter this host's device in it, dialled at `endpoint`. A host that holds
/// the workspace already only enters itself.
///
/// # Errors
/// The workspace could not be made, or the entry written.
pub async fn found(
    workspaces: &Workspaces,
    endpoint: &iroh_base::EndpointAddr,
) -> Result<String, String> {
    let admin = admin_workspace_id(&workspaces.serving_certificate().user_public_key);
    if !holds_admin(workspaces).await {
        workspaces
            .create_workspace(ADMIN_WORKSPACE_NAME, Some(&admin))
            .await?;
    }
    let cert = workspaces.serving_certificate().clone();
    enter(workspaces, &cert, endpoint).await?;
    Ok(admin)
}

/// Apply `action` to the admin workspace's [`SERVERS`] record as this host,
/// which governs the workspace.
///
/// # Errors
/// The action was refused, or the record could not be written.
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

/// The name the admin workspace gives the server `device`: what the
/// workspaces it holds show it by. `None` where its entry names it nowhere,
/// and the keeper goes by its device's label.
pub async fn name(workspaces: &Workspaces, device: &[u8; 32]) -> Option<String> {
    read(workspaces)
        .await
        .ok()?
        .active(device)
        .map(|server| server.label.clone())
        .filter(|label| !label.is_empty())
}

/// Keep this host's own entry among its person's servers current: its name
/// and `endpoint`, what a server that holds the admin workspace does on each
/// start, as the address it is reached at may have changed.
///
/// # Errors
/// This host holds no admin workspace, or the entry could not be written.
pub async fn enroll(
    workspaces: &Workspaces,
    endpoint: &iroh_base::EndpointAddr,
) -> Result<(), String> {
    if !holds_admin(workspaces).await {
        return Err("this server holds no admin workspace yet".into());
    }
    let cert = workspaces.serving_certificate().clone();
    enter(workspaces, &cert, endpoint).await
}

/// Take the server `server`, dialled at `endpoint`, in among this host's
/// person's servers, as it asks when it starts without the admin workspace:
/// it is entered in the document, and the workspace's holders then hand it the
/// workspace as they hand any server the workspaces its entry names
/// ([`applied::reconcile`]). A retired server stays retired.
///
/// # Errors
/// `server` is not a device of this host's person that may hold workspaces,
/// this host holds no admin workspace, or the entry could not be written.
pub async fn admit(
    workspaces: &Workspaces,
    server: &DeviceCert,
    endpoint: &iroh_base::EndpointAddr,
) -> Result<(), String> {
    if server.user_public_key != workspaces.serving_certificate().user_public_key
        || !server.capabilities.can_own_workspaces
    {
        return Err("only a server of this person is taken in among their servers".into());
    }
    if !holds_admin(workspaces).await {
        return Err("this server holds no admin workspace yet".into());
    }
    enter(workspaces, server, endpoint).await
}

/// Enter the device `cert` among the person's servers at `endpoint`; an entry
/// that says so already writes no history.
async fn enter(
    workspaces: &Workspaces,
    cert: &DeviceCert,
    endpoint: &iroh_base::EndpointAddr,
) -> Result<(), String> {
    let endpoint = serde_json::to_string(endpoint).map_err(|error| error.to_string())?;
    if read(workspaces)
        .await?
        .servers
        .get(&device_id(&cert.device_public_key))
        .is_some_and(|server| server.endpoint == endpoint)
    {
        return Ok(());
    }
    let action = Servers::ENROLL_SERVER.with((
        hex::encode(cert.device_public_key),
        cert.device_label.clone(),
        endpoint,
    ));
    apply(workspaces, action).await
}

pub mod floor;

#[cfg(test)]
mod tests;
