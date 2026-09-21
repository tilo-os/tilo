//! `tilo-registry-server` CLI — bind a port, add allowlisted
//! publishers, print the identity, or bootstrap a fresh instance.
//!
//! ```text
//! tilo-registry-server serve --port 8443 --identity <path>
//! tilo-registry-server add-publisher --pubkey <hex> [--can-add-new] [--note ...]
//! tilo-registry-server remove-publisher --pubkey <hex>
//! tilo-registry-server list-publishers
//! tilo-registry-server init --owner-pubkey <hex> --name <n> --description <d> --base-url <url>
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
    AppState, InspectionPolicy, ModerationService, PromotionJournal, PublisherAllowlist,
    PublisherRow, ServerIdentity, StagedArtifact, StagedPromotion, Storage, default_data_dir,
    fetch_entry_bytes, inspect_bytes, inspect_entry, run_server,
};
use registry_types::EntryAction;
use registry_types::{
    CHANNEL_AUTHORIZATION_PROTOCOL, ChannelAuthorization, PublisherAuthorization,
    SNAPSHOT_PROTOCOL, SignedRegistrySnapshot, verify_change_request,
};

#[derive(Debug, Parser)]
#[command(
    name = "tilo-registry-server",
    about = "Self-hostable Tilo app registry."
)]
struct Cli {
    /// Location of the on-disk state. Defaults to
    /// `$XDG_STATE_HOME/tilo-registry` (see `default_data_dir`).
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
    /// Remove an unreferenced publisher from the allowlist. Public entries,
    /// the current snapshot, and pending requests must be drained first.
    RemovePublisher {
        /// Publisher's Ed25519 pubkey (32-byte hex).
        #[arg(long)]
        pubkey: String,
    },
    /// Print the current `publishers.toml` to stdout.
    ListPublishers,
    /// Root-sign and install a short-lived authorization for one online
    /// channel key. This is a bootstrap/manual offline-root operation.
    AuthorizeChannel {
        #[arg(long)]
        owner_key_file: PathBuf,
        #[arg(long)]
        channel_pubkey: String,
        #[arg(long)]
        publisher_pubkey: String,
        #[arg(long)]
        generation: u64,
        #[arg(long, default_value_t = 2_592_000)]
        valid_for_seconds: u64,
        #[arg(long, default_value_t = 86_400)]
        max_snapshot_validity_seconds: u64,
        #[arg(long, default_value_t = 10_000)]
        max_snapshot_entries: usize,
    },
    /// Networked, keyless collection of exact first-party tapp bytes.
    CollectFirstParty {
        #[arg(long)]
        publisher_pubkey: String,
        #[arg(long, default_value_t = 50)]
        max_items: usize,
        #[arg(long, hide = true)]
        allow_insecure_local: bool,
    },
    /// Networkless verification and publication of a collected batch using
    /// only the root-authorized channel key.
    PublishStagedFirstParty {
        #[arg(long)]
        channel_key_file: PathBuf,
        #[arg(long, default_value = "registry-promoter")]
        reviewer: String,
        #[arg(long, default_value = "automated first-party promotion")]
        reason: String,
        #[arg(long, default_value_t = 86_400)]
        valid_for_seconds: u64,
    },
    /// Refresh the current catalogue expiry with the networkless delegated
    /// channel key. Refuses to overtake staged promotion work.
    SignDelegatedSnapshot {
        #[arg(long)]
        channel_key_file: PathBuf,
        #[arg(long, default_value_t = 86_400)]
        valid_for_seconds: u64,
    },
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
    /// Publish a new monotonic snapshot containing the entries from an older
    /// sequence. This is a forward-moving rollback: clients never see a lower
    /// sequence and the current publisher allowlist remains authoritative.
    RollbackSnapshot {
        /// Immutable snapshot sequence whose entry set should be restored.
        #[arg(long)]
        to_sequence: u64,
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
        Cmd::AuthorizeChannel {
            owner_key_file,
            channel_pubkey,
            publisher_pubkey,
            generation,
            valid_for_seconds,
            max_snapshot_validity_seconds,
            max_snapshot_entries,
        } => cmd_authorize_channel(
            &storage,
            &owner_key_file,
            &channel_pubkey,
            &publisher_pubkey,
            generation,
            valid_for_seconds,
            max_snapshot_validity_seconds,
            max_snapshot_entries,
        ),
        Cmd::CollectFirstParty {
            publisher_pubkey,
            max_items,
            allow_insecure_local,
        } => cmd_collect_first_party(&storage, &publisher_pubkey, max_items, allow_insecure_local),
        Cmd::PublishStagedFirstParty {
            channel_key_file,
            reviewer,
            reason,
            valid_for_seconds,
        } => cmd_publish_staged_first_party(
            &storage,
            &channel_key_file,
            &reviewer,
            &reason,
            valid_for_seconds,
        ),
        Cmd::SignDelegatedSnapshot {
            channel_key_file,
            valid_for_seconds,
        } => cmd_sign_delegated_snapshot(&storage, &channel_key_file, valid_for_seconds),
        Cmd::SignSnapshot {
            owner_key_file,
            valid_for_seconds,
        } => cmd_sign_snapshot(&storage, &owner_key_file, valid_for_seconds),
        Cmd::RollbackSnapshot {
            to_sequence,
            owner_key_file,
            valid_for_seconds,
        } => cmd_rollback_snapshot(&storage, to_sequence, &owner_key_file, valid_for_seconds),
        Cmd::Review { command } => cmd_review(&storage, command),
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "each argument is a separately audited CronJob control at the CLI boundary"
)]
fn cmd_authorize_channel(
    storage: &Storage,
    owner_key_file: &std::path::Path,
    channel_pubkey: &str,
    publisher_pubkey: &str,
    generation: u64,
    valid_for_seconds: u64,
    max_snapshot_validity_seconds: u64,
    max_snapshot_entries: usize,
) -> anyhow::Result<()> {
    if generation == 0 || valid_for_seconds == 0 || valid_for_seconds > 31 * 24 * 60 * 60 {
        anyhow::bail!("channel authorization generation and lifetime are invalid");
    }
    if max_snapshot_validity_seconds == 0
        || max_snapshot_validity_seconds > registry_types::MAX_SNAPSHOT_VALIDITY_SECONDS
        || max_snapshot_entries == 0
        || max_snapshot_entries > registry_types::MAX_SNAPSHOT_ENTRIES
    {
        anyhow::bail!("delegated snapshot bounds are invalid");
    }
    let channel = parse_public_key(channel_pubkey, "channel")?;
    let publisher = parse_public_key(publisher_pubkey, "publisher")?;
    let writer = storage.lock_snapshot()?;
    let identity = storage.load_identity()?;
    recover_snapshot_transaction(&writer, &identity.owner_pubkey_hex)?;
    let owner = load_owner_key(owner_key_file, &identity.owner_pubkey_hex)?;
    let allowlist = storage.load_allowlist()?;
    let row = allowlist
        .get(&publisher)
        .ok_or_else(|| anyhow::anyhow!("delegated publisher is not allowlisted"))?;
    let delegated_publishers = vec![PublisherAuthorization {
        pubkey_hex: hex::encode(publisher),
        can_add_new: row.can_add_new,
    }];
    let now = now_unix();
    if let Some(current) = storage.load_channel_authorization()? {
        if generation < current.generation {
            anyhow::bail!("channel authorization generation cannot move backwards");
        }
        if generation == current.generation {
            registry_types::verify_channel_authorization(
                &current,
                &identity.owner_pubkey_hex,
                now,
            )?;
            if current
                .channel_pubkey_hex
                .eq_ignore_ascii_case(channel_pubkey)
                && current.max_snapshot_validity_seconds == max_snapshot_validity_seconds
                && current.max_snapshot_entries == max_snapshot_entries
                && current.publishers == delegated_publishers
            {
                drop(writer);
                println!("{}", serde_json::to_string_pretty(&current)?);
                return Ok(());
            }
            anyhow::bail!("channel authorization generation must increase when policy changes");
        }
    }
    if storage.load_promotion_transaction()?.is_some() {
        anyhow::bail!(
            "complete or recover the active first-party promotion before changing channel authorization"
        );
    }
    let mut authorization = ChannelAuthorization {
        protocol: CHANNEL_AUTHORIZATION_PROTOCOL,
        generation,
        channel_pubkey_hex: hex::encode(channel),
        not_before_unix: now,
        expires_at_unix: now.saturating_add(valid_for_seconds),
        max_snapshot_validity_seconds,
        max_snapshot_entries,
        publishers: delegated_publishers,
        owner_signature_hex: String::new(),
    };
    registry_types::sign_channel_authorization(&mut authorization, &owner)?;
    registry_types::verify_channel_authorization(&authorization, &identity.owner_pubkey_hex, now)?;
    storage.save_channel_authorization(&authorization)?;
    drop(writer);
    println!("{}", serde_json::to_string_pretty(&authorization)?);
    Ok(())
}

