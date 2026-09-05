//! `qos-registry-server` CLI — bind a port, add allowlisted
//! publishers, print the identity, or bootstrap a fresh instance.
//!
//! ```text
//! qos-registry-server serve --port 8443 --identity <path>
//! qos-registry-server add-publisher --pubkey <hex> [--can-add-new] [--note ...]
//! qos-registry-server remove-publisher --pubkey <hex>
//! qos-registry-server list-publishers
//! qos-registry-server init --owner-pubkey <hex> --name <n> --description <d> --base-url <url>
//! ```

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing::info;
use tracing_subscriber::EnvFilter;

use registry_server::{
    AppState, InspectionPolicy, ModerationService, PublisherAllowlist, PublisherRow,
    ServerIdentity, Storage, default_data_dir, inspect_entry, run_server,
};
use registry_types::EntryAction;
use registry_types::{PublisherAuthorization, SNAPSHOT_PROTOCOL, SignedRegistrySnapshot};

#[derive(Debug, Parser)]
#[command(
    name = "qos-registry-server",
    about = "Self-hostable qOS app registry."
)]
struct Cli {
    /// Location of the on-disk state. Defaults to
    /// `$XDG_STATE_HOME/qos-registry` (see `default_data_dir`).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Bind an HTTP listener and serve until ctrl-c. TLS is out
    /// of scope — put nginx or Caddy in front for public deployments.
    Serve {
        /// Port to bind to. Bind is on `0.0.0.0` by default; use
        /// `--bind` to restrict.
        #[arg(long, default_value_t = 8443)]
        port: u16,
        /// IPv4/v6 address to bind. Loopback by default so a
        /// misconfigured server can't accidentally serve the
        /// world before `--identity` is set.
        #[arg(long, default_value = "127.0.0.1")]
        bind: IpAddr,
        /// Path to the registry owner's identity metadata. The pinned public
        /// key authenticates reviewer decisions as well as the handshake;
        /// private signing remains in the reviewer's identity service.
        #[arg(long)]
        identity: Option<PathBuf>,
    },
    /// Bootstrap a fresh data-dir with a `handshake.toml`.
    /// Idempotent — writing over an existing file is not the
    /// intended flow; use `--overwrite` explicitly.
    Init {
        /// Owner Ed25519 pubkey (32-byte hex). Anchors the trust
        /// chain — clients will pin this and refuse to install
        /// entries the current owner didn't allowlist.
        #[arg(long)]
        owner_pubkey: String,
        /// Human-readable name.
        #[arg(long)]
        name: String,
        /// One-line description.
        #[arg(long)]
        description: String,
        /// Public base URL — used by clients to compose full
        /// download URLs.
        #[arg(long)]
        base_url: String,
        /// Overwrite an existing handshake.toml instead of erroring.
        #[arg(long)]
        overwrite: bool,
    },
    /// Add or replace a row in `publishers.toml`.
    AddPublisher {
        /// Publisher's Ed25519 pubkey (32-byte hex).
        #[arg(long)]
        pubkey: String,
        /// Allow this publisher to POST new entries; without
        /// this flag they can only edit their own existing entries.
        #[arg(long)]
        can_add_new: bool,
        /// Free-form note visible to the operator only.
        #[arg(long, default_value = "")]
        note: String,
    },
    /// Remove a publisher from the allowlist. Existing entries
    /// they authored stay in place; the moderator can delete them
    /// separately.
    RemovePublisher {
        /// Publisher's Ed25519 pubkey (32-byte hex).
        #[arg(long)]
        pubkey: String,
    },
    /// Print the current `publishers.toml` to stdout.
    ListPublishers,
    /// Build and owner-sign the next immutable public snapshot. Run this in
    /// the offline signing environment; `serve` never reads this key.
    SignSnapshot {
        /// File containing exactly a 32-byte Ed25519 secret seed as 64 hex
        /// characters. On Unix it must not be accessible by group/others.
        #[arg(long)]
        owner_key_file: PathBuf,
        /// Snapshot lifetime. Short validity bounds freeze attacks.
        #[arg(long, default_value_t = 86_400)]
        valid_for_seconds: u64,
    },
    /// Review signed publisher changes before they enter the public catalogue.
    Review {
        #[command(subcommand)]
        command: ReviewCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ReviewCommand {
    /// List submissions waiting for a decision.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Verify package bytes and show the exact signed request and capabilities.
    Show {
        package_id: String,
        #[arg(long)]
        json: bool,
        #[arg(long, hide = true)]
        allow_insecure_local: bool,
    },
    /// Verify package bytes and publish the exact signed request.
    Approve {
        package_id: String,
        #[arg(long)]
        reviewer: String,
        #[arg(long)]
        reason: String,
        #[arg(long, hide = true)]
        allow_insecure_local: bool,
    },
    /// Reject a request while retaining an audit record.
    Reject {
        package_id: String,
        #[arg(long)]
        reviewer: String,
        #[arg(long)]
        reason: String,
    },
    /// Immediately hide a public entry while retaining a recoverable copy.
    Quarantine {
        package_id: String,
        #[arg(long)]
        reviewer: String,
        #[arg(long)]
        reason: String,
    },
    /// Restore a quarantined entry after current trust checks pass.
    Restore {
        package_id: String,
        #[arg(long)]
        reviewer: String,
        #[arg(long)]
        reason: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let data_dir = cli.data_dir.unwrap_or_else(default_data_dir);
    let storage = Storage::open(&data_dir).map_err(|e| anyhow::anyhow!("open storage: {e}"))?;

    match cli.cmd {
        Cmd::Serve {
            port,
            bind,
            identity: _,
        } => cmd_serve(storage, bind, port).await,
        Cmd::Init {
            owner_pubkey,
            name,
            description,
            base_url,
            overwrite,
        } => cmd_init(
            &storage,
            &owner_pubkey,
            name,
            description,
            base_url,
            overwrite,
        ),
        Cmd::AddPublisher {
            pubkey,
            can_add_new,
            note,
        } => cmd_add_publisher(&storage, &pubkey, can_add_new, note),
        Cmd::RemovePublisher { pubkey } => cmd_remove_publisher(&storage, &pubkey),
        Cmd::ListPublishers => cmd_list_publishers(&storage),
        Cmd::SignSnapshot {
            owner_key_file,
            valid_for_seconds,
        } => cmd_sign_snapshot(&storage, &owner_key_file, valid_for_seconds),
        Cmd::Review { command } => cmd_review(&storage, command),
    }
}

fn cmd_sign_snapshot(
    storage: &Storage,
    owner_key_file: &std::path::Path,
    valid_for_seconds: u64,
) -> anyhow::Result<()> {
    if valid_for_seconds == 0 || valid_for_seconds > registry_types::MAX_SNAPSHOT_VALIDITY_SECONDS {
        anyhow::bail!("snapshot validity must be between 1 second and 7 days");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let mode = std::fs::metadata(owner_key_file)?.mode() & 0o777;
        if mode & 0o077 != 0 {
            anyhow::bail!("owner key file must not be accessible by group or others");
        }
    }
    let key_hex =
        zeroize::Zeroizing::new(std::fs::read_to_string(owner_key_file)?.trim().to_owned());
    let seed: zeroize::Zeroizing<[u8; 32]> = zeroize::Zeroizing::new(
        hex::decode(key_hex.as_str())
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| anyhow::anyhow!("owner key file must contain a 32-byte hex seed"))?,
    );
    let key = ed25519_dalek::SigningKey::from_bytes(&seed);
    let identity = storage.load_identity()?;
    let actual_owner = hex::encode(key.verifying_key().to_bytes());
    if !actual_owner.eq_ignore_ascii_case(&identity.owner_pubkey_hex) {
        anyhow::bail!("owner private key does not match the pinned registry identity");
    }
    let previous = storage.load_snapshot()?;
    if let Some(snapshot) = &previous {
        registry_types::verify_snapshot(
            snapshot,
            &identity.owner_pubkey_hex,
            snapshot.issued_at_unix,
        )?;
    }
    let (sequence, previous_snapshot_hash_hex) = previous.as_ref().map_or_else(
        || Ok((1, String::new())),
        |snapshot| {
            Ok::<_, registry_types::SnapshotError>((
                snapshot.sequence.saturating_add(1),
                registry_types::snapshot_hash(snapshot)?,
            ))
        },
    )?;
    let allowlist = storage.load_allowlist()?;
    let mut publishers: Vec<_> = allowlist
        .publishers
        .into_iter()
        .map(|row| PublisherAuthorization {
            pubkey_hex: row.pubkey_hex.to_ascii_lowercase(),
            can_add_new: row.can_add_new,
        })
        .collect();
    publishers.sort_by(|left, right| left.pubkey_hex.cmp(&right.pubkey_hex));
    let mut entries = storage.load_entries()?;
    entries.sort_by(|left, right| left.package_id.cmp(&right.package_id));
    let issued_at_unix = now_unix();
    let mut snapshot = SignedRegistrySnapshot {
        protocol: SNAPSHOT_PROTOCOL,
        sequence,
        issued_at_unix,
        expires_at_unix: issued_at_unix.saturating_add(valid_for_seconds),
        previous_snapshot_hash_hex,
        moderation_head_hex: storage.moderation_head()?,
        publishers,
        entries,
        owner_signature_hex: String::new(),
    };
    registry_types::sign_snapshot(&mut snapshot, &key)?;
    registry_types::verify_snapshot(&snapshot, &identity.owner_pubkey_hex, issued_at_unix)?;
    storage.save_snapshot(&snapshot)?;
    println!("{}", serde_json::to_string_pretty(&snapshot)?);
    Ok(())
}

fn cmd_review(storage: &Storage, command: ReviewCommand) -> anyhow::Result<()> {
    let moderation = ModerationService::new(storage.clone());
    match command {
        ReviewCommand::List { json } => {
            let mut pending = moderation.pending_submissions()?;
            pending.sort_by(|left, right| left.submission_id.cmp(&right.submission_id));
            if json {
                println!("{}", serde_json::to_string_pretty(&pending)?);
            } else if pending.is_empty() {
                println!("No submissions waiting for review.");
            } else {
                for item in pending {
                    println!(
                        "{}\t{:?}\t{}\t{}",
                        item.submission_id,
                        item.change.action,
                        item.change.entry.name,
                        item.change.entry.publisher_pubkey_hex
                    );
                }
            }
        }
        ReviewCommand::Show {
            package_id,
            json,
            allow_insecure_local,
        } => {
            let pending = require_pending(&moderation, &package_id)?;
            let inspection = inspect_for_decision(&pending, allow_insecure_local)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "submission": pending,
                        "verified_packages": inspection,
                    }))?
                );
            } else {
                println!(
                    "{}: {:?} {} ({})",
                    pending.submission_id,
                    pending.change.action,
                    pending.change.entry.name,
                    pending.change.entry.publisher_pubkey_hex
                );
                for version in inspection {
                    println!("\n{} · {} bytes", version.version, version.size_bytes);
                    println!("{}", serde_json::to_string_pretty(&version.sandbox)?);
                }
            }
        }
        ReviewCommand::Approve {
            package_id,
            reviewer,
            reason,
            allow_insecure_local,
        } => {
            let pending = require_pending(&moderation, &package_id)?;
            let _inspection = inspect_for_decision(&pending, allow_insecure_local)?;
            let allowlist = storage.load_allowlist()?;
            let record = moderation.approve(&pending, reviewer, reason, now_unix(), &allowlist)?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        ReviewCommand::Reject {
            package_id,
            reviewer,
            reason,
        } => {
            let record = moderation.reject(&package_id, reviewer, reason, now_unix())?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        ReviewCommand::Quarantine {
            package_id,
            reviewer,
            reason,
        } => {
            let record = moderation.quarantine(&package_id, reviewer, reason, now_unix())?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        ReviewCommand::Restore {
            package_id,
            reviewer,
            reason,
        } => {
            let allowlist = storage.load_allowlist()?;
            let record = moderation.restore_quarantined(
                &package_id,
                reviewer,
                reason,
                now_unix(),
                &allowlist,
            )?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
    }
    Ok(())
}

fn require_pending(
    moderation: &ModerationService,
    package_id: &str,
) -> anyhow::Result<registry_server::PendingSubmission> {
    moderation
        .pending(package_id)?
        .ok_or_else(|| anyhow::anyhow!("no pending submission for {package_id}"))
}

fn inspect_for_decision(
    pending: &registry_server::PendingSubmission,
    allow_insecure_local: bool,
) -> anyhow::Result<Vec<registry_server::InspectedVersion>> {
    if pending.change.action == EntryAction::Delete {
        return Ok(Vec::new());
    }
    let policy = InspectionPolicy {
        allow_insecure_local,
        ..InspectionPolicy::default()
    };
    inspect_entry(&pending.change.entry, policy).map_err(Into::into)
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// `Cmd::Serve` — load the identity + allowlist and serve until ctrl-c.
async fn cmd_serve(storage: Storage, bind: IpAddr, port: u16) -> anyhow::Result<()> {
    let ident: ServerIdentity = storage
        .load_identity()
        .map_err(|e| anyhow::anyhow!("load identity: {e}"))?;
    if ident.owner_pubkey_hex.is_empty() {
        anyhow::bail!(
            "no handshake.toml at {} — run `qos-registry-server init …` first",
            storage.handshake_path().display()
        );
    }
    let allow = storage
        .load_allowlist()
        .map_err(|e| anyhow::anyhow!("load allowlist: {e}"))?;
    info!(
        "loaded {} publisher(s) from {}",
        allow.publishers.len(),
        storage.publishers_path().display()
    );
    let state = AppState::new(storage, ident);
    let addr = SocketAddr::new(bind, port);
    run_server(addr, state).await?;
    Ok(())
}

/// `Cmd::Init` — bootstrap a fresh `handshake.toml`.
fn cmd_init(
    storage: &Storage,
    owner_pubkey: &str,
    name: String,
    description: String,
    base_url: String,
    overwrite: bool,
) -> anyhow::Result<()> {
    if storage.handshake_path().exists() && !overwrite {
        anyhow::bail!(
            "{} already exists — pass --overwrite to replace",
            storage.handshake_path().display()
        );
    }
    let owner_bytes: [u8; 32] = hex::decode(owner_pubkey)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .filter(|bytes| *bytes != [0; 32])
        .ok_or_else(|| anyhow::anyhow!("owner key must be a non-placeholder 32-byte hex key"))?;
    ed25519_dalek::VerifyingKey::from_bytes(&owner_bytes)
        .map_err(|_| anyhow::anyhow!("owner key is not a valid Ed25519 public key"))?;
    let ident = ServerIdentity {
        owner_pubkey_hex: owner_pubkey.to_ascii_lowercase(),
        name,
        description,
        base_url,
    };
    storage
        .save_identity(&ident)
        .map_err(|e| anyhow::anyhow!("save identity: {e}"))?;
    info!(
        "wrote handshake to {} — start the server with `qos-registry-server serve`",
        storage.handshake_path().display()
    );
    Ok(())
}

/// `Cmd::AddPublisher` — add or replace one allowlist row.
fn cmd_add_publisher(
    storage: &Storage,
    pubkey: &str,
    can_add_new: bool,
    note: String,
) -> anyhow::Result<()> {
    let mut allow: PublisherAllowlist = storage
        .load_allowlist()
        .map_err(|e| anyhow::anyhow!("load allowlist: {e}"))?;
    let row = PublisherRow {
        pubkey_hex: pubkey.to_string(),
        can_add_new,
        note,
    };
    let bytes = row.pubkey_bytes()?;
    if bytes == [0; 32] || ed25519_dalek::VerifyingKey::from_bytes(&bytes).is_err() {
        anyhow::bail!("publisher key must be a valid non-placeholder Ed25519 public key");
    }
    let replaced = allow.upsert(row);
    storage
        .save_allowlist(&allow)
        .map_err(|e| anyhow::anyhow!("save allowlist: {e}"))?;
    if replaced {
        info!("replaced publisher {pubkey}");
    } else {
        info!("added publisher {pubkey}");
    }
    Ok(())
}

/// `Cmd::RemovePublisher` — drop one allowlist row.
fn cmd_remove_publisher(storage: &Storage, pubkey: &str) -> anyhow::Result<()> {
    let mut allow: PublisherAllowlist = storage
        .load_allowlist()
        .map_err(|e| anyhow::anyhow!("load allowlist: {e}"))?;
    if allow.remove(pubkey) {
        storage
            .save_allowlist(&allow)
            .map_err(|e| anyhow::anyhow!("save allowlist: {e}"))?;
        info!("removed publisher {pubkey}");
        Ok(())
    } else {
        anyhow::bail!("no publisher with pubkey {pubkey}");
    }
}

/// `Cmd::ListPublishers` — print `publishers.toml` to stdout.
fn cmd_list_publishers(storage: &Storage) -> anyhow::Result<()> {
    let allow: PublisherAllowlist = storage
        .load_allowlist()
        .map_err(|e| anyhow::anyhow!("load allowlist: {e}"))?;
    let s = allow
        .to_toml()
        .map_err(|e| anyhow::anyhow!("encode allowlist: {e}"))?;
    print!("{s}");
    Ok(())
}
