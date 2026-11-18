//! `tilo server`: what this person's servers are, and what they can be asked
//! to do from here.
//!
//! A server of the person's is headless; its owner reaches it over the
//! owner-only management route from another of their devices, never over ssh.
//! This command is that device: it reads the servers this computer enrolled
//! ([`home_peer_registry_path`]) and asks one for its health, a service's
//! journal, a restart, a reboot, an update, a rollback or a drain
//! ([`ServerOperation`]). Every answer is the server's own
//! ([`ServerOperationResult`]).

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use tilo_device_identity::{IdentityPaths, persist};
use tilo_server_protocol::transport::{
    HomePeerManagementRequest, ServerAnswer, ServerOperation, ServerOperationResult,
};
use tilo_server_protocol::{HomePeerRegistration, read_home_peers};
use tilo_user_identity::{RevocationList, home_peer_registry_path};

use crate::identity;

#[derive(Subcommand)]
pub enum ServerCmd {
    /// The servers this computer can manage.
    List,
    /// What a server is now: its health, what it holds and how its copies
    /// stand, how it unlocks, its updates and its hardware.
    #[command(after_help = "Examples:\n  tilo server status nas\n  tilo server status nas --json")]
    Status {
        /// The server: the name it was enrolled under, or its device key.
        server: String,
        /// The whole answer as JSON.
        #[arg(long)]
        json: bool,
    },
    /// A service's recent journal lines.
    Logs {
        /// The server.
        server: String,
        /// The systemd unit, e.g. tilo-server.service.
        service: String,
        /// How many lines back to read, at most 500.
        #[arg(long, default_value_t = 200)]
        lines: u32,
    },
    /// Restart a service.
    Restart {
        /// The server.
        server: String,
        /// The systemd unit, e.g. tilo-server.service.
        service: String,
    },
    /// Reboot the machine.
    Reboot {
        /// The server.
        server: String,
    },
    /// Fetch and stage the newest OS update now instead of waiting for its
    /// timer.
    Update {
        /// The server.
        server: String,
    },
    /// Stop taking OS updates.
    PauseUpdates {
        /// The server.
        server: String,
    },
    /// Follow OS updates again.
    ResumeUpdates {
        /// The server.
        server: String,
    },
    /// Start the previous system from now on.
    Rollback {
        /// The server.
        server: String,
    },
    /// Suspend its workspaces' Linux machines and hand their work off, before
    /// taking it out of service.
    Drain {
        /// The server.
        server: String,
    },
}

pub fn serve(cmd: ServerCmd) -> Result<std::process::ExitCode> {
    match cmd {
        ServerCmd::List => list(),
        other => {
            let (server, operation, json) = match other {
                ServerCmd::Status { server, json } => (server, ServerOperation::Status, json),
                ServerCmd::Logs {
                    server,
                    service,
                    lines,
                } => (server, ServerOperation::Logs { service, lines }, false),
                ServerCmd::Restart { server, service } => {
                    (server, ServerOperation::Restart { service }, false)
                }
                ServerCmd::Reboot { server } => (server, ServerOperation::Reboot, false),
                ServerCmd::Update { server } => (server, ServerOperation::Update, false),
                ServerCmd::PauseUpdates { server } => {
                    (server, ServerOperation::PauseUpdates, false)
                }
                ServerCmd::ResumeUpdates { server } => {
                    (server, ServerOperation::ResumeUpdates, false)
                }
                ServerCmd::Rollback { server } => (server, ServerOperation::Rollback, false),
                ServerCmd::Drain { server } => (server, ServerOperation::Drain, false),
                ServerCmd::List => return list(),
            };
            let result = ask(&server, operation)?;
            print(&result, json)?;
            Ok(std::process::ExitCode::SUCCESS)
        }
    }
}

fn list() -> Result<std::process::ExitCode> {
    let peers = peers()?;
    if peers.is_empty() {
        bail!(
            "this computer knows no servers; enroll one from your desktop (`tilo device enroll`)"
        );
    }
    for peer in peers {
        println!("{}\t{}", peer.label, peer.device_node_id);
    }
    Ok(std::process::ExitCode::SUCCESS)
}