fn cmd_collect_first_party(
    storage: &Storage,
    publisher_pubkey: &str,
    max_items: usize,
    allow_insecure_local: bool,
) -> anyhow::Result<()> {
    if !(1..=100).contains(&max_items) {
        anyhow::bail!("--max-items must be between 1 and 100");
    }
    let publisher = parse_public_key(publisher_pubkey, "publisher")?;
    let publisher_hex = hex::encode(publisher);
    let allowlist = storage.load_allowlist()?;
    let row = allowlist
        .get(&publisher)
        .ok_or_else(|| anyhow::anyhow!("the selected first-party publisher is not allowlisted"))?;
    let mut submissions: Vec<_> = ModerationService::new(storage.clone())
        .pending_submissions()?
        .into_iter()
        .filter(|pending| {
            pending
                .change
                .entry
                .publisher_pubkey_hex
                .eq_ignore_ascii_case(&publisher_hex)
        })
        .collect();
    submissions.sort_by(|left, right| left.submission_id.cmp(&right.submission_id));
    submissions.truncate(max_items);
    if submissions.is_empty() {
        let _deleted = storage.delete_staged_promotion()?;
        println!("{}", serde_json::json!({"status": "noop", "collected": 0}));
        return Ok(());
    }
    if !row.can_add_new
        && submissions
            .iter()
            .any(|pending| pending.change.action == EntryAction::Add)
    {
        anyhow::bail!("the selected publisher is not authorized to add new packages");
    }
    let policy = InspectionPolicy {
        allow_insecure_local,
        ..InspectionPolicy::default()
    };
    let mut artifacts = Vec::new();
    for pending in &submissions {
        verify_change_request(&pending.change, &[publisher])
            .map_err(|error| anyhow::anyhow!("verify {}: {error}", pending.submission_id))?;
        if pending.change.action == EntryAction::Delete {
            continue;
        }
        let fetched = fetch_entry_bytes(&pending.change.entry, policy)?;
        for (version, (inspection, bytes)) in pending.change.entry.versions.iter().zip(fetched) {
            let digest = blake3::hash(&bytes).to_hex().to_string();
            storage.save_staged_artifact(&digest, &bytes)?;
            artifacts.push(StagedArtifact {
                submission_id: pending.submission_id.clone(),
                version: inspection.version,
                target: Some(version.target.clone()),
                blake3_hex: digest,
            });
        }
    }
    artifacts.sort_by(|left, right| {
        staged_artifact_sort_key(left).cmp(&staged_artifact_sort_key(right))
    });
    let staged = StagedPromotion {
        publisher_pubkey_hex: publisher_hex,
        collected_at_unix: now_unix(),
        submissions,
        artifacts,
    };
    storage.save_staged_promotion(&staged)?;
    println!(
        "{}",
        serde_json::json!({"status": "staged", "collected": staged.submissions.len()})
    );
    Ok(())
}

