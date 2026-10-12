//! `tilo-relay-admin` — thin CLI to bind/unbind workspaces + inspect
//! the running daemon by reading its on-disk state.
//!
//! Mutations are durable file updates which a running daemon reloads.
//! `status` combines configuration, workspace state, and the daemon's
//! periodically persisted metrics snapshot.

mod files_probe;
mod files_setup;

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};

use tilo_relay::RelayConfig;

const DEFAULT_CONFIG_PATH: &str = "/etc/tilo/relay.toml";

#[derive(Parser)]
#[command(
    name = "tilo-relay-admin",
    version,
    about = "Administer a tilo-relay daemon"
)]
struct Cli {
    /// Path to the relay TOML config.
    #[arg(long, global = true, default_value = DEFAULT_CONFIG_PATH)]
    config: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Prepare an exact enrolled Files binding using existing identity and explicit filesystem permission.
    FilesWorkspaceJoin {
        #[command(flatten)]
        options: files_setup::JoinOptions,
    },
    /// Explicitly prepare an empty primary Files namespace for the existing device; never recover or replace saved data.
    FilesWorkspaceInitialize {
        #[command(flatten)]
        options: files_setup::InitializeOptions,
    },
    /// Validate existing primary Files state and its signed filesystem permission before service launch.
    FilesWorkspaceCheck {
        #[arg(long)]
        identity_root: PathBuf,
        #[arg(long)]
        cache_dir: PathBuf,
    },
    /// Sign an existing Files authority revision through the existing protected person identity.
    PublishAuthority {
        #[arg(long)]
        acl: PathBuf,
    },
    /// Query an already-enrolled home peer for public Files binding metadata, without updating local state.
    HomePeerFilesStatus {
        #[arg(long)]
        identity_root: PathBuf,
        #[arg(long)]
        device: String,
        #[arg(long)]
        service: String,
        #[arg(long)]
        workspace_id: u64,
        /// Inspect an existing legacy registry without migrating or copying it.
        #[arg(long)]
        registry: Option<PathBuf>,
    },
    /// Retry durable removals queued while a home peer was offline.
    HomePeerRetryRemovals {
        #[arg(long)]
        identity_root: PathBuf,
    },
    /// Read authenticated health from an enrolled home peer.
    HomePeerStatus {
        #[arg(long)]
        identity_root: PathBuf,
        #[arg(long)]
        device: String,
        #[arg(long)]
        service: String,
    },
    /// Refresh every enrolled home peer's registration from its current
    /// status: endpoint, and what its workers run for environments. A peer
    /// that does not answer keeps its last registration.
    HomePeerRefresh {
        #[arg(long)]
        identity_root: PathBuf,
    },
    /// Rename an enrolled home peer after mutual authentication.
    HomePeerRename {
        #[arg(long)]
        identity_root: PathBuf,
        #[arg(long)]
        device: String,
        #[arg(long)]
        service: String,
        #[arg(long)]
        label: String,
    },
    /// Root-revoke and permanently stop an enrolled home peer.
    HomePeerRemove {
        #[arg(long)]
        identity_root: PathBuf,
        #[arg(long)]
        device: String,
        #[arg(long)]
        service: String,
    },
    /// Bind a workspace to this relay.
    BindWorkspace {
        /// Numeric workspace id.
        workspace_id: u64,
        /// Pin the workspace (persistent CRDT snapshot on disk).
        #[arg(long, default_value_t = true)]
        pin: bool,
    },
    /// Bind a user-facing room UUID, deriving its stable transport id.
    BindRoom {
        /// Durable room UUID shown by the shell.
        room: String,
        /// Pin the workspace (persistent CRDT snapshot on disk).
        #[arg(long, default_value_t = true)]
        pin: bool,
    },
    /// Unbind a workspace from this relay.
    UnbindWorkspace {
        /// Numeric workspace id.
        workspace_id: u64,
    },
    /// List bound workspaces.
    ListWorkspaces,
    /// Add a device to a workspace's durable offline-delivery membership.
    AddMember {
        /// Numeric workspace id (derive it with `bind-room`).
        workspace_id: u64,
        /// 32-byte device public key, hex encoded.
        device_pubkey_hex: String,
    },
    /// Remove a device from explicit shared-host membership. A matching-user
    /// home host will admit the owner's device again; revoke it to deny access.
    RemoveMember {
        workspace_id: u64,
        device_pubkey_hex: String,
    },
    /// List device keys admitted to a workspace.
    ListMembers { workspace_id: u64 },
    /// Print daemon status (config + on-disk state summary).
    Status,
    /// Publish a `RevocationEntry` for a device pubkey (hex-encoded, 32 bytes).
    ///
    /// Requires access to the user root private key, so this
    /// subcommand only runs on the user's primary device.
    RevokeDevice {
        /// Hex-encoded device pubkey to revoke.
        device_pubkey_hex: String,
        /// lost, stolen, retired, or compromised.
        #[arg(long, default_value = "compromised")]
        reason: String,
    },
}

