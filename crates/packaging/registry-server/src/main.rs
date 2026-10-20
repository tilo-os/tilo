//! `tilo-registry-server` CLI — bind a port, add allowlisted
//! publishers, print the identity, or bootstrap a fresh instance.
//!
//! ```text
//! tilo-registry-server serve --port 8443 --identity <path>
//! tilo-registry-server add-publisher --pubkey <hex> --namespace <ns> [--first-party] [--can-add-new] [--note ...]
//! tilo-registry-server remove-publisher --pubkey <hex>
//! tilo-registry-server list-publishers
//! tilo-registry-server init --owner-pubkey <hex> --name <n> --description <d> --base-url <url>
//! ```

use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Mutex;

use clap::{Parser, Subcommand};
use tilo_clock::unix_now;
use tracing::info;
use tracing_subscriber::EnvFilter;

use registry_server::{
    AppState, InspectionPolicy, ModerationService, PromotionJournal, PruneReport,
    PublisherAllowlist, PublisherRow, ServerIdentity, StagedArtifact, StagedPromotion, Storage,
    default_data_dir, run_server,
};
use registry_types::EntryAction;
use registry_types::{
    ChannelAuthorization, PACKAGE_FORMAT_VERSION, PublisherAuthorization, SignedRegistrySnapshot,
    verify_change_request,
};

const DEFAULT_FIRST_PARTY_COLLECTION_PARALLELISM: usize = 4;
const MAX_FIRST_PARTY_COLLECTION_PARALLELISM: usize = 8;

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
        /// Let pages on this exact origin (`https://host[:port]`) read GET
        /// responses cross-origin, e.g. a browser runtime resolving apps.
        /// Repeatable. POST and credentials are never allowed cross-origin.
        #[arg(long = "cors-origin")]
        cors_origins: Vec<String>,
    },
    /// Store a full .tapp under its BLAKE3 so `serve` hosts it at
    /// `/artifacts/<blake3>.tapp`, and print that URL. Importing publishes
    /// nothing: a version must still be submitted, reviewed and signed.
    ImportArtifact {
        /// The full .tapp to host.
        file: PathBuf,
    },
    /// Write the compressed twin (`<blake3>.zst`, `<blake3>.tapp.zst`) of
    /// every stored payload object and imported package that lacks one, and
    /// print what was written. Objects stored from now on get theirs when
    /// they are stored; run this once for those stored before. Safe to rerun
    /// and to run while `serve` runs.
    CompressPayloads,
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
        /// Reverse-DNS namespace the publisher's package ids must lie in
        /// (`com.example`, `io.github.<account>`); repeatable and required.
        /// Tilo's own publishers are added with `--namespace so.tilo
        /// --first-party`.
        #[arg(long = "namespace", required = true)]
        namespaces: Vec<String>,
        /// Confirm a grant of Tilo's own namespace `so.tilo` (or one inside
        /// it). Required whenever such a namespace is named, so the
        /// first-party grant is always deliberate.
        #[arg(long)]
        first_party: bool,
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
        /// Maximum number of submissions fetched and inspected concurrently.
        #[arg(long, default_value_t = DEFAULT_FIRST_PARTY_COLLECTION_PARALLELISM)]
        parallelism: usize,
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
    /// Delete staged packages, thin packages and payload blobs that no
    /// published, quarantined or pending entry, retained snapshot or served
    /// version index references any more, and keep remaining staged packages
    /// as their thin form and shared blobs. Promotion does this after every
    /// publish.
    PruneStorage {
        /// Report what would be deleted and change nothing.
        #[arg(long)]
        dry_run: bool,
        /// With --dry-run, also report which published versions publishers
        /// keeping this many releases per target would drop, and what
        /// pruning would then delete.
        #[arg(long, requires = "dry_run")]
        keep_releases: Option<usize>,
        /// As --keep-releases, for pre-releases.
        #[arg(long, requires = "dry_run")]
        keep_prereleases: Option<usize>,
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
    /// the offline signing environment; `serve` never reads this key. It is
    /// also how a registry moves to this build's package format: a current
    /// snapshot at another format is chained by sequence and hash only.
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
    /// Decide on keys asking to publish under namespaces they proved.
    Applications {
        #[command(subcommand)]
        command: ApplicationsCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ApplicationsCommand {
    /// List applications waiting for a decision.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show one application and the review id for an exact decision.
    Show {
        pubkey: String,
        #[arg(long)]
        json: bool,
    },
    /// Check the proofs again and allowlist the key for their namespaces.
    Approve {
        pubkey: String,
        #[arg(long)]
        reviewer: String,
        /// Review id from applications show; refuse a changed application.
        #[arg(long)]
        review_id: Option<String>,
    },
    /// Turn an application down; the key's allowlist row is untouched.
    Reject {
        pubkey: String,
        #[arg(long)]
        reviewer: String,
        #[arg(long)]
        reason: String,
        /// Review id from applications show; refuse a changed application.
        #[arg(long)]
        review_id: Option<String>,
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
        /// Review id from review show; refuse a replaced request.
        #[arg(long)]
        review_id: Option<String>,
    },
    /// Reject a request while retaining an audit record.
    Reject {
        package_id: String,
        #[arg(long)]
        reviewer: String,
        #[arg(long)]
        reason: String,
        /// Review id from review show; refuse a replaced request.
        #[arg(long)]
        review_id: Option<String>,
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

fn main() -> anyhow::Result<()> {
    tilo_rt::daemon("tilo-registry").run(serve_cli())?
}

#[allow(clippy::too_many_lines, reason = "one dispatch arm per subcommand")]
async fn serve_cli() -> anyhow::Result<()> {
    // Commands print their JSON result on stdout; logs go to stderr.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
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
            cors_origins,
        } => cmd_serve(storage, bind, port, cors_origins).await,
        Cmd::ImportArtifact { file } => cmd_import_artifact(&storage, &file),
        Cmd::CompressPayloads => {
            let report = storage.compress_stored_objects()?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
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
            namespaces,
            first_party,
        } => cmd_add_publisher(
            &storage,
            &pubkey,
            can_add_new,
            note,
            NamespaceGrant {
                namespaces,
                first_party,
            },
        ),
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
            parallelism,
            allow_insecure_local,
        } => cmd_collect_first_party(
            &storage,
            &publisher_pubkey,
            max_items,
            parallelism,
            allow_insecure_local,
        ),
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
        Cmd::PruneStorage {
            dry_run,
            keep_releases,
            keep_prereleases,
        } => {
            if dry_run {
                cmd_preview_prune(&storage, keep_releases, keep_prereleases)
            } else {
                cmd_prune_storage(&storage)
            }
        }
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
        // Review fetches packages with a blocking HTTP client, which must not
        // run (or be dropped) on an async worker.
        Cmd::Applications { command } => cmd_applications(&storage, command),
        Cmd::Review { command } => tilo_rt::io::run(move || Ok(cmd_review(&storage, command)))
            .await
            .map_err(|error| anyhow::anyhow!("review: {error}"))?,
    }
}

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
    let now = unix_now();
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
        protocol: PACKAGE_FORMAT_VERSION,
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
    parallelism: usize,
    allow_insecure_local: bool,
) -> anyhow::Result<()> {
    if !(1..=100).contains(&max_items) {
        anyhow::bail!("--max-items must be between 1 and 100");
    }
    if !(1..=MAX_FIRST_PARTY_COLLECTION_PARALLELISM).contains(&parallelism) {
        anyhow::bail!(
            "--parallelism must be between 1 and {MAX_FIRST_PARTY_COLLECTION_PARALLELISM}"
        );
    }
    let Some(parallelism) = NonZeroUsize::new(parallelism) else {
        anyhow::bail!("--parallelism must be non-zero");
    };
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
    // Keep the bounded batch progressing when apps wait for a provider whose
    // package id sorts after them. Verification remains independent of order.
    submissions.sort_by(|left, right| {
        let provider_priority = |pending: &registry_server::PendingSubmission| {
            !registry_types::is_runtime_package_id(&pending.change.entry.package_id)
        };
        provider_priority(left)
            .cmp(&provider_priority(right))
            .then_with(|| left.submission_id.cmp(&right.submission_id))
    });
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
    let persisted_digests = Mutex::new(BTreeSet::new());
    let (staged, failures) = stage_collected_promotion(
        storage,
        publisher_hex,
        submissions,
        parallelism,
        |pending| {
            collect_submission_artifacts(storage, pending, publisher, policy, &persisted_digests)
        },
    )?;
    let failed = failures
        .iter()
        .map(|(submission_id, error)| {
            serde_json::json!({"submission": submission_id, "error": format!("{error:#}")})
        })
        .collect::<Vec<_>>();
    println!(
        "{}",
        serde_json::json!({
            "status": if failures.is_empty() { "staged" } else { "incomplete" },
            "collected": staged.as_ref().map_or(0, |staged| staged.submissions.len()),
            "failed": failed,
        })
    );
    if let Some((submission_id, error)) = failures.first() {
        anyhow::bail!(
            "{} submission(s) stay pending; first: {submission_id}: {error:#}",
            failures.len()
        );
    }
    Ok(())
}