fn cmd_sign_delegated_snapshot(
    storage: &Storage,
    channel_key_file: &std::path::Path,
    valid_for_seconds: u64,
) -> anyhow::Result<()> {
    let identity = storage.load_identity()?;
    let writer = storage.lock_snapshot()?;
    recover_snapshot_transaction(&writer, &identity.owner_pubkey_hex)?;
    if storage.load_promotion_transaction()?.is_some() || storage.load_staged_promotion()?.is_some()
    {
        anyhow::bail!("staged first-party promotion must publish before a delegated refresh");
    }
    let authorization = storage
        .load_channel_authorization()?
        .ok_or_else(|| anyhow::anyhow!("no active root-signed channel authorization"))?;
    registry_types::verify_channel_authorization(
        &authorization,
        &identity.owner_pubkey_hex,
        now_unix(),
    )?;
    if valid_for_seconds == 0 || valid_for_seconds > authorization.max_snapshot_validity_seconds {
        anyhow::bail!("snapshot validity exceeds the delegated authorization");
    }
    let channel_key = load_signing_key(
        channel_key_file,
        &authorization.channel_pubkey_hex,
        "channel",
    )?;
    let current = writer
        .current()?
        .ok_or_else(|| anyhow::anyhow!("no current snapshot to refresh"))?;
    registry_types::verify_snapshot(&current, &identity.owner_pubkey_hex, current.issued_at_unix)?;
    writer.archive_current(&current)?;
    let mut candidate = unsigned_snapshot(
        storage,
        Some(&current),
        current.entries.clone(),
        valid_for_seconds,
    )?;
    candidate.publishers.clone_from(&authorization.publishers);
    registry_types::sign_snapshot_delegated(&mut candidate, &channel_key, authorization)?;
    registry_types::verify_snapshot(
        &candidate,
        &identity.owner_pubkey_hex,
        candidate.issued_at_unix,
    )?;
    writer.publish(&candidate)?;
    drop(writer);
    println!("{}", serde_json::to_string_pretty(&candidate)?);
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "the networkless signer keeps verification, transaction, and publication order visible at one boundary"
)]
fn cmd_publish_staged_first_party(
    storage: &Storage,
    channel_key_file: &std::path::Path,
    reviewer: &str,
    reason: &str,
    valid_for_seconds: u64,
) -> anyhow::Result<()> {
    let writer = storage.lock_snapshot()?;
    let identity = storage.load_identity()?;
    recover_snapshot_transaction(&writer, &identity.owner_pubkey_hex)?;
    let active_journal = storage.load_promotion_transaction()?;
    if let Some(journal) = active_journal.as_ref()
        && writer.current()?.as_ref() == Some(&journal.candidate_snapshot)
    {
        registry_types::verify_snapshot(
            &journal.candidate_snapshot,
            &identity.owner_pubkey_hex,
            journal.candidate_snapshot.issued_at_unix,
        )?;
        let moderation = ModerationService::new(storage.clone());
        moderation.complete_promotion_in_transaction(&writer, &journal.submissions)?;
        drop(writer);
        println!(
            "{}",
            serde_json::json!({
                "status": "published",
                "promoted": journal.submissions.len(),
                "sequence": journal.candidate_snapshot.sequence,
                "snapshot_hash": registry_types::snapshot_hash(&journal.candidate_snapshot)?,
            })
        );
        return Ok(());
    }
    let authorization = storage
        .load_channel_authorization()?
        .ok_or_else(|| anyhow::anyhow!("no active root-signed channel authorization"))?;
    registry_types::verify_channel_authorization(
        &authorization,
        &identity.owner_pubkey_hex,
        now_unix(),
    )?;
    if valid_for_seconds == 0 || valid_for_seconds > authorization.max_snapshot_validity_seconds {
        anyhow::bail!("snapshot validity exceeds the delegated authorization");
    }
    let channel_key = load_signing_key(
        channel_key_file,
        &authorization.channel_pubkey_hex,
        "channel",
    )?;
    let staged = match storage.load_staged_promotion()? {
        Some(staged) => staged,
        None if active_journal.is_none() => {
            drop(writer);
            println!("{}", serde_json::json!({"status": "noop", "published": 0}));
            return Ok(());
        }
        None => anyhow::bail!("promotion recovery is missing its collected first-party batch"),
    };
    let [delegated_publisher] = authorization.publishers.as_slice() else {
        anyhow::bail!("delegated authorization must contain exactly one publisher");
    };
    if !delegated_publisher
        .pubkey_hex
        .eq_ignore_ascii_case(&staged.publisher_pubkey_hex)
    {
        anyhow::bail!("staged publisher is outside the delegated scope");
    }
    let publisher = parse_public_key(&staged.publisher_pubkey_hex, "publisher")?;
    let allowlist = storage.load_allowlist()?;
    let row = allowlist
        .get(&publisher)
        .ok_or_else(|| anyhow::anyhow!("delegated publisher is no longer allowlisted"))?;
    if row.can_add_new != delegated_publisher.can_add_new {
        anyhow::bail!("live publisher policy differs from the root-signed delegation");
    }
    let moderation = ModerationService::new(storage.clone());
    for pending in &staged.submissions {
        if moderation.pending(&pending.submission_id)?.as_ref() != Some(pending) {
            anyhow::bail!(
                "staged submission {} changed; recollect it",
                pending.submission_id
            );
        }
        verify_change_request(&pending.change, &[publisher])
            .map_err(|error| anyhow::anyhow!("verify {}: {error}", pending.submission_id))?;
        if pending.change.action == EntryAction::Delete {
            continue;
        }
        for version in &pending.change.entry.versions {
            let matching: Vec<_> = staged
                .artifacts
                .iter()
                .filter(|artifact| staged_artifact_matches(artifact, pending, version))
                .collect();
            let [artifact] = matching.as_slice() else {
                anyhow::bail!(
                    "staged artifact inventory is incomplete or ambiguous for {}@{} ({}/{})",
                    pending.submission_id,
                    version.version,
                    version.target.platform,
                    version.target.architecture,
                );
            };
            let bytes = storage.load_staged_artifact(&artifact.blake3_hex)?;
            let _inspection = inspect_bytes(&pending.change.entry, version, &bytes)?;
        }
    }
    let expected_artifacts: usize = staged
        .submissions
        .iter()
        .filter(|pending| pending.change.action != EntryAction::Delete)
        .map(|pending| pending.change.entry.versions.len())
        .sum();
    if staged.artifacts.len() != expected_artifacts {
        anyhow::bail!("staged artifact inventory contains unexpected members");
    }

    let journal = if let Some(journal) = active_journal {
        if journal.submissions != staged.submissions
            || journal.candidate_snapshot.channel_authorization.as_ref() != Some(&authorization)
        {
            anyhow::bail!("a different promotion transaction requires recovery");
        }
        registry_types::verify_snapshot(
            &journal.candidate_snapshot,
            &identity.owner_pubkey_hex,
            journal.candidate_snapshot.issued_at_unix,
        )?;
        journal
    } else {
        let current = writer.current()?;
        if let Some(current) = &current {
            registry_types::verify_snapshot(
                current,
                &identity.owner_pubkey_hex,
                current.issued_at_unix,
            )?;
            writer.archive_current(current)?;
        }
        let mut entries = current
            .as_ref()
            .map_or_else(Vec::new, |snapshot| snapshot.entries.clone());
        apply_staged_changes(&mut entries, &staged.submissions)?;
        if entries.len() > authorization.max_snapshot_entries {
            anyhow::bail!("candidate exceeds delegated entry bound");
        }
        let mut candidate =
            unsigned_snapshot(storage, current.as_ref(), entries, valid_for_seconds)?;
        candidate.publishers.clone_from(&authorization.publishers);
        registry_types::sign_snapshot_delegated(&mut candidate, &channel_key, authorization)?;
        registry_types::verify_snapshot(
            &candidate,
            &identity.owner_pubkey_hex,
            candidate.issued_at_unix,
        )?;
        let journal = PromotionJournal {
            publisher_pubkey_hex: staged.publisher_pubkey_hex.clone(),
            reviewer: reviewer.to_string(),
            reason: reason.to_string(),
            submissions: staged.submissions.clone(),
            candidate_snapshot: candidate,
        };
        storage.save_promotion_transaction(&journal)?;
        journal
    };
    let reviews = moderation.approve_batch_in_transaction(
        &writer,
        &journal.submissions,
        &journal.reviewer,
        &journal.reason,
        now_unix(),
        &allowlist,
    )?;
    writer.publish(&journal.candidate_snapshot)?;
    moderation.complete_promotion_in_transaction(&writer, &journal.submissions)?;
    drop(writer);
    println!(
        "{}",
        serde_json::json!({
            "status": "published",
            "promoted": reviews.len(),
            "sequence": journal.candidate_snapshot.sequence,
            "snapshot_hash": registry_types::snapshot_hash(&journal.candidate_snapshot)?,
        })
    );
    Ok(())
}