fn load(path: &std::path::Path) -> anyhow::Result<RelayConfig> {
    if path.exists() {
        Ok(RelayConfig::load(path).with_context(|| format!("load {}", path.display()))?)
    } else {
        Ok(RelayConfig::default())
    }
}

fn device_key(value: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = hex::decode(value.trim()).with_context(|| "device pubkey must be hex")?;
    bytes.try_into().map_err(|bytes: Vec<u8>| {
        anyhow::anyhow!("device pubkey must be 32 bytes; got {}", bytes.len())
    })
}

fn workspace_store(
    cfg: &RelayConfig,
    workspace_id: u64,
) -> anyhow::Result<tilo_relay::store::WorkspaceStore> {
    let binding = cfg
        .workspaces
        .iter()
        .find(|workspace| workspace.id == workspace_id)
        .ok_or_else(|| anyhow::anyhow!("workspace {workspace_id} is not bound"))?;
    tilo_relay::store::WorkspaceStore::open(&cfg.state_root, workspace_id, binding.pin)
        .map_err(Into::into)
}

fn main() -> anyhow::Result<std::process::ExitCode> {
    // Automerge isolation workers re-enter this binary; serve them first.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return Ok(std::process::ExitCode::from(code));
    }
    run().map(|()| std::process::ExitCode::SUCCESS)
}

fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::FilesWorkspaceJoin { options } => files_setup::join(&options),
        Cmd::FilesWorkspaceInitialize { options } => files_setup::initialize(&options),
        Cmd::FilesWorkspaceCheck {
            identity_root,
            cache_dir,
        } => files_setup::check(&identity_root, &cache_dir),
        Cmd::PublishAuthority { acl } => files_setup::publish(&acl),
        Cmd::HomePeerFilesStatus {
            identity_root,
            device,
            service,
            workspace_id,
            registry,
        } => files_probe::run(
            &identity_root,
            &device,
            &service,
            workspace_id,
            registry.as_deref(),
        ),
        Cmd::HomePeerRetryRemovals { identity_root } => retry_pending_removals(&identity_root),
        Cmd::HomePeerStatus {
            identity_root,
            device,
            service,
        } => cmd_home_peer(&identity_root, &device, &service, None),
        Cmd::HomePeerRename {
            identity_root,
            device,
            service,
            label,
        } => cmd_home_peer(&identity_root, &device, &service, Some(label)),
        Cmd::HomePeerRefresh { identity_root } => cmd_home_peer_refresh(&identity_root),
        Cmd::HomePeerRemove {
            identity_root,
            device,
            service,
        } => cmd_home_peer_remove(&identity_root, &device, &service),
        Cmd::BindWorkspace { workspace_id, pin } => {
            cmd_bind_workspace(&cli.config, workspace_id, pin)
        }
        Cmd::BindRoom { room, pin } => cmd_bind_room(&cli.config, &room, pin),
        Cmd::UnbindWorkspace { workspace_id } => cmd_unbind_workspace(&cli.config, workspace_id),
        Cmd::ListWorkspaces => cmd_list_workspaces(&cli.config),
        Cmd::AddMember {
            workspace_id,
            device_pubkey_hex,
        } => cmd_add_member(&cli.config, workspace_id, &device_pubkey_hex),
        Cmd::RemoveMember {
            workspace_id,
            device_pubkey_hex,
        } => cmd_remove_member(&cli.config, workspace_id, &device_pubkey_hex),
        Cmd::ListMembers { workspace_id } => cmd_list_members(&cli.config, workspace_id),
        Cmd::Status => cmd_status(&cli.config),
        Cmd::RevokeDevice {
            device_pubkey_hex,
            reason,
        } => cmd_revoke_device(&cli.config, &device_pubkey_hex, &reason),
    }
}

