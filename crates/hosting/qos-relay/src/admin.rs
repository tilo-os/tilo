//! `qos-relay-admin` — thin CLI to bind/unbind workspaces + inspect
//! the running daemon by reading its on-disk state.
//!
//! Mutations are durable file updates which a running daemon reloads.
//! `status` combines configuration, workspace state, and the daemon's
//! periodically persisted metrics snapshot.

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};

use qos_relay::RelayConfig;

const DEFAULT_CONFIG_PATH: &str = "/etc/qos/relay.toml";

#[derive(Parser)]
#[command(
    name = "qos-relay-admin",
    version,
    about = "Administer a qos-relay daemon"
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

fn load(path: &PathBuf) -> anyhow::Result<RelayConfig> {
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
) -> anyhow::Result<qos_relay::store::WorkspaceStore> {
    let binding = cfg
        .workspaces
        .iter()
        .find(|workspace| workspace.id == workspace_id)
        .ok_or_else(|| anyhow::anyhow!("workspace {workspace_id} is not bound"))?;
    qos_relay::store::WorkspaceStore::open(&cfg.state_root, workspace_id, binding.pin)
        .map_err(Into::into)
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::BindWorkspace { workspace_id, pin } => {
            let mut cfg = load(&cli.config)?;
            let added = cfg.add_workspace(workspace_id, pin);
            cfg.save(&cli.config)?;
            if added {
                println!("bound workspace {workspace_id} (pin={pin})");
            } else {
                println!("workspace {workspace_id} already bound");
            }
        }
        Cmd::BindRoom { room, pin } => {
            let workspace_id = qos_relay::room_workspace_id(&room)
                .with_context(|| format!("invalid room UUID {room:?}"))?;
            let mut cfg = load(&cli.config)?;
            let added = cfg.add_workspace(workspace_id, pin);
            cfg.save(&cli.config)?;
            println!(
                "{} room {room} as workspace {workspace_id} (pin={pin})",
                if added { "bound" } else { "already bound" }
            );
        }
        Cmd::UnbindWorkspace { workspace_id } => {
            let mut cfg = load(&cli.config)?;
            let removed = cfg.remove_workspace(workspace_id);
            cfg.save(&cli.config)?;
            if removed {
                println!("unbound workspace {workspace_id}");
            } else {
                println!("workspace {workspace_id} was not bound");
            }
        }
        Cmd::ListWorkspaces => {
            let cfg = load(&cli.config)?;
            for ws in &cfg.workspaces {
                println!("{}\tpin={}", ws.id, ws.pin);
            }
        }
        Cmd::AddMember {
            workspace_id,
            device_pubkey_hex,
        } => {
            let cfg = load(&cli.config)?;
            let store = workspace_store(&cfg, workspace_id)?;
            let device = device_key(&device_pubkey_hex)?;
            let mut members = store.load_membership()?;
            if !members.contains(&device) {
                members.push(device);
                store.save_membership(&members)?;
            }
            println!("workspace {workspace_id}: admitted {}", hex::encode(device));
            println!("a running qos-relay reloads membership within one second");
        }
        Cmd::RemoveMember {
            workspace_id,
            device_pubkey_hex,
        } => {
            let cfg = load(&cli.config)?;
            let store = workspace_store(&cfg, workspace_id)?;
            let device = device_key(&device_pubkey_hex)?;
            let mut members = store.load_membership()?;
            members.retain(|member| member != &device);
            store.save_membership(&members)?;
            println!("workspace {workspace_id}: removed {}", hex::encode(device));
            println!("a running qos-relay disconnects removed members within one second");
        }
        Cmd::ListMembers { workspace_id } => {
            let cfg = load(&cli.config)?;
            for member in workspace_store(&cfg, workspace_id)?.load_membership()? {
                println!("{}", hex::encode(member));
            }
        }
        Cmd::Status => {
            let cfg = load(&cli.config)?;
            println!("qos-relay-admin {}", qos_relay::VERSION);
            println!("config:    {}", cli.config.display());
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
                let store =
                    qos_relay::store::WorkspaceStore::open(&cfg.state_root, workspace_id, true)?;
                let members = store.load_membership()?.len();
                let retained = store.load_retained()?.len();
                println!("  {workspace_id}\tmembers={members}\tretained={retained}");
            }
            if let Some(metrics) = qos_relay::store::load_metrics(&cfg.state_root)? {
                println!("metrics:");
                println!("  connections accepted: {}", metrics.conns_accepted);
                println!("  connections authed:   {}", metrics.conns_authed);
                println!("  auth rejected:        {}", metrics.auth_rejected);
                println!("  deltas received:      {}", metrics.deltas_received);
                println!("  deltas fanned out:    {}", metrics.deltas_fanned_out);
                println!("  bytes queued:         {}", metrics.bytes_queued);
            }
        }
        Cmd::RevokeDevice {
            device_pubkey_hex,
            reason,
        } => {
            let cfg = load(&cli.config)?;
            let paths = qos_user_identity::IdentityPaths::under(&cfg.identity_root);
            if !paths.user_privkey.exists() {
                anyhow::bail!(
                    "cannot revoke: no user root private key at {}. Run this from your primary device.",
                    paths.user_privkey.display()
                );
            }
            let pk = device_key(&device_pubkey_hex)?;
            let reason = match reason.as_str() {
                "lost" => qos_user_identity::RevocationReason::Lost,
                "stolen" => qos_user_identity::RevocationReason::Stolen,
                "retired" => qos_user_identity::RevocationReason::Retired,
                "compromised" => qos_user_identity::RevocationReason::Compromised,
                _ => anyhow::bail!("reason must be lost, stolen, retired, or compromised"),
            };
            let root = qos_user_identity::persist::read_signing_key(&paths.user_privkey)?;
            let mut entry = qos_user_identity::RevocationEntry {
                user_public_key: root.verifying_key().to_bytes(),
                revoked_device_public_key: pk,
                revoked_at_ns: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |duration| duration.as_nanos() as u64),
                reason,
                signature: [0; 64],
            };
            qos_user_identity::sign_revocation_entry(&mut entry, &root)?;
            let mut entries = qos_relay::store::load_revocations(&cfg.state_root)?;
            entries.retain(|existing| existing.revoked_device_public_key != pk);
            entries.push(entry);
            qos_relay::store::save_revocations(&cfg.state_root, &entries)?;
            println!("revoked device {} ({reason:?})", hex::encode(pk));
            println!("a running qos-relay disconnects the device within one second");
        }
    }
    Ok(())
}