type StagedTargetSortKey<'a> = Option<(&'a str, &'a str, &'a str, Option<&'a str>)>;
type StagedArtifactSortKey<'a> = (&'a str, &'a str, StagedTargetSortKey<'a>);

fn staged_artifact_sort_key(artifact: &StagedArtifact) -> StagedArtifactSortKey<'_> {
    (
        &artifact.submission_id,
        &artifact.version,
        artifact.target.as_ref().map(|target| {
            (
                target.platform.as_str(),
                target.architecture.as_str(),
                target.isa.as_str(),
                target.runtime.as_deref(),
            )
        }),
    )
}

fn staged_artifact_matches(
    artifact: &StagedArtifact,
    pending: &registry_server::PendingSubmission,
    version: &registry_types::RegistryVersion,
) -> bool {
    if artifact.submission_id != pending.submission_id || artifact.version != version.version {
        return false;
    }
    artifact.target.as_ref().map_or_else(
        || {
            pending
                .change
                .entry
                .versions
                .iter()
                .filter(|candidate| candidate.version == version.version)
                .count()
                == 1
        },
        |target| target == &version.target,
    )
}

fn apply_staged_changes(
    entries: &mut Vec<registry_types::RegistryEntry>,
    submissions: &[registry_server::PendingSubmission],
) -> anyhow::Result<()> {
    for pending in submissions {
        let id = &pending.change.entry.package_id;
        let existing = entries.iter().position(|entry| entry.package_id == *id);
        match pending.change.action {
            EntryAction::Add if existing.is_none() => entries.push(pending.change.entry.clone()),
            EntryAction::Update => {
                let index = existing.ok_or_else(|| anyhow::anyhow!("{id} is not published"))?;
                let entry = entries
                    .get_mut(index)
                    .ok_or_else(|| anyhow::anyhow!("{id} disappeared during promotion"))?;
                *entry = pending.change.entry.clone();
            }
            EntryAction::Delete => {
                let index = existing.ok_or_else(|| anyhow::anyhow!("{id} is not published"))?;
                entries.remove(index);
            }
            EntryAction::Add => anyhow::bail!("{id} is already published"),
        }
    }
    entries.sort_by(|left, right| left.package_id.cmp(&right.package_id));
    Ok(())
}