fn remote_home_peer(
    identity_root: &std::path::Path,
    device: &str,
    service: &str,
    request: tilo_relay::transport::HomePeerManagementRequest,
) -> anyhow::Result<tilo_relay::transport::HomePeerManagementResponse> {
    let paths = tilo_user_identity::IdentityPaths::under(identity_root);
    let user = tilo_user_identity::persist::read_public_key(&paths.user_pubkey)?;
    let signing_key = tilo_user_identity::persist::read_device_key(&paths.device_privkey)?;
    let cert = tilo_user_identity::persist::read_device_cert(&paths.device_cert_for_user(&user))
        .or_else(|_| tilo_user_identity::persist::read_device_cert(&paths.device_cert))?;
    let expected_device = device_key(device)?;
    let service = device_key(service)?;
    let relay_addr = registered_home_peer_addr(identity_root, device, service)?;
    tilo_rt::block_on(async move {
        let endpoint = iroh::Endpoint::bind(iroh::endpoint::presets::N0).await?;
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tilo_relay::client::RelayClient::manage_home_peer(
                &endpoint,
                relay_addr,
                user,
                expected_device,
                cert,
                signing_key,
                tilo_user_identity::RevocationList::new(),
                request,
            ),
        )
        .await
        .map_err(|_| anyhow::anyhow!("home peer did not answer within 10 seconds"))??;
        endpoint.close().await;
        Ok(response)
    })?
}

fn cmd_home_peer(
    identity_root: &std::path::Path,
    device: &str,
    service: &str,
    rename: Option<String>,
) -> anyhow::Result<()> {
    let requested_label = rename.clone();
    let request = rename.map_or(
        tilo_relay::transport::HomePeerManagementRequest::Status,
        |label| tilo_relay::transport::HomePeerManagementRequest::Rename { label },
    );
    let response = remote_home_peer(identity_root, device, service, request)?;
    if let Some(label) = requested_label {
        update_home_peer_registry(identity_root, device, Some(label))?;
    }
    refresh_home_peer_endpoint(identity_root, device, device_key(service)?, &response)?;
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}

fn cmd_home_peer_refresh(identity_root: &std::path::Path) -> anyhow::Result<()> {
    let path = identity_root.join("home-peers.json");
    let registrations: Vec<tilo_relay::HomePeerRegistration> =
        serde_json::from_slice(&std::fs::read(&path)?)?;
    let mut refreshed = 0usize;
    for registration in &registrations {
        let (device, service) = (
            registration.device_node_id.as_str(),
            registration.relay_service_node_id.as_str(),
        );
        let answered = remote_home_peer(
            identity_root,
            device,
            service,
            tilo_relay::transport::HomePeerManagementRequest::Status,
        )
        .and_then(|response| {
            refresh_home_peer_endpoint(identity_root, device, device_key(service)?, &response)
        });
        match answered {
            Ok(()) => refreshed = refreshed.saturating_add(1),
            Err(error) => eprintln!("{}: {error:#}", registration.label),
        }
    }
    println!(
        "{}",
        serde_json::json!({ "refreshed": refreshed, "registered": registrations.len() })
    );
    Ok(())
}

