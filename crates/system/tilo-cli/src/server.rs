//! `tilo server`: what this person's servers are, and what they can be asked
//! to do from here.
//!
//! A server of the person's is headless. Its operations are actions on their
//! Servers workspace, which every device of theirs holds, and this command is
//! the device asking: it reads the servers this computer enrolled
//! ([`home_peer_registry_path`]) and asks one of them, over the workspace's
//! own session, for its health, a service's journal, a restart, a reboot, an
//! update, a rollback, a drain or making Tilo its default start
//! ([`ServerOperation`]). Every answer is the server's own
//! ([`ServerOperationResult`]), recorded in its entry in that workspace.

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, RemoteActionFrame, RemoteActionMessage, RemoteActionTarget,
    WORKSPACE_GOVERNANCE_PROVIDER, workspace_governance_resource,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_device_identity::{IdentityPaths, persist};
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::servers::admin_workspace_id;
use tilo_server_protocol::transport::{
    Envelope, Opening, REMOTE_ACTION_ENVELOPE_VERSION, ServerAnswer, ServerOperation,
    ServerOperationResult, ServerStatus, UnlockMethod,
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
    /// Make Tilo the server's default start, in the retained OS's place after
    /// any network-boot entries; asked again, it restores the order it
    /// wrote.
    SetDefaultStart {
        /// The server.
        server: String,
    },
    /// The disk recovery key of a server whose install had nobody at its
    /// screen: the person's own devices keep it, and this computer is one of
    /// them. Prints it, or whether one is saved with `--json`.
    #[command(
        after_help = "Examples:\n  tilo server recovery-key ryzen\n  tilo server recovery-key ryzen --json"
    )]
    RecoveryKey {
        /// The server: the name it was enrolled under, its device key, or its
        /// machine key.
        server: String,
        /// The saved key and its machine as one JSON object.
        #[arg(long)]
        json: bool,
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
                ServerCmd::SetDefaultStart { server } => {
                    (server, ServerOperation::SetDefaultStart, false)
                }
                ServerCmd::List => return list(),
                ServerCmd::RecoveryKey { server, json } => return recovery_key(&server, json),
            };
            let result = ask(&server, &operation)?;
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
    let saved = saved_recovery_keys().unwrap_or_default();
    for peer in peers {
        let each = saved
            .iter()
            .find(|entry| entry.machine == peer.device_node_id)
            .map(|_| "\trecovery key saved");
        println!(
            "{}\t{}{}",
            peer.label,
            peer.device_node_id,
            each.unwrap_or_default()
        );
    }
    Ok(std::process::ExitCode::SUCCESS)
}

/// The disk recovery keys this computer's person keeps in their own store, as
/// the devices that approved an install wrote them there: nothing when this
/// computer holds none or cannot open the store, which is a store that has
/// not reached it yet.
fn saved_recovery_keys() -> Result<Vec<tilo_session_client::machines::MachineRecovery>> {
    let state = tilo_paths::state_dir().context("this account has no state directory (no HOME)")?;
    let dir = identity::dir()?;
    let paths = IdentityPaths::under(&dir);
    let user = persist::read_public_key(&paths.user_pubkey)
        .context("this computer holds no identity; there is no store to read")?;
    let cert = persist::read_device_cert(&paths.device_cert_for_user(&user))
        .or_else(|_| persist::read_device_cert(&paths.device_cert))
        .context("this computer holds no certificate for its device")?;
    tilo_session_client::machines::saved_recovery_keys(&state, user, cert)
        .map_err(|error| anyhow::anyhow!(error))
}

/// The saved recovery key of `server`: the person's own device keeps it, and
/// this computer is one of them.
fn recovery_key(server: &str, json: bool) -> Result<std::process::ExitCode> {
    let asked = server.trim();
    let peers = peers().unwrap_or_default();
    let peer = peers
        .iter()
        .find(|peer| peer.label == asked || peer.device_node_id == asked);
    let keys = saved_recovery_keys()?;
    let entry = keys
        .iter()
        .find(|entry| {
            entry.machine == asked || peer.is_some_and(|peer| entry.machine == peer.device_node_id)
        })
        .or_else(|| {
            let named = peer.map_or(asked, |peer| peer.label.as_str());
            keys.iter().find(|entry| entry.label == named)
        });
    let Some(entry) = entry else {
        bail!(
            "no recovery key is saved here for {asked}: one of your own devices that approved its \
             install keeps it, and this computer is one of them once their stores have met"
        );
    };
    if json {
        println!(
            "{}",
            serde_json::json!({
                "machine": entry.machine,
                "label": entry.label,
                "key": entry.key,
                "at_ns": entry.at_ns,
            })
        );
    } else {
        println!("{}", entry.label);
        println!("  machine:      {}", entry.machine);
        println!("  recovery key: {}", entry.key);
    }
    Ok(std::process::ExitCode::SUCCESS)
}

