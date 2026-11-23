//! `tilo server`: what this person's servers are, and what they can be asked
//! to do from here.
//!
//! A server of the person's is headless. Its operations are actions on their
//! Servers workspace, which every device of theirs holds, and this command is
//! the device asking: it reads the person's servers from that workspace's
//! document, through this computer's session ([`machines::SERVERS`]), and asks
//! one of them, over the workspace's
//! own session, for its health, a service's journal, a restart, a reboot, an
//! update, a rollback, a drain or making Tilo its default start
//! ([`ServerOperation`]). Every answer is the server's own
//! ([`ServerOperationResult`]), recorded in its entry in that workspace.

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use tilo_actions::remote::machines;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, RemoteActionFrame, RemoteActionMessage, RemoteActionTarget,
    WORKSPACE_GOVERNANCE_PROVIDER, workspace_governance_resource,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_control::front::{self, Spelling};
use tilo_device_identity::persist;
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::servers::{PersonServer, RecoveryKey, person_workspace_id};
use tilo_server_protocol::transport::{
    Envelope, Opening, REMOTE_ACTION_ENVELOPE_VERSION, ServerAnswer, ServerOperation,
    ServerOperationResult, ServerStatus, UnlockMethod,
};
use tilo_user_identity::RevocationList;

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
    /// screen: the vault of your Servers workspace keeps it, and every device
    /// of yours reads it there.
    #[command(
        after_help = "Examples:\n  tilo server recovery-key ryzen\n  tilo server recovery-key ryzen --json"
    )]
    RecoveryKey {
        /// The server: the name it was enrolled under, or its device key.
        server: String,
        /// The key, its machine and its name as one JSON object.
        #[arg(long)]
        json: bool,
    },
    /// Hand this machine's disk recovery key to your devices: the vault of
    /// your Servers workspace keeps it. The installer runs this as soon as the
    /// machine is one of your devices, and keeps no copy.
    #[command(
        after_help = "Example:\n  tilo server keep-recovery-key /run/tilo-install/recovery-key"
    )]
    KeepRecoveryKey {
        /// The file the key is in, never the key itself.
        file: std::path::PathBuf,
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
                ServerCmd::KeepRecoveryKey { file } => return keep_recovery_key(&file),
            };
            let result = ask(&server, &operation)?;
            print(&result, json)?;
            Ok(std::process::ExitCode::SUCCESS)
        }
    }
}

fn list() -> Result<std::process::ExitCode> {
    let servers = servers()?;
    if servers.is_empty() {
        bail!("you have no servers yet; enroll one from your desktop (`tilo device enroll`)");
    }
    let saved = recovery_keys().unwrap_or_default();
    for server in servers {
        let each = saved
            .iter()
            .find(|entry| entry.machine == server.device)
            .map(|_| "\trecovery key saved");
        println!(
            "{}\t{}{}",
            server.label,
            server.device,
            each.unwrap_or_default()
        );
    }
    Ok(std::process::ExitCode::SUCCESS)
}

/// The disk recovery keys of the person's machines, as the vault of their
/// Servers workspace keeps them, read through this computer's session.
fn recovery_keys() -> Result<Vec<RecoveryKey>> {
    let answer = front::call_target(
        &crate::desktop::sockets(),
        machines::target(machines::RECOVERY_KEYS),
        "Read your machines' disk recovery keys",
        &serde_json::json!({}),
        &Spelling::tilo(),
    )
    .map_err(anyhow::Error::msg)?;
    serde_json::from_value(
        answer
            .get("result")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )
    .context("the session's list of your recovery keys does not read")
}

