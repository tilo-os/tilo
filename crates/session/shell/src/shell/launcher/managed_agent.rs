//! Host-only managed actor custody. No actor key crosses the sandbox boundary.
#[cfg(test)]
mod tests;

use super::{InstalledApp, RoomId, WORKSPACE_AGENT_PACKAGE_ID, Workspace, lock};
use crate::shell::native_workspace_owner::NativeWorkspaceOwner;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use tilo_identity_host::native_agent::{
    MAX_NATIVE_AGENT_LIFETIME_NS, ManagedNativeAgent, NativeAgentRequest,
};
use tilo_relay_protocol::delegation::Delegation;
use tilo_user_identity::DeviceCert;

type ManagedActorKey = (String, String);
type ManagedActors = HashMap<ManagedActorKey, Arc<ManagedNativeAgent>>;
type ManagedLaunches = HashMap<ManagedActorKey, Arc<ManagedLaunch>>;

/// An owner is installed by the authenticated session, never by an application.
static OWNERS: LazyLock<Mutex<HashMap<RoomId, NativeWorkspaceOwner>>> =
    LazyLock::new(Mutex::default);
static ACTORS: LazyLock<Mutex<ManagedActors>> = LazyLock::new(Mutex::default);
static LAUNCHES: LazyLock<Mutex<ManagedLaunches>> = LazyLock::new(Mutex::default);

pub(super) fn install_owner(owner: &NativeWorkspaceOwner) {
    let mut owners = lock(&OWNERS);
    for room in owner.rooms() {
        owners.insert(room, owner.clone());
    }
}

pub(super) fn owner(room: RoomId) -> Result<NativeWorkspaceOwner, String> {
    lock(&OWNERS)
        .get(&room)
        .cloned()
        .ok_or_else(|| "managed actor requires the installed native workspace owner".into())
}

/// Unlike creation metadata, this check reads every signed payload entry now.
/// The launcher repeats verification against the independently pinned hash.
/// The image supplies this pin independently of the writable package registry.
/// No publisher or expected hash from an application is a trust input.
const INSTALLED_AGENT_HASH: &str = "/usr/lib/tilo/managed-agent-package-hash";

pub(super) fn verify_installed_package(app: &InstalledApp) -> Result<String, String> {
    let expected = read_installed_hash(std::path::Path::new(INSTALLED_AGENT_HASH), 0)?;
    verify_installed_package_hash(app, &expected)
}

fn read_installed_hash(path: &std::path::Path, owner: u32) -> Result<String, String> {
    use std::io::Read as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    // Reject redirected ancestors as well as a symlink at the pin itself.
    for parent in path.ancestors().skip(1) {
        let meta = std::fs::symlink_metadata(parent).map_err(|error| error.to_string())?;
        if !meta.is_dir() || meta.uid() != owner || meta.mode() & 0o022 != 0 {
            return Err("managed executor pin must have protected host-owned parents".into());
        }
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| format!("managed executor pin unavailable: {error}"))?;
    let meta = file.metadata().map_err(|error| error.to_string())?;
    if !meta.is_file() || meta.uid() != owner || meta.mode() & 0o022 != 0 || meta.len() > 65 {
        return Err("managed executor pin must be a protected host-owned hash".into());
    }
    let mut hash = String::new();
    file.take(66)
        .read_to_string(&mut hash)
        .map_err(|error| error.to_string())?;
    let hash = hash.trim_end_matches('\n');
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("managed executor pin is not a package hash".into());
    }
    Ok(hash.to_ascii_lowercase())
}

fn verify_installed_package_hash(app: &InstalledApp, expected: &str) -> Result<String, String> {
    if app.package_id != WORKSPACE_AGENT_PACKAGE_ID {
        return Err("managed actor package is not the fixed native executor".into());
    }
    let installed = package_store::find_installed(&app.package_id)
        .map_err(|error| error.to_string())?
        .ok_or("managed actor package is not installed")?;
    if installed.package_id_hex != expected {
        return Err("managed executor package does not match the image's trusted hash".into());
    }
    if installed.manifest_path != app.manifest_path || installed.payload_root != app.payload_root {
        return Err("managed actor package differs from the installed copy".into());
    }
    let report = package_store::integrity::verify(&installed, &app.payload_root)
        .map_err(|error| format!("managed actor package verification failed: {error}"))?;
    if !report.is_intact() || !report.unexpected.is_empty() {
        return Err("managed actor package payload differs from its signed collection".into());
    }
    let manifest =
        launcher::manifest::read_manifest(&app.manifest_path).map_err(|error| error.to_string())?;
    if manifest.package_id != WORKSPACE_AGENT_PACKAGE_ID {
        return Err("managed executor signed manifest names another package".into());
    }
    Ok(installed.package_id_hex)
}