fn collect_submission_artifacts(
    storage: &Storage,
    pending: &registry_server::PendingSubmission,
    publisher: [u8; 32],
    policy: InspectionPolicy,
    persisted_digests: &Mutex<BTreeSet<String>>,
) -> anyhow::Result<Vec<StagedArtifact>> {
    verify_change_request(&pending.change, &[publisher])
        .map_err(|error| anyhow::anyhow!("verify {}: {error}", pending.submission_id))?;
    if pending.change.action == EntryAction::Delete {
        return Ok(Vec::new());
    }
    let mut artifacts = Vec::with_capacity(pending.change.entry.versions.len());
    for version in &pending.change.entry.versions {
        let Some(tapp) = version.tapp() else {
            anyhow::bail!(
                "{}@{} binds no package to collect",
                pending.submission_id,
                version.version
            );
        };
        // Staged bytes are content-addressed by the digest the publisher
        // signed, so an update that repeats already collected versions needs
        // no refetch, and those versions stay collectable after their
        // advertised URL stops serving them. They pass the same inspection.
        let inspection = if let Some(bytes) = storage.find_staged_artifact(&tapp.tapp_blake3_hex)? {
            registry_server::inspect_stored_bytes(
                storage,
                &pending.change.entry,
                version,
                &bytes,
                Some(policy),
            )?
        } else {
            let (inspection, bytes) = registry_server::review_version_bytes(
                storage,
                &pending.change.entry,
                version,
                policy,
            )?;
            let digest = blake3::hash(&bytes).to_hex().to_string();
            persist_distinct_artifact(storage, persisted_digests, &digest, &bytes)?;
            inspection
        };
        artifacts.push(StagedArtifact {
            submission_id: pending.submission_id.clone(),
            version: inspection.version,
            target: Some(tapp.target.clone()),
            blake3_hex: tapp.tapp_blake3_hex.to_ascii_lowercase(),
        });
    }
    Ok(artifacts)
}

fn persist_distinct_artifact(
    storage: &Storage,
    persisted_digests: &Mutex<BTreeSet<String>>,
    digest: &str,
    bytes: &[u8],
) -> anyhow::Result<()> {
    // Serialize only the short content-addressed write boundary. Fetch and
    // inspection remain parallel, while duplicate digests cannot race the
    // atomic path check/write sequence.
    let mut persisted = persisted_digests
        .lock()
        .map_err(|_| anyhow::anyhow!("staged artifact digest lock is poisoned"))?;
    if persisted.contains(digest) {
        return Ok(());
    }
    storage.save_staged_artifact(digest, bytes)?;
    persisted.insert(digest.to_owned());
    drop(persisted);
    Ok(())
}

/// Submissions whose artifacts could not be collected, with the reason.
type CollectionFailures = Vec<(String, anyhow::Error)>;