/// The saved recovery key of `server`, as the vault of the person's Servers
/// workspace keeps it.
fn recovery_key(server: &str, json: bool) -> Result<std::process::ExitCode> {
    let asked = server.trim();
    let servers = servers().unwrap_or_default();
    let peer = servers
        .iter()
        .find(|server| server.label == asked || server.device == asked);
    let keys = recovery_keys()?;
    let entry = keys
        .iter()
        .find(|entry| {
            entry.machine == asked || peer.is_some_and(|peer| entry.machine == peer.device)
        })
        .or_else(|| {
            let named = peer.map_or(asked, |peer| peer.label.as_str());
            keys.iter().find(|entry| entry.label == named)
        });
    let Some(entry) = entry else {
        bail!(
            "no recovery key is saved for {asked}: a server keeps one in your Servers workspace \
             as it installs with nobody at its screen"
        );
    };
    if json {
        println!(
            "{}",
            serde_json::json!({
                "machine": entry.machine,
                "label": entry.label,
                "key": entry.key,
            })
        );
    } else {
        println!("{}", entry.label);
        println!("  machine:      {}", entry.machine);
        println!("  recovery key: {}", entry.key);
    }
    Ok(std::process::ExitCode::SUCCESS)
}

/// Hand the disk recovery key in `file` to the person's devices, as this
/// machine, through the host its login enters: the vault of their Servers
/// workspace keeps it there.
fn keep_recovery_key(file: &std::path::Path) -> Result<std::process::ExitCode> {
    let key = std::fs::read_to_string(file)
        .with_context(|| format!("reading the recovery key in {}", file.display()))?;
    let key = key.trim();
    if key.is_empty() {
        bail!("{} holds no recovery key", file.display());
    }
    let place = tilo_device_identity::login::Place::here(identity::paths())?;
    let (signing, device) = tilo_device_identity::login::load(&place)?.ok_or_else(|| {
        anyhow::anyhow!("this machine is not one of your devices yet: run `tilo login` first")
    })?;
    let runtime = crate::login::runtime()?;
    let kept = runtime.block_on(async {
        let entered = crate::login::connect_retrying(&signing, &device).await?;
        let kept = entered
            .host_action(
                machines::KEEP_RECOVERY_KEY,
                serde_json::Map::from_iter([("key".to_owned(), key.into())]),
            )
            .await
            .map_err(|error| anyhow::anyhow!("{error}"));
        entered.close();
        kept
    });
    kept.context("your devices did not keep this machine's recovery key")?;
    println!("Your devices keep this machine's disk recovery key.");
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
    let paths = identity::paths();
    let user = persist::read_public_key(&paths.user_pubkey)
        .context("this computer holds no identity; start `tilo login` first")?;
    let signing_key = persist::load_device_key(&paths)?;
    let cert = persist::read_device_cert(&paths.device_cert_for_user(&user))
        .or_else(|_| persist::read_device_cert(&paths.device_cert))?;
    let found = find(&servers()?, server)?;
    let expected = key_of(&found.device)
        .with_context(|| format!("{} is not a server device key", found.device))?;
    let addr = found.endpoint.with_context(|| {
        format!(
            "{} has not said where it is reached yet; ask again once it has started",
            found.label
        )
    })?;
    let admin = person_workspace_id(&user);
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
        signed: None,
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

/// The person's server named `name`: by its name or its device key, else by
/// the start of its device or service key.
fn find(servers: &[PersonServer], name: &str) -> Result<PersonServer> {
    let name = name.trim();
    let service = |server: &PersonServer| {
        server
            .endpoint
            .as_ref()
            .map(|endpoint| hex::encode(endpoint.id.as_bytes()))
            .unwrap_or_default()
    };
    servers
        .iter()
        .find(|server| server.label == name || server.device == name)
        .or_else(|| {
            servers
                .iter()
                .find(|server| server.device.starts_with(name) || service(server).starts_with(name))
        })
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "you have no server {name:?}; `tilo server list` names the ones you have"
            )
        })
}

/// The person's servers in service, as the document of their workspace
/// names them, read through this computer's session.
fn servers() -> Result<Vec<PersonServer>> {
    let answer = front::call_target(
        &crate::desktop::sockets(),
        machines::target(machines::SERVERS),
        "List your servers",
        &serde_json::json!({}),
        &Spelling::tilo(),
    )
    .map_err(anyhow::Error::msg)?;
    serde_json::from_value(
        answer
            .get("result")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )
    .context("the session's list of your servers does not read")
}

fn key_of(hex_key: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(hex_key).ok()?;
    bytes.try_into().ok()
}