fn print(result: &ServerOperationResult, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(result)?);
        return Ok(());
    }
    match &result.answer {
        ServerAnswer::Status(status) => {
            println!("{}", status.hostname);
            println!("  up:            {}s", status.uptime_seconds);
            println!(
                "  system:        {}{}",
                if status.booted_image.is_empty() {
                    "unknown"
                } else {
                    &status.booted_image
                },
                if status.staged_image.is_empty() {
                    String::new()
                } else {
                    format!(" (staged {})", status.staged_image)
                }
            );
            println!(
                "  updates:       {}{}{}",
                if status.updates.is_empty() {
                    "the image's"
                } else {
                    &status.updates
                },
                if status.updates_paused {
                    " (paused)"
                } else {
                    ""
                },
                if status.pending_update.is_empty() {
                    String::new()
                } else {
                    format!(", pending {}", status.pending_update)
                }
            );
            println!(
                "  unlocks:       {}",
                if status.unlock.is_empty() {
                    "the image's default"
                } else {
                    &status.unlock
                }
            );
            println!(
                "  hardware:      {} {} ({} processors, {} MB memory, {} of {} GB free)",
                status.hardware.product,
                status.hardware.cpu_model,
                status.hardware.cpus,
                status.hardware.memory_mb,
                status.hardware.storage_free_gb,
                status.hardware.storage_total_gb
            );
            println!("  holds:");
            for held in &status.holding {
                println!(
                    "    {} ({})",
                    if held.name.is_empty() {
                        &held.workspace
                    } else {
                        &held.name
                    },
                    held.workspace
                );
                for holder in &held.holders {
                    let short: String = holder.device.chars().take(12).collect();
                    println!(
                        "      {} {}{}",
                        holder.role,
                        short,
                        if holder.current { "" } else { " (catching up)" }
                    );
                }
                for short in &held.shortfall {
                    println!("      ! {short}");
                }
            }
            if !status.history.is_empty() {
                println!("  recently asked:");
                for entry in status.history.iter().take(10) {
                    println!(
                        "    {}{}",
                        entry.operation,
                        if entry.done { "" } else { " (asked)" }
                    );
                }
            }
        }
        ServerAnswer::Text(text) => print!("{text}"),
        ServerAnswer::Nothing => println!("{}", result.summary),
    }
    Ok(())
}

/// Ask `server` to do `operation`, as this computer's device.
fn ask(server: &str, operation: ServerOperation) -> Result<ServerOperationResult> {
    let dir = identity::dir()?;
    let paths = IdentityPaths::under(&dir);
    let user = persist::read_public_key(&paths.user_pubkey)
        .context("this computer holds no identity; start `tilo login` first")?;
    let signing_key = persist::read_device_key(&paths.device_privkey)?;
    let cert = persist::read_device_cert(&paths.device_cert_for_user(&user))
        .or_else(|_| persist::read_device_cert(&paths.device_cert))?;
    let peer = find(&dir, server)?;
    let expected = key_of(&peer.device_node_id)
        .with_context(|| format!("{} is not a server device key", peer.device_node_id))?;
    let service = key_of(&peer.server_node_id)
        .with_context(|| format!("{} is not a server service key", peer.server_node_id))?;
    let addr = match peer.server_endpoint {
        Some(endpoint) if endpoint.id.as_bytes() == &service && !endpoint.is_empty() => endpoint,
        _ => iroh_addr(service)
            .with_context(|| format!("{} is not a server endpoint", peer.server_node_id))?,
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
        let dialer = tilo_server_client::host::Host::at(addr.clone());
        let endpoint = dialer
            .bind(None)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let answer = tilo_server_session::client::ServerClient::manage_home_peer(
            &endpoint,
            addr,
            user,
            expected,
            cert,
            signing_key,
            RevocationList::new(),
            HomePeerManagementRequest::Operate(operation),
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
        endpoint.close().await;
        Ok::<_, anyhow::Error>(answer.operation)
    })?;
    Ok(result)
}

/// The server this computer enrolled, by its name or its device key.
fn find(dir: &std::path::Path, name: &str) -> Result<HomePeerRegistration> {
    let peers = peers_in(dir)?;
    let name = name.trim();
    peers
        .iter()
        .find(|peer| peer.label == name || peer.device_node_id == name)
        .or_else(|| {
            peers.iter().find(|peer| {
                peer.device_node_id.starts_with(name) || peer.server_node_id.starts_with(name)
            })
        })
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "this computer knows no server {name:?}; `tilo server list` names the ones it does"
            )
        })
}

fn peers() -> Result<Vec<HomePeerRegistration>> {
    peers_in(&identity::dir()?)
}

fn peers_in(dir: &std::path::Path) -> Result<Vec<HomePeerRegistration>> {
    match read_home_peers(&home_peer_registry_path(dir)) {
        Ok(peers) => Ok(peers),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error).context("this computer's servers cannot be read"),
    }
}

fn key_of(hex_key: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(hex_key).ok()?;
    bytes.try_into().ok()
}

fn iroh_addr(key: [u8; 32]) -> Option<iroh_base::EndpointAddr> {
    iroh_base::PublicKey::from_bytes(&key)
        .ok()
        .map(iroh_base::EndpointAddr::new)
}