fn unsigned_snapshot(
    storage: &Storage,
    previous: Option<&SignedRegistrySnapshot>,
    mut entries: Vec<registry_types::RegistryEntry>,
    valid_for_seconds: u64,
) -> anyhow::Result<SignedRegistrySnapshot> {
    entries.sort_by(|left, right| left.package_id.cmp(&right.package_id));
    let (sequence, previous_snapshot_hash_hex) = match previous {
        Some(previous) => (
            previous
                .sequence
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("snapshot sequence is exhausted"))?,
            registry_types::snapshot_hash(previous)?,
        ),
        None => (1, String::new()),
    };
    let issued_at_unix = now_unix();
    Ok(SignedRegistrySnapshot {
        protocol: SNAPSHOT_PROTOCOL,
        sequence,
        issued_at_unix,
        expires_at_unix: issued_at_unix.saturating_add(valid_for_seconds),
        previous_snapshot_hash_hex,
        moderation_head_hex: storage.moderation_head()?,
        publishers: Vec::new(),
        entries,
        owner_signature_hex: String::new(),
        channel_authorization: None,
    })
}

fn cmd_sign_snapshot(
    storage: &Storage,
    owner_key_file: &std::path::Path,
    valid_for_seconds: u64,
) -> anyhow::Result<()> {
    if valid_for_seconds == 0 || valid_for_seconds > registry_types::MAX_SNAPSHOT_VALIDITY_SECONDS {
        anyhow::bail!("snapshot validity must be between 1 second and 7 days");
    }
    let writer = storage.lock_snapshot()?;
    let identity = storage.load_identity()?;
    let key = load_owner_key(owner_key_file, &identity.owner_pubkey_hex)?;
    recover_snapshot_transaction(&writer, &identity.owner_pubkey_hex)?;
    refuse_active_promotion(storage)?;
    let previous = writer.current()?;
    if let Some(snapshot) = &previous {
        registry_types::verify_snapshot(
            snapshot,
            &identity.owner_pubkey_hex,
            snapshot.issued_at_unix,
        )?;
        // Backfill history for registries created before immutable snapshots
        // were introduced. Saving identical bytes is idempotent.
        writer.archive_current(snapshot)?;
    }
    let (sequence, previous_snapshot_hash_hex) = previous.as_ref().map_or_else(
        || Ok((1, String::new())),
        |snapshot| {
            Ok::<_, registry_types::SnapshotError>((
                snapshot
                    .sequence
                    .checked_add(1)
                    .ok_or(registry_types::SnapshotError::Sequence)?,
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
        channel_authorization: None,
    };
    registry_types::sign_snapshot(&mut snapshot, &key)?;
    registry_types::verify_snapshot(&snapshot, &identity.owner_pubkey_hex, issued_at_unix)?;
    writer.publish(&snapshot)?;
    drop(writer);
    println!("{}", serde_json::to_string_pretty(&snapshot)?);
    Ok(())
}

fn cmd_rollback_snapshot(
    storage: &Storage,
    to_sequence: u64,
    owner_key_file: &std::path::Path,
    valid_for_seconds: u64,
) -> anyhow::Result<()> {
    if valid_for_seconds == 0 || valid_for_seconds > registry_types::MAX_SNAPSHOT_VALIDITY_SECONDS {
        anyhow::bail!("snapshot validity must be between 1 second and 7 days");
    }
    let writer = storage.lock_snapshot()?;
    let identity = storage.load_identity()?;
    let key = load_owner_key(owner_key_file, &identity.owner_pubkey_hex)?;
    recover_snapshot_transaction(&writer, &identity.owner_pubkey_hex)?;
    refuse_active_promotion(storage)?;
    let target = writer
        .sequence(to_sequence)?
        .ok_or_else(|| anyhow::anyhow!("snapshot sequence {to_sequence} is not retained"))?;
    let current = writer
        .current()?
        .ok_or_else(|| anyhow::anyhow!("no current snapshot to roll back"))?;
    registry_types::verify_snapshot(&target, &identity.owner_pubkey_hex, target.issued_at_unix)?;
    registry_types::verify_snapshot(&current, &identity.owner_pubkey_hex, current.issued_at_unix)?;

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
    for entry in &target.entries {
        if !publishers.iter().any(|publisher| {
            publisher
                .pubkey_hex
                .eq_ignore_ascii_case(&entry.publisher_pubkey_hex)
        }) {
            anyhow::bail!(
                "snapshot sequence {to_sequence} contains publisher {} which is no longer allowed",
                entry.publisher_pubkey_hex
            );
        }
    }

    let issued_at_unix = now_unix();
    let mut snapshot = SignedRegistrySnapshot {
        protocol: SNAPSHOT_PROTOCOL,
        sequence: current
            .sequence
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("snapshot sequence is exhausted"))?,
        issued_at_unix,
        expires_at_unix: issued_at_unix.saturating_add(valid_for_seconds),
        previous_snapshot_hash_hex: registry_types::snapshot_hash(&current)?,
        moderation_head_hex: storage.moderation_head()?,
        publishers,
        entries: target.entries,
        owner_signature_hex: String::new(),
        channel_authorization: None,
    };
    registry_types::sign_snapshot(&mut snapshot, &key)?;
    registry_types::verify_snapshot(&snapshot, &identity.owner_pubkey_hex, issued_at_unix)?;
    writer.publish(&snapshot)?;
    drop(writer);
    println!("{}", serde_json::to_string_pretty(&snapshot)?);
    Ok(())
}

fn recover_snapshot_transaction(
    writer: &registry_server::storage::SnapshotLock<'_>,
    owner_pubkey_hex: &str,
) -> anyhow::Result<()> {
    let Some(pending) = writer.pending_transaction()? else {
        return Ok(());
    };
    registry_types::verify_snapshot(&pending, owner_pubkey_hex, pending.issued_at_unix)
        .map_err(|error| anyhow::anyhow!("verify pending snapshot transaction: {error}"))?;
    let recovered = writer
        .recover()?
        .ok_or_else(|| anyhow::anyhow!("pending snapshot transaction disappeared"))?;
    if recovered != pending {
        anyhow::bail!("recovered snapshot transaction changed unexpectedly");
    }
    Ok(())
}

fn refuse_active_promotion(storage: &Storage) -> anyhow::Result<()> {
    if storage.load_promotion_transaction()?.is_some() {
        anyhow::bail!(
            "a first-party promotion transaction requires recovery; run publish-staged-first-party"
        );
    }
    Ok(())
}

fn load_owner_key(
    owner_key_file: &std::path::Path,
    expected_owner: &str,
) -> anyhow::Result<ed25519_dalek::SigningKey> {
    load_signing_key(owner_key_file, expected_owner, "owner")
}

fn parse_public_key(value: &str, label: &str) -> anyhow::Result<[u8; 32]> {
    let bytes: [u8; 32] = hex::decode(value)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .filter(|bytes| *bytes != [0; 32])
        .ok_or_else(|| anyhow::anyhow!("{label} key must be a non-placeholder 32-byte hex key"))?;
    ed25519_dalek::VerifyingKey::from_bytes(&bytes)
        .map_err(|_| anyhow::anyhow!("{label} key is not a valid Ed25519 public key"))?;
    Ok(bytes)
}

fn load_signing_key(
    key_file: &std::path::Path,
    expected_public: &str,
    label: &str,
) -> anyhow::Result<ed25519_dalek::SigningKey> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let mode = std::fs::metadata(key_file)?.mode() & 0o777;
        if mode & 0o077 != 0 {
            anyhow::bail!("{label} key file must not be accessible by group or others");
        }
    }
    let key_hex = zeroize::Zeroizing::new(std::fs::read_to_string(key_file)?.trim().to_owned());
    let seed: zeroize::Zeroizing<[u8; 32]> = zeroize::Zeroizing::new(
        hex::decode(key_hex.as_str())
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| anyhow::anyhow!("{label} key file must contain a 32-byte hex seed"))?,
    );
    let key = ed25519_dalek::SigningKey::from_bytes(&seed);
    let actual = hex::encode(key.verifying_key().to_bytes());
    if !actual.eq_ignore_ascii_case(expected_public) {
        anyhow::bail!("{label} private key does not match its authorized identity");
    }
    Ok(key)
}