fn stage_collected_promotion<F>(
    storage: &Storage,
    publisher_pubkey_hex: String,
    submissions: Vec<registry_server::PendingSubmission>,
    parallelism: NonZeroUsize,
    collect: F,
) -> anyhow::Result<(Option<StagedPromotion>, CollectionFailures)>
where
    F: Fn(&registry_server::PendingSubmission) -> anyhow::Result<Vec<StagedArtifact>> + Sync,
{
    // Invalidate any earlier batch before fallible network or storage work. A
    // failed recollection may leave harmless content-addressed bytes behind,
    // but never a manifest the networkless signer could publish.
    let _deleted = storage.delete_staged_promotion()?;
    let collected = bounded_parallel_map(&submissions, parallelism, |pending| {
        Ok::<_, std::convert::Infallible>(collect(pending))
    })
    .unwrap_or_else(|never| match never {});
    // Every submission is verified on its own, so one whose artifacts cannot
    // be collected stays pending while the complete ones are staged.
    let mut staged_submissions = Vec::with_capacity(submissions.len());
    let mut artifacts = Vec::new();
    let mut failures = Vec::new();
    for (pending, result) in submissions.into_iter().zip(collected) {
        match result {
            Ok(collected) => {
                artifacts.extend(collected);
                staged_submissions.push(pending);
            }
            Err(error) => failures.push((pending.submission_id, error)),
        }
    }
    if staged_submissions.is_empty() {
        return Ok((None, failures));
    }
    artifacts.sort_by(|left, right| {
        staged_artifact_sort_key(left).cmp(&staged_artifact_sort_key(right))
    });
    let staged = StagedPromotion {
        publisher_pubkey_hex,
        collected_at_unix: unix_now(),
        submissions: staged_submissions,
        artifacts,
    };
    storage.save_staged_promotion(&staged)?;
    Ok((Some(staged), failures))
}

fn bounded_parallel_map<T, R, E, F>(
    items: &[T],
    parallelism: NonZeroUsize,
    operation: F,
) -> Result<Vec<R>, E>
where
    T: Sync,
    R: Send,
    E: Send,
    F: Fn(&T) -> Result<R, E> + Sync,
{
    let mut output = Vec::with_capacity(items.len());
    for batch in items.chunks(parallelism.get()) {
        let results = std::thread::scope(|scope| {
            let workers = batch
                .iter()
                .map(|item| scope.spawn(|| operation(item)))
                .collect::<Vec<_>>();
            workers
                .into_iter()
                .map(std::thread::ScopedJoinHandle::join)
                .collect::<Vec<_>>()
        });
        for result in results {
            let result = result.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
            output.push(result?);
        }
    }
    Ok(output)
}