pub(super) struct ManagedLaunch {
    actor: Arc<ManagedNativeAgent>,
    delegation: Delegation,
    owner: NativeWorkspaceOwner,
    room: RoomId,
}

impl std::fmt::Debug for ManagedLaunch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedLaunch")
            .field("room", &self.room)
            .finish_non_exhaustive()
    }
}

impl ManagedLaunch {
    pub(super) fn identity(&self) -> Result<(DeviceCert, Delegation, [u8; 64]), String> {
        let proof = self
            .actor
            .join_proof_for(&self.delegation)
            .map_err(|error| error.to_string())?;
        let certificate = self.actor.certificate();
        self.owner.with_current(self.room, |view| {
            let (admitted, delegation, withdrawn) = view
                .peer()
                .governance()
                .governed_agent(&certificate.device_public_key)
                .ok_or("managed actor is no longer admitted")?;
            if &admitted != certificate || delegation != self.delegation || withdrawn.is_some() {
                return Err("managed actor admission changed".into());
            }
            view.peer()
                .permissions(certificate.device_public_key, std::time::SystemTime::now())?;
            Ok(())
        })?;
        Ok((certificate.clone(), self.delegation.clone(), proof))
    }
}

impl tilo_action_broker::services::ManagedApplicationIdentity for ManagedLaunch {
    fn identity(&self) -> Result<(DeviceCert, Delegation, [u8; 64]), String> {
        ManagedLaunch::identity(self)
    }
}

pub(super) fn retained(room: &str, agent: &str) -> Option<Arc<ManagedLaunch>> {
    lock(&LAUNCHES)
        .get(&(room.to_owned(), agent.to_owned()))
        .cloned()
}

pub(super) fn prepare(
    owner: &NativeWorkspaceOwner,
    workspace: &Workspace,
    agent: &str,
) -> Result<Arc<ManagedLaunch>, String> {
    let room = workspace.room;
    let room_name = room.to_string();
    let (key, governor) = tilo_identity_host::broker::trusted_current_device(&room_name)
        .map_err(|error| error.to_string())?;
    // A local replica is not its foreign governor. Do not substitute a local
    // root or pretend a forwarded enrollment was acknowledged.
    owner.with_current(room, |view| {
        if view.governor() != &governor || view.device() != &governor {
            return Err("managed actor admission requires the actual governing owner".into());
        }
        view.peer().workspace().check_workspace_administration(
            governor.device_public_key,
            std::time::SystemTime::now(),
        )
    })?;
    let person = hex::encode(governor.user_public_key);
    let person_room = collab_types::person_room_of(&person)
        .ok_or("managed actor person has no canonical room")?
        .to_string();
    let mut actors = lock(&ACTORS);
    let actor_key = (person, agent.to_owned());
    let actor = if let Some(actor) = actors.get(&actor_key) {
        actor.join_proof().map_err(|error| error.to_string())?;
        actor.clone()
    } else {
        let now_ns = tilo_clock::unix_now_ns();
        let expires_at_ns = governor
            .expires_at_ns
            .unwrap_or(u64::MAX)
            .min(now_ns.saturating_add(MAX_NATIVE_AGENT_LIFETIME_NS));
        let mut apps = workspace.launch_grants(tilo_clock::unix_now()).authors;
        apps.push(WORKSPACE_AGENT_PACKAGE_ID.into());
        apps.sort();
        apps.dedup();
        let actor = Arc::new(
            // A process restart requests at most a day, but must use the
            // enrolled certificate's remaining lifetime, never renew it.
            tilo_identity_host::native_agent::provision_retained(
                &person_room,
                &NativeAgentRequest {
                    agent_id: agent.to_owned(),
                    label: agent.to_owned(),
                    // Stable actor colour derived from the selected person's public root.
                    color: [
                        governor.user_public_key[0],
                        governor.user_public_key[1],
                        governor.user_public_key[2],
                    ],
                    apps,
                    expires_at_ns,
                },
            )
            .map_err(|error| error.to_string())?,
        );
        actors.insert(actor_key, actor.clone());
        actor
    };
    drop(actors);
    let delegation = actor
        .delegate_room(&room_name)
        .map_err(|error| error.to_string())?;
    actor
        .join_proof_for(&delegation)
        .map_err(|error| error.to_string())?;
    owner.admit_managed_agent(
        room,
        actor.certificate().clone(),
        delegation.clone(),
        &governor,
        &key,
        std::time::SystemTime::now(),
    )?;
    let launch = Arc::new(ManagedLaunch {
        actor,
        delegation,
        owner: owner.clone(),
        room,
    });
    launch.identity()?;
    lock(&LAUNCHES).insert((room_name, agent.to_owned()), launch.clone());
    Ok(launch)
}