fn print(result: &ServerOperationResult, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(result)?);
        return Ok(());
    }
    match &result.answer {
        ServerAnswer::Status(status) => print_status(status),
        ServerAnswer::Text(text) => print!("{text}"),
        ServerAnswer::Nothing => println!("{}", result.summary),
    }
    Ok(())
}

/// What a server is, as its owner reads it.
fn print_status(status: &ServerStatus) {
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
    println!("  opened:        {}", opened_line(status.unlocked.as_ref()));
    println!(
        "  hardware:      {} {} ({} processors, {} MB memory, {} of {} GB free)",
        status.hardware.product,
        status.hardware.cpu_model,
        status.hardware.cpus,
        status.hardware.memory_mb,
        status.hardware.storage_free_gb,
        status.hardware.storage_total_gb
    );
    print_holds(status);
}

/// How this boot opened its volume, as a person reads it.
fn opened_line(opened: Option<&Opening>) -> String {
    let Some(opened) = opened else {
        return "early boot left no note of how it opened".to_owned();
    };
    format!(
        "{} at {}",
        match opened.method {
            UnlockMethod::Tpm => "by the TPM",
            UnlockMethod::SiblingServer => {
                "by another server of yours, on this machine's attested boot chain"
            }
            UnlockMethod::PersonDevice => "by you, on one of your devices",
            UnlockMethod::Passphrase => "with a passphrase",
            UnlockMethod::RecoveryKey => "with the recovery key",
        },
        opened_at(opened.at_ns)
    )
}

/// What the server holds, and what was asked of it.
fn print_holds(status: &ServerStatus) {
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

/// `at` in nanoseconds since the Unix epoch, as a person reads it.
fn opened_at(at_ns: u64) -> String {
    let seconds = i64::try_from(at_ns / 1_000_000_000).unwrap_or(i64::MAX);
    let nanos = i32::try_from(at_ns % 1_000_000_000).unwrap_or(0);
    jiff::Timestamp::new(seconds, nanos)
        .map(|at| at.to_string())
        .unwrap_or_default()
}

/// Ask `server` to do `operation`, as this computer's device, through the
/// action on the person's Servers workspace that asks it.
fn ask(server: &str, operation: &ServerOperation) -> Result<ServerOperationResult> {
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
    let admin = admin_workspace_id(&user);
    let workspace_id = WorkspaceId::parse(&admin).context("the Servers workspace is not an id")?;
    let mut argument = serde_json::Map::new();
    argument.insert("server".into(), hex::encode(expected).into());
    if !operation.service().is_empty() {
        argument.insert("service".into(), operation.service().into());
    }
    if operation.lines() > 0 {
        argument.insert("lines".into(), operation.lines().into());
    }
    let message = RemoteActionMessage::Invoke(InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: WORKSPACE_GOVERNANCE_PROVIDER.into(),
            address: ActionAddress::new(
                workspace_governance_resource(&admin),
                format!("server.{}", operation.name()),
            ),
        },
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    });
    let frame = RemoteActionFrame::new(message).encode()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
        let dialer = tilo_server_client::host::ServerAddress::at(addr.clone());
        let endpoint = dialer
            .bind(None)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let client = tilo_server_session::client::ServerClient::connect_to_pinned_host(
            endpoint.clone(),
            addr,
            cert,
            signing_key,
            RevocationList::new(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
        client
            .send(Envelope::RemoteAction {
                workspace_id,
                version: REMOTE_ACTION_ENVELOPE_VERSION,
                to_device_public_key: expected,
                from_device_public_key: [0; 32],
                from_user_public_key: [0; 32],
                frame,
            })
            .map_err(|_| anyhow::anyhow!("the session closed before the server was asked"))?;
        let answer = tokio::time::timeout(std::time::Duration::from_secs(120), async {
            loop {
                match client.recv().await {
                    Some(Envelope::RemoteAction { frame, .. }) => {
                        let message = RemoteActionFrame::decode(&frame)
                            .map_err(|error| anyhow::anyhow!("{error}"))?
                            .message;
                        return answer_of(message);
                    }
                    Some(Envelope::RemoteActionUndeliverable { refusal, .. }) => {
                        return Err(anyhow::anyhow!("the server did not take it: {refusal}"));
                    }
                    None => return Err(anyhow::anyhow!("the session closed")),
                    Some(_) => {}
                }
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("the server did not answer"))?;
        client.close().await;
        endpoint.close().await;
        answer
    })?;
    Ok(result)
}

/// What an invocation came to: the operation's result, or why the server
/// refused.
fn answer_of(message: RemoteActionMessage) -> Result<ServerOperationResult> {
    use tilo_actions::remote::ReceiptStatus;
    let RemoteActionMessage::Receipt(receipt) = message else {
        bail!("the server answered no receipt");
    };
    match receipt.status {
        ReceiptStatus::Completed { output } => serde_json::from_value(
            output
                .value
                .ok_or_else(|| anyhow::anyhow!("the server answered nothing"))?,
        )
        .context("the server's answer does not read"),
        ReceiptStatus::Rejected { error } => Err(anyhow::anyhow!("{error}")),
        other => bail!("the server answered {other:?}"),
    }
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