#[allow(
    clippy::too_many_lines,
    reason = "the review command keeps all moderation subcommands visible in one CLI dispatcher"
)]
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
                        item.change
                            .entry
                            .name
                            .resolve("en")
                            .unwrap_or(&item.change.entry.package_id),
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
                    pending
                        .change
                        .entry
                        .name
                        .resolve("en")
                        .unwrap_or(&pending.change.entry.package_id),
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
            "no handshake.toml at {} — run `tilo-registry-server init …` first",
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
    let transaction = storage.lock_snapshot()?;
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
    let normalized_owner = owner_pubkey.to_ascii_lowercase();
    if storage.handshake_path().exists() {
        let existing = storage.load_identity()?;
        if !existing
            .owner_pubkey_hex
            .eq_ignore_ascii_case(&normalized_owner)
            && (transaction.current()?.is_some()
                || transaction.pending_transaction()?.is_some()
                || storage.load_channel_authorization()?.is_some()
                || storage.load_promotion_transaction()?.is_some())
        {
            anyhow::bail!(
                "cannot replace the registry owner while signed or transactional state exists"
            );
        }
    }
    let ident = ServerIdentity {
        owner_pubkey_hex: normalized_owner,
        name,
        description,
        base_url,
    };
    storage
        .save_identity(&ident)
        .map_err(|e| anyhow::anyhow!("save identity: {e}"))?;
    drop(transaction);
    info!(
        "wrote handshake to {} — start the server with `tilo-registry-server serve`",
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
    let transaction = storage.lock_snapshot()?;
    refuse_active_promotion(storage)?;
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
    drop(transaction);
    if replaced {
        info!("replaced publisher {pubkey}");
    } else {
        info!("added publisher {pubkey}");
    }
    Ok(())
}