fn cmd_sign_delegated_snapshot(
    storage: &Storage,
    channel_key_file: &std::path::Path,
    valid_for_seconds: u64,
) -> anyhow::Result<()> {
    let identity = storage.load_identity()?;
    let writer = storage.lock_snapshot()?;
    recover_snapshot_transaction(&writer, &identity.owner_pubkey_hex)?;
    // A collected batch has not changed the public catalogue. Refreshing only
    // the current entries keeps it available while an incomplete batch waits
    // for its runtime. An active transaction owns a candidate/sequence and
    // must complete before any independent snapshot can advance that sequence.
    if storage.load_promotion_transaction()?.is_some() {
        anyhow::bail!("active first-party promotion must publish before a delegated refresh");
    }
    let authorization = storage
        .load_channel_authorization()?
        .ok_or_else(|| anyhow::anyhow!("no active root-signed channel authorization"))?;
    registry_types::verify_channel_authorization(
        &authorization,
        &identity.owner_pubkey_hex,
        unix_now(),
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
    publish_compact(
        &writer,
        &candidate,
        &channel_key,
        &identity.owner_pubkey_hex,
    )?;
    writer.compact_history()?;
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
        let pruned = writer.prune_unreferenced()?;
        let history = writer.compact_history()?;
        drop(writer);
        let pruned = compact_after_prune(storage, pruned)?;
        println!(
            "{}",
            serde_json::json!({
                "status": "published",
                "promoted": journal.submissions.len(),
                "sequence": journal.candidate_snapshot.sequence,
                "snapshot_hash": registry_types::snapshot_hash(&journal.candidate_snapshot)?,
                "pruned": pruned,
                "history": history,
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
        unix_now(),
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
                    "staged artifact inventory is incomplete or ambiguous for {}@{} ({:?})",
                    pending.submission_id,
                    version.version,
                    version.group(),
                );
            };
            let bytes = storage.load_staged_artifact(&artifact.blake3_hex)?;
            let _inspection = registry_server::inspect_stored_bytes(
                storage,
                &pending.change.entry,
                version,
                &bytes,
                None,
            )?;
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
        validate_new_native_runtimes(
            &journal.candidate_snapshot.entries,
            writer.current()?.as_ref(),
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
        let (entries, selected) = select_installable_promotions(&staged, current.as_ref())?;
        if selected.submissions.is_empty() {
            println!(
                "{}",
                serde_json::json!({
                    "status": "deferred",
                    "published": 0,
                    "deferred": staged.submissions.len(),
                    "reason": "exact native runtime is not offered",
                })
            );
            return Ok(());
        }
        validate_new_native_runtimes(&entries, current.as_ref())?;
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
            submissions: selected.submissions.clone(),
            candidate_snapshot: candidate,
        };
        // A recovered journal must see exactly its selected, verified inventory.
        // Unselected requests remain pending and retain their staged bytes; the
        // next collector can reconsider them without changing their signatures.
        if selected != staged {
            storage.save_staged_promotion(&selected)?;
        }
        storage.save_promotion_transaction(&journal)?;
        journal
    };
    let reviews = moderation.approve_batch_in_transaction(
        &writer,
        &journal.submissions,
        &journal.reviewer,
        &journal.reason,
        unix_now(),
        &allowlist,
    )?;
    writer.publish(&journal.candidate_snapshot)?;
    publish_compact(
        &writer,
        &journal.candidate_snapshot,
        &channel_key,
        &identity.owner_pubkey_hex,
    )?;
    moderation.complete_promotion_in_transaction(&writer, &journal.submissions)?;
    let pruned = writer.prune_unreferenced()?;
    let history = writer.compact_history()?;
    drop(writer);
    let pruned = compact_after_prune(storage, pruned)?;
    println!(
        "{}",
        serde_json::json!({
            "status": "published",
            "promoted": reviews.len(),
            "deferred": staged.submissions.len().saturating_sub(reviews.len()),
            "sequence": journal.candidate_snapshot.sequence,
            "snapshot_hash": registry_types::snapshot_hash(&journal.candidate_snapshot)?,
            "pruned": pruned,
            "history": history,
        })
    );
    Ok(())
}

/// After pruning what no entry references any more, such as the versions a
/// full entry just let go, compact staged packages one lock at a time.
fn compact_after_prune(
    storage: &Storage,
    pruned: PruneReport,
) -> anyhow::Result<serde_json::Value> {
    let compacted = storage.compact_staged_artifacts()?;
    Ok(serde_json::json!({
        "staged_artifacts": pruned.staged_artifacts,
        "thin_packages": pruned.thin_packages,
        "payload_blobs": pruned.payload_blobs,
        "payload_twins": pruned.payload_twins,
        "bytes": pruned.bytes,
        "compacted_staged_artifacts": compacted,
    }))
}

fn cmd_prune_storage(storage: &Storage) -> anyhow::Result<()> {
    let writer = storage.lock_snapshot()?;
    let pruned = writer.prune_unreferenced()?;
    let history = writer.compact_history()?;
    drop(writer);
    let pruned = compact_after_prune(storage, pruned)?;
    println!(
        "{}",
        serde_json::json!({"status": "pruned", "pruned": pruned, "history": history})
    );
    Ok(())
}

fn cmd_preview_prune(
    storage: &Storage,
    keep_releases: Option<usize>,
    keep_prereleases: Option<usize>,
) -> anyhow::Result<()> {
    let retention = (keep_releases.is_some() || keep_prereleases.is_some()).then(|| {
        let default = registry_types::retention::RetentionPolicy::default();
        registry_types::retention::RetentionPolicy {
            releases: keep_releases.unwrap_or(default.releases),
            prereleases: keep_prereleases.unwrap_or(default.prereleases),
        }
    });
    let writer = storage.lock_snapshot()?;
    let preview = writer.preview_prune(retention.as_ref())?;
    drop(writer);
    println!(
        "{}",
        serde_json::json!({"status": "dry-run", "preview": preview})
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
        |target| Some(target) == version.target(),
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

/// A new thin app must be installable from the same registry. Collection can
/// stage an app before its runtime arrives, or while that runtime's artifacts
/// are unavailable; accepting the app submission does not prove its runtime is
/// offered. Check the complete candidate before signing or reserving a journal.
/// Retained versions keep their history when old runtimes leave under retention.
fn validate_new_native_runtimes(
    entries: &[registry_types::RegistryEntry],
    previous: Option<&SignedRegistrySnapshot>,
) -> anyhow::Result<()> {
    for entry in entries {
        // A runtime is one, and a language pack binds none.
        if registry_types::is_runtime_package_id(&entry.package_id)
            || registry_types::is_language_pack_package_id(&entry.package_id)
        {
            continue;
        }
        if let Some((version, target)) = missing_native_runtime(entry, entries, previous) {
            anyhow::bail!(
                "{}@{} ({}) has no offered native runtime {}; keep the app pending until its runtime is collected",
                entry.package_id,
                version.version,
                target.architecture,
                target.runtime.as_deref().unwrap_or_default(),
            );
        }
    }
    Ok(())
}

/// Select complete signed requests, never individual versions. All collected
/// signatures and bytes have already been reverified by the caller. A request
/// whose new native versions would be uninstallable stays pending; it must not
/// prevent independently installable requests (including SDK providers) from
/// being published. Providers do not depend on app requests, so removing only
/// apps leaves the candidate's provider inventory unchanged.
fn select_installable_promotions(
    staged: &StagedPromotion,
    previous: Option<&SignedRegistrySnapshot>,
) -> anyhow::Result<(Vec<registry_types::RegistryEntry>, StagedPromotion)> {
    let baseline = previous.map_or_else(Vec::new, |snapshot| snapshot.entries.clone());
    let mut entries = baseline.clone();
    apply_staged_changes(&mut entries, &staged.submissions)?;
    let deferred: BTreeSet<_> = entries
        .iter()
        .filter(|entry| {
            !registry_types::is_runtime_package_id(&entry.package_id)
                && !registry_types::is_language_pack_package_id(&entry.package_id)
        })
        .filter(|entry| missing_native_runtime(entry, &entries, previous).is_some())
        .map(|entry| entry.package_id.clone())
        .collect();
    for id in &deferred {
        info!(
            package_id = id,
            "keeping request pending until its exact native runtimes are offered"
        );
    }
    let mut selected = staged.clone();
    selected
        .submissions
        .retain(|pending| !deferred.contains(&pending.change.entry.package_id));
    selected.artifacts.retain(|artifact| {
        selected.submissions.iter().any(|pending| {
            pending
                .change
                .entry
                .versions
                .iter()
                .any(|version| staged_artifact_matches(artifact, pending, version))
        })
    });
    let mut entries = baseline;
    apply_staged_changes(&mut entries, &selected.submissions)?;
    validate_new_native_runtimes(&entries, previous)?;
    Ok((entries, selected))
}

fn missing_native_runtime<'a>(
    entry: &'a registry_types::RegistryEntry,
    entries: &[registry_types::RegistryEntry],
    previous: Option<&SignedRegistrySnapshot>,
) -> Option<(
    &'a registry_types::RegistryVersion,
    &'a registry_types::RegistryTarget,
)> {
    let retained = previous.and_then(|snapshot| {
        snapshot
            .entries
            .iter()
            .find(|old| old.package_id == entry.package_id)
    });
    entry.versions.iter().find_map(|version| {
        let target = version.target()?;
        let missing = target.native_runtime().is_some()
            && !retained.is_some_and(|old| old.versions.contains(version))
            && !entries.iter().any(|provider| {
                registry_types::is_runtime_package_id(&provider.package_id)
                    && provider
                        .versions
                        .iter()
                        .filter_map(registry_types::RegistryVersion::target)
                        .any(|provided| {
                            provided.platform == target.platform
                                && provided.architecture == target.architecture
                                && provided.runtime == target.runtime
                        })
            });
        missing.then_some((version, target))
    })
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
    let issued_at_unix = unix_now();
    Ok(SignedRegistrySnapshot {
        protocol: PACKAGE_FORMAT_VERSION,
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
        // The owner's signature is how a registry moves to a new package
        // format. A predecessor at another format cannot be verified by this
        // build, which reads only its own format, so it is chained by its
        // sequence and hash alone: nothing else of it is used, since the
        // catalogue signed here is the stored entries, verified in full
        // below. The channel signers keep refusing such a predecessor,
        // because they carry its entries forward.
        if snapshot.protocol == PACKAGE_FORMAT_VERSION {
            registry_types::verify_snapshot(
                snapshot,
                &identity.owner_pubkey_hex,
                snapshot.issued_at_unix,
            )?;
        }
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
    let issued_at_unix = unix_now();
    let mut snapshot = SignedRegistrySnapshot {
        protocol: PACKAGE_FORMAT_VERSION,
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
    publish_compact(&writer, &snapshot, &key, &identity.owner_pubkey_hex)?;
    writer.compact_history()?;
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
    let target =
        writer
            .sequence(to_sequence)?
            .ok_or_else(|| match storage.history_compaction() {
                Ok(Some(compaction)) if to_sequence <= compaction.through_sequence => {
                    anyhow::anyhow!(
                        "snapshot sequence {to_sequence} was compacted (history is pruned through \
                 {}); restore it from a backup",
                        compaction.through_sequence
                    )
                }
                _ => anyhow::anyhow!("snapshot sequence {to_sequence} is not retained"),
            })?;
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

    let issued_at_unix = unix_now();
    let mut snapshot = SignedRegistrySnapshot {
        protocol: PACKAGE_FORMAT_VERSION,
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
    publish_compact(&writer, &snapshot, &key, &identity.owner_pubkey_hex)?;
    writer.compact_history()?;
    drop(writer);
    println!("{}", serde_json::to_string_pretty(&snapshot)?);
    Ok(())
}

/// Sign and publish the compact form of the snapshot just published, with
/// the key that signed it.
fn publish_compact(
    writer: &registry_server::storage::SnapshotLock<'_>,
    snapshot: &SignedRegistrySnapshot,
    key: &ed25519_dalek::SigningKey,
    owner_pubkey_hex: &str,
) -> anyhow::Result<()> {
    let (mut compact, indexes) = registry_types::compact_snapshot(snapshot)?;
    registry_types::sign_compact_snapshot(&mut compact, key)?;
    registry_types::verify_compact_snapshot(&compact, owner_pubkey_hex, compact.issued_at_unix)?;
    writer.publish_compact(&compact, &indexes)?;
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
            let inspection: Vec<_> = fetch_for_decision(storage, &pending, allow_insecure_local)?
                .into_iter()
                .map(|(inspection, _bytes)| inspection)
                .collect();
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "review_id": pending.review_id()?,
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
                for inspected in inspection {
                    match inspected {
                        registry_server::Inspection::Package(version) => {
                            println!("\n{} · {} bytes", version.version, version.size_bytes);
                            println!("{}", serde_json::to_string_pretty(&version.sandbox)?);
                        }
                        registry_server::Inspection::Program(program) => {
                            println!(
                                "\nprogram {}: {}, {} steps, triggers {:?}, apps {:?}, connectors {:?}{}",
                                program.version,
                                program.name,
                                program.steps,
                                program.triggers,
                                program.apps,
                                program.connectors,
                                if program.appearance {
                                    ", posts under other names"
                                } else {
                                    ""
                                }
                            );
                        }
                    }
                }
            }
        }
        ReviewCommand::Approve {
            package_id,
            reviewer,
            reason,
            allow_insecure_local,
            review_id,
        } => {
            let pending = require_reviewed_pending(&moderation, &package_id, review_id.as_deref())?;
            let record = registry_server::submission_review::approve(
                storage,
                &pending,
                &reviewer,
                &reason,
                unix_now(),
                InspectionPolicy {
                    allow_insecure_local,
                    ..InspectionPolicy::default()
                },
            )?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        ReviewCommand::Reject {
            package_id,
            reviewer,
            reason,
            review_id,
        } => {
            let pending = require_reviewed_pending(&moderation, &package_id, review_id.as_deref())?;
            let record = moderation.reject_reviewed(&pending, reviewer, reason, unix_now())?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        ReviewCommand::Quarantine {
            package_id,
            reviewer,
            reason,
        } => {
            let record = moderation.quarantine(&package_id, reviewer, reason, unix_now())?;
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
                unix_now(),
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

fn require_reviewed_pending(
    moderation: &ModerationService,
    package_id: &str,
    review_id: Option<&str>,
) -> anyhow::Result<registry_server::PendingSubmission> {
    let pending = require_pending(moderation, package_id)?;
    if let Some(review_id) = review_id
        && pending.review_id()? != review_id
    {
        return Err(registry_server::ModerationError::PendingChanged(package_id.into()).into());
    }
    Ok(pending)
}

fn fetch_for_decision(
    storage: &Storage,
    pending: &registry_server::PendingSubmission,
    allow_insecure_local: bool,
) -> anyhow::Result<Vec<(registry_server::Inspection, Vec<u8>)>> {
    let policy = InspectionPolicy {
        allow_insecure_local,
        ..InspectionPolicy::default()
    };
    registry_server::submission_review::inspect(storage, pending, policy)
}

fn cmd_import_artifact(storage: &Storage, file: &std::path::Path) -> anyhow::Result<()> {
    let bytes = std::fs::read(file).map_err(|e| anyhow::anyhow!("read {}: {e}", file.display()))?;
    let digest = storage.import_artifact(&bytes)?;
    let base = storage.load_identity()?.base_url;
    println!("{}/artifacts/{digest}.tapp", base.trim_end_matches('/'));
    Ok(())
}

/// `Cmd::Serve` — load the identity + allowlist and serve until ctrl-c.
async fn cmd_serve(
    storage: Storage,
    bind: IpAddr,
    port: u16,
    cors_origins: Vec<String>,
) -> anyhow::Result<()> {
    if let Some(invalid) = cors_origins
        .iter()
        .find(|origin| !registry_server::serve::valid_cors_origin(origin))
    {
        anyhow::bail!(
            "--cors-origin {invalid:?} is not an exact origin like https://tilo.so or http://localhost:4321"
        );
    }
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
    allow.warn_rows_without_namespaces();
    if !cors_origins.is_empty() {
        info!("cross-origin GET reads allowed for {cors_origins:?}");
    }
    let state = AppState::with_options(
        storage,
        ident,
        registry_server::serve::ServeOptions { cors_origins },
    );
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

/// The namespaces `add-publisher` grants, and whether the operator confirmed
/// that a grant of Tilo's own namespace is meant.
struct NamespaceGrant {
    namespaces: Vec<String>,
    first_party: bool,
}

impl NamespaceGrant {
    /// The granted namespaces, once each is valid and any first-party one is
    /// confirmed. An empty list is refused: a row that owns nothing is never
    /// what `add-publisher` was asked for.
    fn validated(self) -> anyhow::Result<Vec<String>> {
        use registry_types::namespace::{TILO_NAMESPACE, namespace_owns, valid_namespace};
        if self.namespaces.is_empty() {
            anyhow::bail!(
                "name at least one --namespace the publisher's package ids must lie in, such as com.example"
            );
        }
        if let Some(bad) = self
            .namespaces
            .iter()
            .find(|namespace| !valid_namespace(namespace))
        {
            anyhow::bail!("{bad:?} is not a reverse-DNS namespace such as com.example");
        }
        if let Some(first_party) = self.namespaces.iter().find(|namespace| {
            namespace.as_str() == TILO_NAMESPACE || namespace_owns(TILO_NAMESPACE, namespace)
        }) && !self.first_party
        {
            anyhow::bail!(
                "{first_party:?} is Tilo's own namespace; pass --first-party to confirm this key publishes first-party apps"
            );
        }
        Ok(self.namespaces)
    }
}

/// `Cmd::AddPublisher` — add or replace one allowlist row.
fn cmd_add_publisher(
    storage: &Storage,
    pubkey: &str,
    can_add_new: bool,
    note: String,
    grant: NamespaceGrant,
) -> anyhow::Result<()> {
    let namespaces = grant.validated()?;
    let transaction = storage.lock_snapshot()?;
    refuse_active_promotion(storage)?;
    let mut allow: PublisherAllowlist = storage
        .load_allowlist()
        .map_err(|e| anyhow::anyhow!("load allowlist: {e}"))?;
    let row = PublisherRow {
        pubkey_hex: pubkey.to_string(),
        can_add_new,
        note,
        namespaces,
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
fn cmd_applications(storage: &Storage, command: ApplicationsCommand) -> anyhow::Result<()> {
    use registry_server::applications::{self, ApplicationStatus, NetworkProofs};
    match command {
        ApplicationsCommand::List { json } => {
            let mut waiting: Vec<_> = storage
                .load_publisher_applications()?
                .into_iter()
                .filter(|stored| stored.status == ApplicationStatus::Pending)
                .collect();
            waiting.sort_by_key(|stored| stored.received_at_unix);
            if json {
                println!("{}", serde_json::to_string_pretty(&waiting)?);
                return Ok(());
            }
            for stored in &waiting {
                println!(
                    "{}  {}  {}",
                    stored.application.pubkey_hex,
                    stored.namespaces().join(","),
                    stored.application.contact
                );
                for claim in &stored.verified {
                    println!("    {} via {}", claim.namespace, claim.location);
                }
                if !stored.application.note.is_empty() {
                    println!("    note: {}", stored.application.note);
                }
            }
        }
        ApplicationsCommand::Show { pubkey, json } => {
            let stored = storage
                .load_publisher_application(&pubkey)?
                .ok_or_else(|| applications::ApplicationFailure::Unknown(pubkey.clone()))?;
            let review_id = stored.review_id()?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "review_id": review_id, "application": stored,
                    }))?
                );
            } else {
                println!(
                    "{}  {:?}  {}",
                    pubkey,
                    stored.status,
                    stored.namespaces().join(", ")
                );
                println!("review id: {review_id}");
                println!(
                    "contact: {}\nnote: {}",
                    stored.application.contact, stored.application.note
                );
            }
        }
        ApplicationsCommand::Approve {
            pubkey,
            reviewer,
            review_id,
        } => {
            let candidate = reviewed_application(storage, &pubkey, review_id.as_deref())?;
            let (_, row) = applications::approve_reviewed(
                storage,
                &candidate,
                &reviewer,
                unix_now(),
                &NetworkProofs::default(),
            )?;
            info!(
                "allowlisted {} for {}",
                row.pubkey_hex,
                row.namespaces.join(", ")
            );
        }
        ApplicationsCommand::Reject {
            pubkey,
            reviewer,
            reason,
            review_id,
        } => {
            let candidate = reviewed_application(storage, &pubkey, review_id.as_deref())?;
            applications::reject_reviewed(storage, &candidate, &reviewer, &reason, unix_now())?;
            info!("rejected the application from {pubkey}");
        }
    }
    Ok(())
}

fn reviewed_application(
    storage: &Storage,
    pubkey: &str,
    review_id: Option<&str>,
) -> anyhow::Result<registry_server::applications::StoredApplication> {
    use registry_server::applications::{ApplicationFailure, ApplicationStatus};
    let stored = storage
        .load_publisher_application(pubkey)?
        .ok_or_else(|| ApplicationFailure::Unknown(pubkey.to_owned()))?;
    if stored.status != ApplicationStatus::Pending {
        return Err(ApplicationFailure::NotPending(pubkey.to_owned()).into());
    }
    if let Some(expected) = review_id
        && expected != stored.review_id()?
    {
        return Err(ApplicationFailure::Changed(pubkey.to_owned()).into());
    }
    Ok(stored)
}

fn cmd_list_publishers(storage: &Storage) -> anyhow::Result<()> {
    let allow: PublisherAllowlist = storage
        .load_allowlist()
        .map_err(|e| anyhow::anyhow!("load allowlist: {e}"))?;
    let s = allow
        .to_toml()
        .map_err(|e| anyhow::anyhow!("encode allowlist: {e}"))?;
    print!("{s}");
    allow.warn_rows_without_namespaces();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use registry_types::{RegistryEntry, RegistryTarget, RegistryVersion, sign_entry};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    #[test]
    fn bounded_parallel_map_orders_bounds_and_stops_on_error() {
        // phase: bounded_parallel_map_preserves_order_and_respects_the_bound
        {
            let active = Arc::new(AtomicUsize::new(0));
            let peak = Arc::new(AtomicUsize::new(0));
            let parallelism =
                NonZeroUsize::new(DEFAULT_FIRST_PARTY_COLLECTION_PARALLELISM).unwrap();
            let barrier = Arc::new(Barrier::new(parallelism.get()));
            let inputs = (0..8).collect::<Vec<_>>();
            let output = bounded_parallel_map(&inputs, parallelism, |value| -> Result<_, ()> {
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(current, Ordering::SeqCst);
                barrier.wait();
                active.fetch_sub(1, Ordering::SeqCst);
                Ok(value * 2)
            })
            .unwrap();

            assert_eq!(
                output,
                inputs.iter().map(|value| value * 2).collect::<Vec<_>>()
            );
            assert_eq!(peak.load(Ordering::SeqCst), parallelism.get());
        }
        // phase: bounded_parallel_map_stops_before_the_next_batch_after_an_error
        {
            let calls = AtomicUsize::new(0);
            let error = bounded_parallel_map(
                &[0, 1, 2, 3, 4, 5],
                NonZeroUsize::new(4).unwrap(),
                |value| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    if *value == 1 {
                        Err("rejected")
                    } else {
                        Ok(*value)
                    }
                },
            )
            .unwrap_err();

            assert_eq!(error, "rejected");
            assert_eq!(calls.load(Ordering::SeqCst), 4);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn artifact_collection_stages_complete_submissions_in_sorted_batches() {
        // phase: collected_artifacts_are_sorted_and_duplicate_bytes_are_safe
        {
            let storage_dir = tempfile::tempdir().unwrap();
            let storage = Storage::open(storage_dir.path()).unwrap();
            let publisher = ed25519_dalek::SigningKey::from_bytes(&[43; 32]);
            let publisher_hex = hex::encode(publisher.verifying_key().to_bytes());
            let target = |architecture: &str| RegistryTarget {
                platform: registry_types::TILO_OS_PLATFORM.into(),
                architecture: architecture.into(),
                isa: architecture.into(),
                runtime: None,
                interfaces: Vec::new(),
            };
            let shared_bytes = b"same verified tapp".to_vec();
            let digest = blake3::hash(&shared_bytes).to_hex().to_string();
            let submissions = ["so.tilo.zeta", "so.tilo.alpha"]
                .into_iter()
                .map(|package_id| {
                    let mut entry = publisher_entry(&publisher);
                    entry.package_id = package_id.into();
                    sign_entry(&mut entry, &publisher);
                    registry_server::PendingSubmission {
                        submission_id: package_id.into(),
                        submitted_at_unix: 1,
                        change: registry_types::sign_change_request(
                            EntryAction::Add,
                            entry,
                            &publisher,
                        ),
                    }
                })
                .collect::<Vec<_>>();
            let persisted = Mutex::new(BTreeSet::new());
            let (staged, failures) = stage_collected_promotion(
                &storage,
                publisher_hex,
                submissions,
                NonZeroUsize::new(2).unwrap(),
                |pending| {
                    persist_distinct_artifact(&storage, &persisted, &digest, &shared_bytes)?;
                    let architecture = if pending.submission_id.ends_with("alpha") {
                        "aarch64"
                    } else {
                        "x86_64"
                    };
                    Ok(vec![StagedArtifact {
                        submission_id: pending.submission_id.clone(),
                        version: "1.0.0".into(),
                        target: Some(target(architecture)),
                        blake3_hex: digest.clone(),
                    }])
                },
            )
            .unwrap();
            assert!(failures.is_empty());
            let staged = staged.unwrap();

            assert_eq!(
                staged
                    .artifacts
                    .iter()
                    .map(|artifact| artifact.submission_id.as_str())
                    .collect::<Vec<_>>(),
                ["so.tilo.alpha", "so.tilo.zeta"]
            );
            assert!(
                staged
                    .artifacts
                    .iter()
                    .all(|artifact| artifact.blake3_hex == digest)
            );
            assert_eq!(storage.load_staged_artifact(&digest).unwrap(), shared_bytes);
        }
        // phase: a_failed_submission_stays_out_of_the_staged_batch
        {
            let storage_dir = tempfile::tempdir().unwrap();
            let storage = Storage::open(storage_dir.path()).unwrap();
            let publisher = ed25519_dalek::SigningKey::from_bytes(&[44; 32]);
            let publisher_hex = hex::encode(publisher.verifying_key().to_bytes());
            let submissions = ["so.tilo.alpha", "so.tilo.beta"]
                .into_iter()
                .map(|package_id| {
                    let mut entry = publisher_entry(&publisher);
                    entry.package_id = package_id.into();
                    sign_entry(&mut entry, &publisher);
                    registry_server::PendingSubmission {
                        submission_id: package_id.into(),
                        submitted_at_unix: 1,
                        change: registry_types::sign_change_request(
                            EntryAction::Add,
                            entry,
                            &publisher,
                        ),
                    }
                })
                .collect::<Vec<_>>();
            storage
                .save_staged_promotion(&StagedPromotion {
                    publisher_pubkey_hex: publisher_hex.clone(),
                    collected_at_unix: 1,
                    submissions: Vec::new(),
                    artifacts: Vec::new(),
                })
                .unwrap();
            let bytes = b"verified first artifact".to_vec();
            let digest = blake3::hash(&bytes).to_hex().to_string();
            let persisted = Mutex::new(BTreeSet::new());

            let (staged, failures) = stage_collected_promotion(
                &storage,
                publisher_hex,
                submissions.clone(),
                NonZeroUsize::new(1).unwrap(),
                |pending| {
                    if pending.submission_id.ends_with("beta") {
                        anyhow::bail!("second fetch failed");
                    }
                    persist_distinct_artifact(&storage, &persisted, &digest, &bytes)?;
                    Ok(vec![StagedArtifact {
                        submission_id: pending.submission_id.clone(),
                        version: "1.0.0".into(),
                        target: Some(
                            pending.change.entry.versions[0]
                                .tapp()
                                .unwrap()
                                .target
                                .clone(),
                        ),
                        blake3_hex: digest.clone(),
                    }])
                },
            )
            .unwrap();

            let [(failed_id, error)] = failures.as_slice() else {
                panic!("expected one failed submission");
            };
            assert_eq!(failed_id, "so.tilo.beta");
            assert!(error.to_string().contains("second fetch failed"));
            let staged = staged.unwrap();
            assert_eq!(staged.submissions, submissions[..1]);
            assert_eq!(storage.load_staged_promotion().unwrap(), Some(staged));
            assert_eq!(storage.load_staged_artifact(&digest).unwrap(), bytes);

            // A crash retry starts with a fresh in-memory digest set. Re-saving the
            // same content-addressed bytes verifies and reuses the durable artifact.
            persist_distinct_artifact(&storage, &Mutex::new(BTreeSet::new()), &digest, &bytes)
                .unwrap();
        }
        // phase: failed_collection_invalidates_an_older_staged_batch
        {
            let storage_dir = tempfile::tempdir().unwrap();
            let storage = Storage::open(storage_dir.path()).unwrap();
            let publisher = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
            let publisher_hex = hex::encode(publisher.verifying_key().to_bytes());
            storage
                .save_allowlist(&PublisherAllowlist {
                    publishers: vec![PublisherRow {
                        pubkey_hex: publisher_hex.clone(),
                        can_add_new: true,
                        note: "first party".into(),
                        namespaces: vec![registry_types::namespace::TILO_NAMESPACE.to_owned()],
                    }],
                })
                .unwrap();
            storage
                .save_staged_promotion(&StagedPromotion {
                    publisher_pubkey_hex: publisher_hex.clone(),
                    collected_at_unix: 1,
                    submissions: Vec::new(),
                    artifacts: Vec::new(),
                })
                .unwrap();

            let mut entry = publisher_entry(&publisher);
            entry.versions[0].tapp_mut().unwrap().tapp_url =
                "file:///untrusted/package.tapp".into();
            sign_entry(&mut entry, &publisher);
            storage
                .save_pending(&registry_server::PendingSubmission {
                    submission_id: entry.package_id.clone(),
                    submitted_at_unix: 2,
                    change: registry_types::sign_change_request(
                        EntryAction::Add,
                        entry,
                        &publisher,
                    ),
                })
                .unwrap();

            assert!(cmd_collect_first_party(&storage, &publisher_hex, 50, 4, false).is_err());
            assert!(storage.load_staged_promotion().unwrap().is_none());
        }
        // phase: staged_artifacts_are_selected_by_version_and_target
        {
            let target = |architecture: &str, isa: &str| RegistryTarget {
                platform: registry_types::TILO_OS_PLATFORM.into(),
                architecture: architecture.into(),
                isa: isa.into(),
                runtime: None,
                interfaces: Vec::new(),
            };
            let key = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
            let mut entry = publisher_entry(&key);
            let mut arm = entry.versions[0].clone();
            arm.tapp_mut().unwrap().target = target("aarch64", "aarch64-v8.2");
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
                    matches,
                    1,
                    "{} must select exactly one staged artifact",
                    version.tapp().unwrap().target.architecture
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
    }

    fn publisher_entry(key: &ed25519_dalek::SigningKey) -> RegistryEntry {
        let mut entry = RegistryEntry {
            package_id: "so.tilo.concurrent".into(),
            kind: registry_types::EntryKind::App,
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
                released_at_unix: 1,
                artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
                    tapp_url: "https://apps.tilo.so/artifacts/concurrent.tapp".into(),
                    tapp_root_hash_hex: "ab".repeat(32),
                    tapp_size_bytes: 1,
                    tapp_blake3_hex: "cd".repeat(32),
                    target: RegistryTarget {
                        platform: registry_types::TILO_OS_PLATFORM.into(),
                        architecture: "x86_64".into(),
                        isa: "x86_64-v3".into(),
                        runtime: None,
                        interfaces: Vec::new(),
                    },
                }),
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
        cmd_add_publisher(
            &storage,
            &publisher_hex,
            true,
            String::new(),
            NamespaceGrant {
                namespaces: vec![registry_types::namespace::TILO_NAMESPACE.to_owned()],
                first_party: true,
            },
        )
        .unwrap();
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
        let response = tilo_net::http::client()
            .build()
            .unwrap()
            .get(format!("http://{bound}/entries"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let served: SignedRegistrySnapshot = response.json().await.unwrap();
        assert_eq!(served, third);
    }

    #[test]
    fn owner_signature_moves_a_registry_from_another_package_format() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let owner = ed25519_dalek::SigningKey::from_bytes(&[61; 32]);
        let publisher = ed25519_dalek::SigningKey::from_bytes(&[62; 32]);
        let owner_hex = hex::encode(owner.verifying_key().to_bytes());
        storage
            .save_identity(&ServerIdentity {
                owner_pubkey_hex: owner_hex.clone(),
                name: "test".into(),
                description: "test".into(),
                base_url: "http://127.0.0.1".into(),
            })
            .unwrap();
        cmd_add_publisher(
            &storage,
            &hex::encode(publisher.verifying_key().to_bytes()),
            true,
            String::new(),
            NamespaceGrant {
                namespaces: vec![registry_types::namespace::TILO_NAMESPACE.to_owned()],
                first_party: true,
            },
        )
        .unwrap();
        let entry = publisher_entry(&publisher);
        storage.save_entry(&entry).unwrap();

        // The published snapshot was signed by a build at an earlier format,
        // which this build cannot verify.
        let mut earlier =
            build_signed_snapshot(&storage, None, vec![entry.clone()], &owner, 3_600).unwrap();
        earlier.protocol = PACKAGE_FORMAT_VERSION - 1;
        let writer = storage.lock_snapshot().unwrap();
        writer.publish(&earlier).unwrap();
        drop(writer);
        assert_eq!(
            registry_types::verify_snapshot(&earlier, &owner_hex, earlier.issued_at_unix),
            Err(registry_types::SnapshotError::Protocol)
        );

        let key_file = dir.path().join("owner.seed");
        std::fs::write(&key_file, hex::encode(owner.to_bytes())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        cmd_sign_snapshot(&storage, &key_file, 3_600).unwrap();

        let current = storage.load_snapshot().unwrap().unwrap();
        assert_eq!(current.protocol, PACKAGE_FORMAT_VERSION);
        assert_eq!(current.sequence, earlier.sequence + 1);
        assert_eq!(
            current.previous_snapshot_hash_hex,
            registry_types::snapshot_hash(&earlier).unwrap()
        );
        assert_eq!(current.entries, vec![entry]);
        registry_types::verify_snapshot(&current, &owner_hex, current.issued_at_unix).unwrap();

        // At this build's format the predecessor must verify, as before.
        let mut forged =
            build_signed_snapshot(&storage, Some(&current), Vec::new(), &owner, 3_600).unwrap();
        forged.owner_signature_hex = "00".repeat(64);
        let writer = storage.lock_snapshot().unwrap();
        writer.publish(&forged).unwrap();
        drop(writer);
        assert!(cmd_sign_snapshot(&storage, &key_file, 3_600).is_err());
        assert_eq!(storage.load_snapshot().unwrap().unwrap(), forged);
    }
}
