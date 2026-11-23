//! `tilo-server-admin` — thin CLI to revoke devices and inspect the running
//! daemon by reading its on-disk state.
//!
//! Mutations are durable file updates which a running daemon reloads.
//! `status` combines configuration, workspace state, and the daemon's
//! periodically persisted metrics snapshot.

mod files_setup;

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};

use tilo_server::ServerConfig;

const DEFAULT_CONFIG_PATH: &str = "/etc/tilo/server.toml";

#[derive(Parser)]
#[command(
    name = "tilo-server-admin",
    version,
    about = "Administer a tilo-server daemon"
)]
struct Cli {
    /// Path to the server TOML config.
    #[arg(long, global = true, default_value = DEFAULT_CONFIG_PATH)]
    config: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Check that the primary Files mount starts as this device's existing certified identity.
    FilesWorkspaceCheck {
        #[arg(long)]
        identity_root: PathBuf,
    },
    /// Print daemon status (config + on-disk state summary).
    Status,
    /// Print the endpoint id a server's device key answers on, from the key file it keeps
    /// (`host-device.key`): the device key's own public key.
    NodeId {
        /// The server's raw 32-byte device key.
        #[arg(long)]
        key_file: PathBuf,
    },
    /// Ask the running daemon about an OS update, as the update service does.
    /// Exits 0 when it may go ahead, 75 when it has to wait.
    Rollout {
        #[command(subcommand)]
        ask: RolloutAsk,
    },
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
        /// The person's identity directory, whose active identity holds the
        /// root that signs; this account's own when left out.
        #[arg(long)]
        identity_dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum RolloutAsk {
    /// Whether this machine may restart into the staged image TARGET now;
    /// the daemon takes its person's update lease and drains before it says so.
    Prepare { target: String },
    /// This machine booted IMAGE and passed its health verdict; the update lease
    /// goes to the person's next server.
    Booted { image: String },
    /// After a healthy boot of GENERATION (IMAGE, from UPDATES): ask the
    /// person's other devices to approve this machine's rollback floor moving,
    /// and keep the approvals they signed in APPROVALS. Exits 0 once some are
    /// kept, 75 while none are.
    Floor {
        generation: u64,
        image: String,
        updates: String,
        /// The public key (PEM) this machine's floor counter takes approvals from.
        #[arg(long)]
        authority: PathBuf,
        /// What `tilo-rollback floor-request` stated.
        #[arg(long)]
        request: PathBuf,
        /// Where `tilo-rollback confirm` finds approvals.
        #[arg(long)]
        approvals: PathBuf,
    },
}

fn load(path: &std::path::Path) -> anyhow::Result<ServerConfig> {
    if path.exists() {
        Ok(ServerConfig::load(path).with_context(|| format!("load {}", path.display()))?)
    } else {
        Ok(ServerConfig::default())
    }
}

fn device_key(value: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = hex::decode(value.trim()).with_context(|| "device pubkey must be hex")?;
    bytes.try_into().map_err(|bytes: Vec<u8>| {
        anyhow::anyhow!("device pubkey must be 32 bytes; got {}", bytes.len())
    })
}

fn main() -> anyhow::Result<std::process::ExitCode> {
    // Automerge isolation workers re-enter this binary; serve them first.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return Ok(std::process::ExitCode::from(code));
    }
    dispatch()
}

fn dispatch() -> anyhow::Result<std::process::ExitCode> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Rollout { ask } => cmd_rollout(&cli.config, ask),
        cmd => command(&cli.config, cmd).map(|()| std::process::ExitCode::SUCCESS),
    }
}

/// Ask the daemon about an update and say what it answered: 0 to go ahead,
/// 75 to wait.
fn cmd_rollout(
    config: &std::path::Path,
    ask: RolloutAsk,
) -> anyhow::Result<std::process::ExitCode> {
    use tilo_server::rollout::{Answer, Request};
    let cfg = load(config)?;
    let mut keep = None;
    let request = match ask {
        RolloutAsk::Prepare { target } => Request::Prepare { target },
        RolloutAsk::Booted { image } => Request::Booted { image },
        RolloutAsk::Floor {
            generation,
            image,
            updates,
            authority,
            request,
            approvals,
        } => {
            let read = |path: &std::path::Path| {
                std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))
            };
            keep = Some(approvals);
            Request::Floor(tilo_server::servers::floor::Asked {
                generation,
                image,
                updates,
                authority: read(&authority)?,
                request: read(&request)?,
            })
        }
    };
    let answer = match tilo_server::rollout::ask(&cfg.state_root, &request) {
        Ok(answer) => answer,
        Err(why) => Answer::Wait { why },
    };
    let (said, code) = match &answer {
        Answer::Restart => (
            "restart: this server holds the update lease and has drained".to_owned(),
            0,
        ),
        Answer::Wait { why } => (format!("wait: {why}"), 75),
        Answer::Released => (
            "released: the update booted healthy; the next server may update".to_owned(),
            0,
        ),
        Answer::Halted { target, booted } => (
            format!(
                "halted: {target} did not boot (running {booted}); this server keeps the update lease so no other server starts it"
            ),
            0,
        ),
        Answer::Nothing => ("nothing: no update was under way".to_owned(), 0),
        Answer::Floor { approvals, why } if approvals.is_empty() => (format!("floor: {why}"), 75),
        Answer::Floor { approvals, .. } => {
            let dir = keep.context("approvals answer only a floor request")?;
            tilo_rollback::approval::store(&dir, approvals)?;
            (
                format!(
                    "floor: kept {} approval(s) another of the person's devices signed",
                    approvals.len()
                ),
                0,
            )
        }
    };
    println!("{said}");
    Ok(std::process::ExitCode::from(code))
}