/// `Cmd::RemovePublisher` — drop one allowlist row.
fn cmd_remove_publisher(storage: &Storage, pubkey: &str) -> anyhow::Result<()> {
    let transaction = storage.lock_snapshot()?;
    refuse_active_promotion(storage)?;
    let current_snapshot = transaction.current()?;
    let referenced_entry = storage
        .load_entries()?
        .into_iter()
        .find(|entry| entry.publisher_pubkey_hex.eq_ignore_ascii_case(pubkey))
        .or_else(|| {
            current_snapshot.and_then(|snapshot| {
                snapshot
                    .entries
                    .into_iter()
                    .find(|entry| entry.publisher_pubkey_hex.eq_ignore_ascii_case(pubkey))
            })
        });
    if let Some(entry) = referenced_entry {
        anyhow::bail!(
            "cannot remove publisher {pubkey}: public package {} still references it; quarantine or delete the package and publish that state first",
            entry.package_id
        );
    }
    if let Some(pending) = storage
        .load_pending_submissions()?
        .into_iter()
        .find(|pending| {
            pending
                .change
                .entry
                .publisher_pubkey_hex
                .eq_ignore_ascii_case(pubkey)
        })
    {
        anyhow::bail!(
            "cannot remove publisher {pubkey}: pending package {} still references it; reject or approve the request first",
            pending.change.entry.package_id
        );
    }
    let mut allow: PublisherAllowlist = storage
        .load_allowlist()
        .map_err(|e| anyhow::anyhow!("load allowlist: {e}"))?;
    if !allow.remove(pubkey) {
        anyhow::bail!("no publisher with pubkey {pubkey}");
    }
    storage
        .save_allowlist(&allow)
        .map_err(|e| anyhow::anyhow!("save allowlist: {e}"))?;
    drop(transaction);
    info!("removed publisher {pubkey}");
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use registry_types::{RegistryEntry, RegistryTarget, RegistryVersion, sign_entry};

    #[test]
    fn staged_artifacts_are_selected_by_version_and_target() {
        let target = |architecture: &str, isa: &str| RegistryTarget {
            platform: registry_types::TILO_OS_PLATFORM.into(),
            architecture: architecture.into(),
            isa: isa.into(),
            runtime: None,
        };
        let key = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
        let mut entry = publisher_entry(&key);
        let mut arm = entry.versions[0].clone();
        arm.target = target("aarch64", "aarch64-v8.2");
        entry.versions.push(arm);
        sign_entry(&mut entry, &key);
        let pending = registry_server::PendingSubmission {
            submission_id: entry.package_id.clone(),
            submitted_at_unix: 1,
            change: registry_types::sign_change_request(EntryAction::Add, entry, &key),
        };
        let artifacts = [
            StagedArtifact {
                submission_id: pending.submission_id.clone(),
                version: "1.0.0".into(),
                target: Some(target("x86_64", "x86_64-v3")),
                blake3_hex: "aa".repeat(32),
            },
            StagedArtifact {
                submission_id: pending.submission_id.clone(),
                version: "1.0.0".into(),
                target: Some(target("aarch64", "aarch64-v8.2")),
                blake3_hex: "bb".repeat(32),
            },
        ];
        for version in &pending.change.entry.versions {
            let matches = artifacts
                .iter()
                .filter(|artifact| staged_artifact_matches(artifact, &pending, version))
                .count();
            assert_eq!(
                matches, 1,
                "{} must select exactly one staged artifact",
                version.target.architecture
            );
        }

        let legacy = StagedArtifact {
            submission_id: pending.submission_id.clone(),
            version: "1.0.0".into(),
            target: None,
            blake3_hex: "cc".repeat(32),
        };
        assert!(
            pending
                .change
                .entry
                .versions
                .iter()
                .all(|version| !staged_artifact_matches(&legacy, &pending, version)),
            "a target-less artifact must fail closed when a version is multi-architecture"
        );

        let mut single = pending;
        single.change.entry.versions.truncate(1);
        assert!(staged_artifact_matches(
            &legacy,
            &single,
            &single.change.entry.versions[0]
        ));
    }

    fn publisher_entry(key: &ed25519_dalek::SigningKey) -> RegistryEntry {
        let mut entry = RegistryEntry {
            package_id: "so.tilo.concurrent".into(),
            name: "Concurrent".into(),
            description: "publisher removal regression".into(),
            tagline: "Test concurrent publication".into(),
            images: registry_types::LocalizedImages::english_icon(
                "https://apps.tilo.so/images/test-icon.svg",
                "Test icon",
            ),
            supported_languages: vec!["en".into()],
            categories: Vec::new(),
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: hex::encode(key.verifying_key().to_bytes()),
            versions: vec![RegistryVersion {
                version: "1.0.0".into(),
                tapp_url: "https://apps.tilo.so/artifacts/concurrent.tapp".into(),
                tapp_root_hash_hex: "ab".repeat(32),
                tapp_size_bytes: 1,
                released_at_unix: 1,
                tapp_blake3_hex: "cd".repeat(32),
                target: RegistryTarget {
                    platform: registry_types::TILO_OS_PLATFORM.into(),
                    architecture: "x86_64".into(),
                    isa: "x86_64-v3".into(),
                    runtime: None,
                },
            }],
            updated_at_unix: 1,
            signature_hex: String::new(),
        };
        sign_entry(&mut entry, key);
        entry
    }

    fn build_signed_snapshot(
        storage: &Storage,
        previous: Option<&SignedRegistrySnapshot>,
        entries: Vec<RegistryEntry>,
        owner: &ed25519_dalek::SigningKey,
        valid_for_seconds: u64,
    ) -> anyhow::Result<SignedRegistrySnapshot> {
        let mut snapshot = unsigned_snapshot(storage, previous, entries, valid_for_seconds)?;
        snapshot.publishers = storage
            .load_allowlist()?
            .publishers
            .into_iter()
            .map(|row| PublisherAuthorization {
                pubkey_hex: row.pubkey_hex.to_ascii_lowercase(),
                can_add_new: row.can_add_new,
            })
            .collect();
        snapshot
            .publishers
            .sort_by(|left, right| left.pubkey_hex.cmp(&right.pubkey_hex));
        registry_types::sign_snapshot(&mut snapshot, owner)?;
        registry_types::verify_snapshot(
            &snapshot,
            &hex::encode(owner.verifying_key().to_bytes()),
            snapshot.issued_at_unix,
        )?;
        Ok(snapshot)
    }

    #[tokio::test]
    async fn publisher_removal_waits_for_promotion_and_keeps_entries_servable() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let owner = ed25519_dalek::SigningKey::from_bytes(&[51; 32]);
        let publisher = ed25519_dalek::SigningKey::from_bytes(&[52; 32]);
        let owner_hex = hex::encode(owner.verifying_key().to_bytes());
        let publisher_hex = hex::encode(publisher.verifying_key().to_bytes());
        let identity = ServerIdentity {
            owner_pubkey_hex: owner_hex.clone(),
            name: "test".into(),
            description: "test".into(),
            base_url: "http://127.0.0.1".into(),
        };
        storage.save_identity(&identity).unwrap();
        cmd_add_publisher(&storage, &publisher_hex, true, String::new()).unwrap();
        let entry = publisher_entry(&publisher);
        storage.save_entry(&entry).unwrap();

        // Simulate promotion holding the registry transaction while a
        // concurrent CLI attempts revocation. Removal must observe the
        // published result, rather than racing the allowlist read.
        let writer = storage.lock_snapshot().unwrap();
        let remove_storage = storage.clone();
        let remove_key = publisher_hex.clone();
        let remover = std::thread::spawn(move || {
            cmd_remove_publisher(&remove_storage, &remove_key).unwrap_err()
        });
        let first = build_signed_snapshot(&storage, None, vec![entry], &owner, 3_600).unwrap();
        writer.publish(&first).unwrap();
        drop(writer);
        let removal_error = remover.join().unwrap().to_string();
        assert!(removal_error.contains("public package so.tilo.concurrent"));
        registry_types::verify_snapshot(&first, &owner_hex, first.issued_at_unix).unwrap();
        assert!(
            storage
                .load_allowlist()
                .unwrap()
                .get(&publisher.verifying_key().to_bytes())
                .is_some()
        );

        // Revocation becomes legal only after both live storage and the
        // published pointer no longer reference the publisher.
        storage.delete_entry("so.tilo.concurrent").unwrap();
        assert!(
            cmd_remove_publisher(&storage, &publisher_hex)
                .unwrap_err()
                .to_string()
                .contains("public package so.tilo.concurrent")
        );
        let writer = storage.lock_snapshot().unwrap();
        let second =
            build_signed_snapshot(&storage, Some(&first), Vec::new(), &owner, 3_600).unwrap();
        writer.publish(&second).unwrap();
        drop(writer);
        cmd_remove_publisher(&storage, &publisher_hex).unwrap();
        assert!(
            build_signed_snapshot(
                &storage,
                Some(&second),
                vec![publisher_entry(&publisher)],
                &owner,
                3_600,
            )
            .is_err(),
            "a promotion candidate may never contain an unauthorized entry"
        );

        let writer = storage.lock_snapshot().unwrap();
        let third =
            build_signed_snapshot(&storage, Some(&second), Vec::new(), &owner, 3_600).unwrap();
        writer.publish(&third).unwrap();
        drop(writer);
        registry_types::verify_snapshot(&third, &owner_hex, third.issued_at_unix).unwrap();

        let bound = registry_server::bind_and_run(
            "127.0.0.1:0".parse().unwrap(),
            AppState::new(storage, identity),
        )
        .await
        .unwrap();
        let response = reqwest::get(format!("http://{bound}/entries"))
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let served: SignedRegistrySnapshot = response.json().await.unwrap();
        assert_eq!(served, third);
    }
}