fn registered_home_peer_addr(
    identity_root: &std::path::Path,
    device: &str,
    service: [u8; 32],
) -> anyhow::Result<iroh_base::EndpointAddr> {
    let service_id = iroh_base::PublicKey::from_bytes(&service)?;
    let path = tilo_relay::home_peer_registry_path(identity_root);
    let registrations: Vec<tilo_relay::HomePeerRegistration> =
        serde_json::from_slice(&std::fs::read(path)?)?;
    let registration = registrations
        .into_iter()
        .find(|entry| {
            entry.device_node_id == device && entry.relay_service_node_id == hex::encode(service)
        })
        .ok_or_else(|| anyhow::anyhow!("home peer is not registered"))?;
    Ok(registration
        .relay_endpoint
        .filter(|endpoint| endpoint.id == service_id && !endpoint.is_empty())
        .unwrap_or_else(|| iroh_base::EndpointAddr::new(service_id)))
}

fn refresh_home_peer_endpoint(
    identity_root: &std::path::Path,
    device: &str,
    service: [u8; 32],
    response: &tilo_relay::transport::HomePeerManagementResponse,
) -> anyhow::Result<()> {
    let endpoint = &response.relay_endpoint;
    if endpoint.id.as_bytes() != &service || endpoint.is_empty() {
        anyhow::bail!("home peer returned an invalid relay endpoint advertisement");
    }
    let path = tilo_relay::home_peer_registry_path(identity_root);
    let mut registrations: Vec<tilo_relay::HomePeerRegistration> =
        serde_json::from_slice(&std::fs::read(&path)?)?;
    let registration = registrations
        .iter_mut()
        .find(|entry| {
            entry.device_node_id == device && entry.relay_service_node_id == hex::encode(service)
        })
        .ok_or_else(|| anyhow::anyhow!("home peer is not registered"))?;
    registration.relay_endpoint = Some(endpoint.clone());
    registration.environments = response.environments;
    registration.schema_version = tilo_relay::HomePeerRegistration::CURRENT_SCHEMA;
    tilo_files::native::atomic::write_with_mode(
        &path,
        &serde_json::to_vec_pretty(&registrations)?,
        0o644,
    )?;
    Ok(())
}

fn cmd_home_peer_remove(
    identity_root: &std::path::Path,
    device: &str,
    service: &str,
) -> anyhow::Result<()> {
    let paths = tilo_user_identity::IdentityPaths::under(identity_root);
    let target = device_key(device)?;
    let mut pending = load_pending_decommissions(identity_root)?;
    let intent = if let Some(existing) = pending
        .iter()
        .find(|intent| intent.device == device)
        .cloned()
    {
        existing
    } else {
        let root = tilo_user_identity::persist::read_owned_signing_key(&paths.user_privkey)?;
        let mut revocation = tilo_user_identity::RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: target,
            revoked_at_ns: tilo_clock::unix_now_ns(),
            reason: tilo_user_identity::RevocationReason::Retired,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut revocation, &root)?;
        let intent = PendingDecommission {
            device: device.to_string(),
            service: service.to_string(),
            revocation,
        };
        pending.push(intent.clone());
        save_pending_decommissions(identity_root, &pending)?;
        intent
    };

    // Local denial is durable first. The management dial deliberately uses
    // an empty local server revocation view so this exact pinned certificate
    // remains reachable only long enough to deliver its own decommission.
    persist_canonical_revocation(&paths, &intent.revocation)?;
    let mut fleet_revocations = tilo_relay::store::load_revocations(identity_root)?;
    fleet_revocations.retain(|entry| entry.revoked_device_public_key != target);
    fleet_revocations.push(intent.revocation.clone());
    tilo_relay::store::save_revocations(identity_root, &fleet_revocations)?;
    persist_fleet_revocation(&intent.revocation)?;
    if tilo_relay::home_peer_registry_path(identity_root).exists() {
        update_home_peer_registry(identity_root, device, None)?;
    }

    let remote = remote_home_peer(
        identity_root,
        &intent.device,
        &intent.service,
        tilo_relay::transport::HomePeerManagementRequest::Decommission {
            revocation: intent.revocation.clone(),
        },
    );
    if remote
        .as_ref()
        .is_ok_and(|response| response.decommissioned)
    {
        pending.retain(|queued| queued.device != intent.device);
        save_pending_decommissions(identity_root, &pending)?;
    }
    let response = remote.unwrap_or(tilo_relay::transport::HomePeerManagementResponse {
        device_public_key: target,
        relay_endpoint: iroh_base::EndpointAddr::new(iroh_base::PublicKey::from_bytes(
            &device_key(&intent.service)?,
        )?),
        label: "Removed".into(),
        files_ready: false,
        services: tilo_relay_protocol::ServiceCapabilities::default(),
        environments: tilo_relay_protocol::RelayEnvironments::default(),
        decommissioned: false,
    });
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct PendingDecommission {
    device: String,
    service: String,
    revocation: tilo_user_identity::RevocationEntry,
}