fn command(config: &std::path::Path, cmd: Cmd) -> anyhow::Result<()> {
    match cmd {
        Cmd::FilesWorkspaceCheck { identity_root } => files_setup::check(&identity_root),
        Cmd::Status => cmd_status(config),
        Cmd::NodeId { key_file } => {
            let bytes = std::fs::read(&key_file)
                .map_err(|error| anyhow::anyhow!("read {}: {error}", key_file.display()))?;
            let key = <[u8; 32]>::try_from(bytes.as_slice())
                .map_err(|_| anyhow::anyhow!("{}: a device key is 32 bytes", key_file.display()))?;
            let key = ed25519_dalek::SigningKey::from_bytes(&key);
            println!("{}", hex::encode(key.verifying_key().to_bytes()));
            Ok(())
        }
        Cmd::RevokeDevice {
            device_pubkey_hex,
            reason,
            identity_dir,
        } => cmd_revoke_device(
            config,
            &device_pubkey_hex,
            &reason,
            &identity_dir_or_own(identity_dir)?,
        ),
        Cmd::Rollout { ask } => cmd_rollout(config, ask).map(drop),
    }
}

fn cmd_status(config: &std::path::Path) -> anyhow::Result<()> {
    let cfg = load(config)?;
    println!("tilo-server-admin {}", tilo_server::VERSION);
    println!("config:    {}", config.display());
    println!("state:     {}", cfg.state_root.display());
    println!("identity:  {}", cfg.identity_root.display());
    println!("iroh:      {}", cfg.iroh_listen);
    let workspace_root = cfg.state_root.join("workspaces");
    let mut durable: Vec<_> = std::fs::read_dir(&workspace_root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            tilo_server_protocol::WorkspaceId::parse(&entry.file_name().to_string_lossy())
        })
        .collect();
    durable.sort_unstable();
    durable.dedup();
    println!("durable workspace shards: {}", durable.len());
    for workspace_id in durable {
        let files =
            tilo_server::workspace_files::WorkspaceFiles::open(&cfg.state_root, workspace_id)?;
        let members = files.load_membership()?.len();
        println!("  {workspace_id}\tmembers={members}");
    }
    if let Some(metrics) = tilo_server::workspace_files::load_metrics(&cfg.state_root)? {
        println!("metrics:");
        println!("  connections accepted: {}", metrics.conns_accepted);
        println!("  connections authed:   {}", metrics.conns_authed);
        println!("  auth rejected:        {}", metrics.auth_rejected);
        println!("  deltas received:      {}", metrics.deltas_received);
        println!("  deltas fanned out:    {}", metrics.deltas_fanned_out);
        println!("  bytes queued:         {}", metrics.bytes_queued);
        println!("  bytes sent:           {}", metrics.bytes_sent);
        println!("  bytes sent live:      {}", metrics.bytes_sent_live);
        for (workspace_id, egress) in &metrics.workspaces {
            println!(
                "  {workspace_id}\tbytes sent={}\tlive={}",
                egress.bytes_sent, egress.bytes_sent_live
            );
        }
    }
    Ok(())
}

/// The person's identity directory `--identity-dir` names, else this account's own: the one every
/// reader of their records and their device list uses.
fn identity_dir_or_own(named: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    match named {
        Some(dir) => Ok(dir),
        None => Ok(tilo_identity::store::identity_dir_from_env()?),
    }
}

fn cmd_revoke_device(
    config: &std::path::Path,
    device_pubkey_hex: &str,
    reason: &str,
    identity_dir: &std::path::Path,
) -> anyhow::Result<()> {
    let cfg = load(config)?;
    let pk = device_key(device_pubkey_hex)?;
    let reason = match reason {
        "lost" => tilo_user_identity::RevocationReason::Lost,
        "stolen" => tilo_user_identity::RevocationReason::Stolen,
        "retired" => tilo_user_identity::RevocationReason::Retired,
        "compromised" => tilo_user_identity::RevocationReason::Compromised,
        _ => anyhow::bail!("reason must be lost, stolen, retired, or compromised"),
    };
    let root = tilo_server::root_signer::load_root(Some(identity_dir))?;
    tilo_server::root_signer::revoke_device(&cfg.state_root, root.as_ref(), pk, reason)?;
    println!("revoked device {} ({reason:?})", hex::encode(pk));
    println!("a running tilo-server disconnects the device within one second");
    Ok(())
}
