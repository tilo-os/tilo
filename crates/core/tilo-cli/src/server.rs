//! `tilo server`: what this person's servers are, and what they can be asked
//! to do from here.
//!
//! A server of the person's is headless. This command reads the person's
//! servers from their Servers workspace's document, through this computer's
//! daemon ([`machines::SERVERS`]), and asks one of them for its health, a
//! service's journal, a restart, a reboot, an update, a rollback, a drain or
//! making Tilo its default start: the machine's own `server` device actions,
//! which this computer's daemon hands to that server's, as it hands every
//! device action naming another of the person's devices. Every answer is the
//! server's own ([`ServerOperationResult`]).

use anyhow::{Context as _, Result, bail};
use clap::Subcommand;
use serde_json::{Map, Value};
use tilo_actions::remote::machines;
use tilo_server_protocol::servers::{PersonServer, RecoveryKey};
use tilo_server_protocol::transport::{
    Opening, ServerAnswer, ServerOperationResult, ServerStatus, UnlockMethod,
};

use crate::api::{Calls, Reach};
use crate::login::{self, Device};

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
    let unit = |service: String| Map::from_iter([("service".to_owned(), Value::String(service))]);
    let (server, action, args, json) = match cmd {
        ServerCmd::List => return list(),
        ServerCmd::RecoveryKey { server, json } => return recovery_key(&server, json),
        ServerCmd::KeepRecoveryKey { file } => return keep_recovery_key(&file),
        ServerCmd::Status { server, json } => (server, "status", Map::new(), json),
        ServerCmd::Logs {
            server,
            service,
            lines,
        } => {
            let mut args = unit(service);
            args.insert("lines".to_owned(), lines.into());
            (server, "journal", args, false)
        }
        ServerCmd::Restart { server, service } => (server, "restart", unit(service), false),
        ServerCmd::Reboot { server } => (server, "reboot", Map::new(), false),
        ServerCmd::Update { server } => (server, "stage-update", Map::new(), false),
        ServerCmd::PauseUpdates { server } => (server, "pause-updates", Map::new(), false),
        ServerCmd::ResumeUpdates { server } => (server, "resume-updates", Map::new(), false),
        ServerCmd::Rollback { server } => (server, "rollback", Map::new(), false),
        ServerCmd::Drain { server } => (server, "drain", Map::new(), false),
        ServerCmd::SetDefaultStart { server } => (server, "set-default-start", Map::new(), false),
    };
    let result = ask(&server, action, args)?;
    print(&result, json)?;
    Ok(std::process::ExitCode::SUCCESS)
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
/// Servers workspace keeps them, read through this computer's daemon.
fn recovery_keys() -> Result<Vec<RecoveryKey>> {
    serde_json::from_value(read(machines::RECOVERY_KEYS)?)
        .context("the daemon's list of your recovery keys does not read")
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
    let device = crate::login::home().ok_or_else(|| {
        anyhow::anyhow!("this machine is not one of your devices yet: run `tilo login` first")
    })?;
    // This machine signs as its device through its daemon: the key never enters this process.
    let signer = std::sync::Arc::new(tilo_identity::daemon_signer::DaemonSigner::here(
        crate::identity::paths(),
    )?);
    let runtime = crate::login::runtime()?;
    let kept = runtime.block_on(async {
        let entered = crate::login::connect_retrying(signer, &device).await?;
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

/// What the server holds.
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
}

/// `at` in nanoseconds since the Unix epoch, as a person reads it.
fn opened_at(at_ns: u64) -> String {
    let seconds = i64::try_from(at_ns / 1_000_000_000).unwrap_or(i64::MAX);
    let nanos = i32::try_from(at_ns % 1_000_000_000).unwrap_or(0);
    jiff::Timestamp::new(seconds, nanos)
        .map(|at| at.to_string())
        .unwrap_or_default()
}

/// Ask `server` the control `action` with `args`, through this computer's
/// daemon, which hands it to that server's: the server's own answer.
fn ask(server: &str, action: &str, mut args: Map<String, Value>) -> Result<ServerOperationResult> {
    let found = find(&servers()?, server)?;
    args.insert("device".to_owned(), found.device.clone().into());
    // Where the server is reached, as its entry says: a server is no device a lookup by its
    // key alone always finds.
    if let Some(endpoint) = &found.endpoint {
        args.insert("at".to_owned(), serde_json::to_string(endpoint)?.into());
    }
    let here = login::home().filter(Device::reaches_a_server);
    let daemon = login::ensure_daemon()?;
    let runtime = login::runtime()?;
    let answer = runtime.block_on(async {
        let client = Calls::open_with(Reach::default(), || Ok(daemon)).await?;
        let workspace = match &here {
            Some(device) => device.workspace.clone(),
            None => client.founded_workspace().await?,
        };
        client
            .act(
                Some(&workspace),
                "server",
                action,
                &Value::Object(args),
                &format!("Server: {action}"),
            )
            .await
    })?;
    let result = answer
        .get("result")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("{} answered nothing: {answer}", found.label))?;
    serde_json::from_value(result).context("the server's answer does not read")
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
/// names them, read through this computer's daemon.
fn servers() -> Result<Vec<PersonServer>> {
    serde_json::from_value(read(machines::SERVERS)?)
        .context("the daemon's list of your servers does not read")
}

/// What the person's Servers workspace answers the machine step `step`, as this computer's
/// daemon reads it: the read a desktop makes, so a computer with none answers it as well.
fn read(step: &str) -> Result<Value> {
    let daemon = login::ensure_daemon()?;
    login::runtime()?.block_on(async {
        Calls::open_with(Reach::default(), || Ok(daemon))
            .await?
            .machines(step, Map::new())
            .await
    })
}
