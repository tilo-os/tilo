//! `qos-relay-admin` — thin CLI to bind/unbind workspaces + inspect
//! the running daemon by reading its on-disk state.
//!
//! For the initial cut this simply mirrors the workspace-binding
//! subcommands from `qos-relay`. Once the daemon exposes a UDS
//! control socket, `status` will read live counters from it; today
//! it just prints the config.

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
    /// Unbind a workspace from this relay.
    UnbindWorkspace {
        /// Numeric workspace id.
        workspace_id: u64,
    },
    /// List bound workspaces.
    ListWorkspaces,
    /// Print daemon status (config + on-disk state summary).
    Status,
    /// Publish a `RevocationEntry` for a device pubkey (hex-encoded, 32 bytes).
    ///
    /// Requires access to the user root private key, so this
    /// subcommand only runs on the user's primary device.
    RevokeDevice {
        /// Hex-encoded device pubkey to revoke.
        device_pubkey_hex: String,
    },
}

fn load(path: &PathBuf) -> anyhow::Result<RelayConfig> {
    if path.exists() {
        Ok(RelayConfig::load(path).with_context(|| format!("load {}", path.display()))?)
    } else {
        Ok(RelayConfig::default())
    }
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
        }
        Cmd::RevokeDevice { device_pubkey_hex } => {
            // Guard: refuse if we cannot see a root private key on
            // disk. This subcommand deliberately does NOT sign
            // anything today — the revocation flow needs the
            // WorkspaceDirectory writer + a networked publish to
            // hosts, and that is admin work the caller must do via
            // a device that carries the root key. Ships as a stub
            // that documents the intent + fails loudly.
            let paths = qos_user_identity::IdentityPaths::under(
                &load(&cli.config)?.identity_root,
            );
            if !paths.user_privkey.exists() {
                anyhow::bail!(
                    "cannot revoke: no user root private key at {}. Run this from your primary device.",
                    paths.user_privkey.display()
                );
            }
            let bytes = hex::decode(device_pubkey_hex.trim())
                .with_context(|| "device pubkey must be 32-byte hex")?;
            if bytes.len() != 32 {
                anyhow::bail!("device pubkey must be 32 bytes; got {}", bytes.len());
            }
            let mut pk = [0u8; 32];
            pk.copy_from_slice(&bytes);
            println!(
                "would revoke device {} — signing + publish not yet plumbed through the admin CLI",
                hex::encode(pk)
            );
        }
    }
    Ok(())
}