fn pending_decommissions_path(identity_root: &std::path::Path) -> PathBuf {
    identity_root.join("pending-home-peer-decommissions.json")
}

fn load_pending_decommissions(
    identity_root: &std::path::Path,
) -> anyhow::Result<Vec<PendingDecommission>> {
    match std::fs::read(pending_decommissions_path(identity_root)) {
        Ok(bytes) if bytes.len() <= 1024 * 1024 => Ok(serde_json::from_slice(&bytes)?),
        Ok(_) => anyhow::bail!("pending home-peer removal list is too large"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

fn save_pending_decommissions(
    identity_root: &std::path::Path,
    pending: &[PendingDecommission],
) -> anyhow::Result<()> {
    tilo_files::native::atomic::write_with_mode(
        &pending_decommissions_path(identity_root),
        &serde_json::to_vec_pretty(pending)?,
        0o600,
    )?;
    Ok(())
}

fn retry_pending_removals(identity_root: &std::path::Path) -> anyhow::Result<()> {
    let mut pending = load_pending_decommissions(identity_root)?;
    for intent in pending.clone() {
        let paths = tilo_user_identity::IdentityPaths::under(identity_root);
        let target = device_key(&intent.device)?;
        persist_canonical_revocation(&paths, &intent.revocation)?;
        let mut relay_revocations = tilo_relay::store::load_revocations(identity_root)?;
        relay_revocations.retain(|entry| entry.revoked_device_public_key != target);
        relay_revocations.push(intent.revocation.clone());
        tilo_relay::store::save_revocations(identity_root, &relay_revocations)?;
        persist_fleet_revocation(&intent.revocation)?;
        if tilo_relay::home_peer_registry_path(identity_root).exists() {
            update_home_peer_registry(identity_root, &intent.device, None)?;
        }
        if remote_home_peer(
            identity_root,
            &intent.device,
            &intent.service,
            tilo_relay::transport::HomePeerManagementRequest::Decommission {
                revocation: intent.revocation,
            },
        )
        .is_ok_and(|response| response.decommissioned)
        {
            pending.retain(|queued| queued.device != intent.device);
            save_pending_decommissions(identity_root, &pending)?;
        }
    }
    println!("{}", pending.len());
    Ok(())
}

fn persist_canonical_revocation(
    paths: &tilo_user_identity::IdentityPaths,
    revocation: &tilo_user_identity::RevocationEntry,
) -> anyhow::Result<()> {
    let path = paths
        .user_pubkey
        .parent()
        .ok_or_else(|| anyhow::anyhow!("identity public-key path has no parent"))?
        .join("revocations.bin");
    let mut entries: Vec<tilo_user_identity::RevocationEntry> = match std::fs::read(&path) {
        Ok(bytes) if bytes.len() <= 4 * 1024 * 1024 => postcard::from_bytes(&bytes)?,
        Ok(_) => anyhow::bail!("canonical revocation list is too large"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    if entries.len() >= 4096 {
        anyhow::bail!("canonical revocation list is full");
    }
    for entry in &entries {
        tilo_user_identity::verify_revocation_entry(entry)?;
        if entry.user_public_key != revocation.user_public_key {
            anyhow::bail!("canonical revocation list crosses identity roots");
        }
    }
    entries.retain(|entry| entry.revoked_device_public_key != revocation.revoked_device_public_key);
    entries.push(revocation.clone());
    entries.sort_by_key(|entry| entry.revoked_device_public_key);
    tilo_files::native::atomic::write_with_mode(&path, &postcard::to_allocvec(&entries)?, 0o600)?;
    Ok(())
}

fn persist_fleet_revocation(
    revocation: &tilo_user_identity::RevocationEntry,
) -> anyhow::Result<()> {
    let config = tilo_paths::xdg::config_home()
        .ok_or_else(|| anyhow::anyhow!("user configuration directory is unavailable"))?;
    let path = config
        .join("tilo/identities/fleets")
        .join(format!("{}.json", hex::encode(revocation.user_public_key)));
    if !path.exists() {
        return Ok(());
    }
    let bytes = std::fs::read(&path)?;
    if bytes.len() > 4 * 1024 * 1024 {
        anyhow::bail!("device fleet is too large");
    }
    let mut fleet: serde_json::Value = serde_json::from_slice(&bytes)?;
    let revocations = fleet
        .get_mut("revocations")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| anyhow::anyhow!("device fleet has no revocation list"))?;
    revocations.retain(|entry| {
        entry
            .get("revoked_device_public_key")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|bytes| {
                bytes
                    .iter()
                    .filter_map(serde_json::Value::as_u64)
                    .collect::<Vec<_>>()
                    != revocation
                        .revoked_device_public_key
                        .iter()
                        .map(|byte| u64::from(*byte))
                        .collect::<Vec<_>>()
            })
    });
    revocations.push(serde_json::to_value(revocation)?);
    tilo_files::native::atomic::write_with_mode(&path, &serde_json::to_vec_pretty(&fleet)?, 0o600)?;
    Ok(())
}

fn update_home_peer_registry(
    identity_root: &std::path::Path,
    device: &str,
    new_label: Option<String>,
) -> anyhow::Result<()> {
    let path = tilo_relay::home_peer_registry_path(identity_root);
    let bytes = std::fs::read(&path)?;
    if bytes.len() > 1024 * 1024 {
        anyhow::bail!("home-peer registry is too large");
    }
    let mut registrations: Vec<tilo_relay::HomePeerRegistration> = serde_json::from_slice(&bytes)?;
    if let Some(label) = new_label {
        let label = label.trim();
        if !tilo_relay::valid_home_peer_label(label) {
            anyhow::bail!("home-peer name must be 1 to 80 bytes");
        }
        let registration = registrations
            .iter_mut()
            .find(|entry| entry.device_node_id == device)
            .ok_or_else(|| anyhow::anyhow!("home peer is not registered"))?;
        registration.label = label.to_string();
    } else {
        registrations.retain(|entry| entry.device_node_id != device);
    }
    tilo_files::native::atomic::write_with_mode(
        &path,
        &serde_json::to_vec_pretty(&registrations)?,
        0o644,
    )?;
    Ok(())
}

fn cmd_bind_workspace(
    config: &std::path::Path,
    workspace_id: u64,
    pin: bool,
) -> anyhow::Result<()> {
    let mut cfg = load(config)?;
    let added = cfg.add_workspace(workspace_id, pin);
    cfg.save(config)?;
    if added {
        println!("bound workspace {workspace_id} (pin={pin})");
    } else {
        println!("workspace {workspace_id} already bound");
    }
    Ok(())
}

fn cmd_bind_room(config: &std::path::Path, room: &str, pin: bool) -> anyhow::Result<()> {
    let workspace_id = tilo_relay::room_workspace_id(room)
        .with_context(|| format!("invalid room UUID {room:?}"))?;
    let mut cfg = load(config)?;
    let added = cfg.add_workspace(workspace_id, pin);
    cfg.save(config)?;
    println!(
        "{} room {room} as workspace {workspace_id} (pin={pin})",
        if added { "bound" } else { "already bound" }
    );
    Ok(())
}

fn cmd_unbind_workspace(config: &std::path::Path, workspace_id: u64) -> anyhow::Result<()> {
    let mut cfg = load(config)?;
    let removed = cfg.remove_workspace(workspace_id);
    cfg.save(config)?;
    if removed {
        println!("unbound workspace {workspace_id}");
    } else {
        println!("workspace {workspace_id} was not bound");
    }
    Ok(())
}

fn cmd_list_workspaces(config: &std::path::Path) -> anyhow::Result<()> {
    let cfg = load(config)?;
    for ws in &cfg.workspaces {
        println!("{}\tpin={}", ws.id, ws.pin);
    }
    Ok(())
}

fn cmd_add_member(
    config: &std::path::Path,
    workspace_id: u64,
    device_pubkey_hex: &str,
) -> anyhow::Result<()> {
    let cfg = load(config)?;
    let store = workspace_store(&cfg, workspace_id)?;
    let device = device_key(device_pubkey_hex)?;
    let mut members = store.load_membership()?;
    if !members.contains(&device) {
        members.push(device);
        store.save_membership(&members)?;
    }
    println!("workspace {workspace_id}: admitted {}", hex::encode(device));
    println!("a running tilo-relay reloads membership within one second");
    Ok(())
}

fn cmd_remove_member(
    config: &std::path::Path,
    workspace_id: u64,
    device_pubkey_hex: &str,
) -> anyhow::Result<()> {
    let cfg = load(config)?;
    let store = workspace_store(&cfg, workspace_id)?;
    let device = device_key(device_pubkey_hex)?;
    let mut members = store.load_membership()?;
    members.retain(|member| member != &device);
    store.save_membership(&members)?;
    println!("workspace {workspace_id}: removed {}", hex::encode(device));
    println!("a running tilo-relay disconnects removed members within one second");
    Ok(())
}

fn cmd_list_members(config: &std::path::Path, workspace_id: u64) -> anyhow::Result<()> {
    let cfg = load(config)?;
    for member in workspace_store(&cfg, workspace_id)?.load_membership()? {
        println!("{}", hex::encode(member));
    }
    Ok(())
}

fn cmd_status(config: &std::path::Path) -> anyhow::Result<()> {
    let cfg = load(config)?;
    println!("tilo-relay-admin {}", tilo_relay::VERSION);
    println!("config:    {}", config.display());
    println!("state:     {}", cfg.state_root.display());
    println!("identity:  {}", cfg.identity_root.display());
    println!("iroh:      {}", cfg.iroh_listen);
    println!("bindings:");
    for ws in &cfg.workspaces {
        println!("  {}\tpin={}", ws.id, ws.pin);
    }
    let workspace_root = cfg.state_root.join("workspaces");
    let mut durable: Vec<_> = std::fs::read_dir(&workspace_root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u64>().ok())
        .collect();
    durable.sort_unstable();
    durable.dedup();
    println!("durable workspace shards: {}", durable.len());
    for workspace_id in durable {
        let store = tilo_relay::store::WorkspaceStore::open(&cfg.state_root, workspace_id, true)?;
        let members = store.load_membership()?.len();
        let retained = store.load_retained()?.len();
        println!("  {workspace_id}\tmembers={members}\tretained={retained}");
    }
    if let Some(metrics) = tilo_relay::store::load_metrics(&cfg.state_root)? {
        println!("metrics:");
        println!("  connections accepted: {}", metrics.conns_accepted);
        println!("  connections authed:   {}", metrics.conns_authed);
        println!("  auth rejected:        {}", metrics.auth_rejected);
        println!("  deltas received:      {}", metrics.deltas_received);
        println!("  deltas fanned out:    {}", metrics.deltas_fanned_out);
        println!("  bytes queued:         {}", metrics.bytes_queued);
        println!("  bytes sent:           {}", metrics.bytes_sent);
        println!("  bytes sent live:      {}", metrics.bytes_sent_live);
        for (workspace_id, egress) in &metrics.rooms {
            println!(
                "  {workspace_id}\tbytes sent={}\tlive={}",
                egress.bytes_sent, egress.bytes_sent_live
            );
        }
    }
    Ok(())
}

fn cmd_revoke_device(
    config: &std::path::Path,
    device_pubkey_hex: &str,
    reason: &str,
) -> anyhow::Result<()> {
    let cfg = load(config)?;
    let paths = tilo_user_identity::IdentityPaths::under(&cfg.identity_root);
    if !paths.user_privkey.exists() {
        anyhow::bail!(
            "cannot revoke: no user root private key at {}. Run this from your primary device.",
            paths.user_privkey.display()
        );
    }
    let pk = device_key(device_pubkey_hex)?;
    let reason = match reason {
        "lost" => tilo_user_identity::RevocationReason::Lost,
        "stolen" => tilo_user_identity::RevocationReason::Stolen,
        "retired" => tilo_user_identity::RevocationReason::Retired,
        "compromised" => tilo_user_identity::RevocationReason::Compromised,
        _ => anyhow::bail!("reason must be lost, stolen, retired, or compromised"),
    };
    let root = tilo_user_identity::persist::read_owned_signing_key(&paths.user_privkey)?;
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: pk,
        revoked_at_ns: tilo_clock::unix_now_ns(),
        reason,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &root)?;
    let mut entries = tilo_relay::store::load_revocations(&cfg.state_root)?;
    entries.retain(|existing| existing.revoked_device_public_key != pk);
    entries.push(entry);
    tilo_relay::store::save_revocations(&cfg.state_root, &entries)?;
    println!("revoked device {} ({reason:?})", hex::encode(pk));
    println!("a running tilo-relay disconnects the device within one second");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn canonical_revocation_aggregate_is_verified_deduplicated_and_private() {
        let temp = tempfile::tempdir().unwrap();
        let paths = tilo_user_identity::IdentityPaths::under(temp.path());
        std::fs::create_dir_all(paths.user_pubkey.parent().unwrap()).unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[71; 32]);
        let mut entry = tilo_user_identity::RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: [72; 32],
            revoked_at_ns: 1,
            reason: tilo_user_identity::RevocationReason::Retired,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
        persist_canonical_revocation(&paths, &entry).unwrap();
        entry.revoked_at_ns = 2;
        tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
        persist_canonical_revocation(&paths, &entry).unwrap();

        let path = paths.user_pubkey.parent().unwrap().join("revocations.bin");
        let stored: Vec<tilo_user_identity::RevocationEntry> =
            postcard::from_bytes(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored, vec![entry.clone()]);
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let pending = PendingDecommission {
            device: hex::encode(entry.revoked_device_public_key),
            service: hex::encode([73; 32]),
            revocation: entry.clone(),
        };
        save_pending_decommissions(temp.path(), std::slice::from_ref(&pending)).unwrap();
        let after_restart = load_pending_decommissions(temp.path()).unwrap();
        assert_eq!(after_restart.len(), 1);
        assert_eq!(after_restart[0].device, pending.device);
        assert_eq!(after_restart[0].revocation, entry);
    }
}
