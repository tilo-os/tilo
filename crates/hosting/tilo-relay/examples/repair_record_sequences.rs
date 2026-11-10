//! Repair a record whose history holds two different changes under one
//! writer's sequence, once, without losing what either of them wrote.
//!
//! Automerge numbers each writer's changes, and a document holds one change
//! under each writer's sequence. Two copies of a record that wrote under one
//! writer (two pages of a profile that reused the writer a saved row carried,
//! or a bot reopened under a writer named after it) each numbered their next
//! change the same, and a sealed record's history keeps every write it took
//! in. Such a record no longer reads whole: the host's read keeps the change
//! of each sequence that reached it first and leaves the other write, and
//! everything built on it, out, or refuses the read altogether; a member
//! joining is not served it.
//!
//! With the host stopped, this opens the host's workspaces as the host does
//! (from its state, its root file and its device key, under the certificate
//! the host signs at every start) on an endpoint nothing outside can reach,
//! and for every record that holds a sequence twice:
//!
//! - reads the record's whole history: its base and every write, each opened
//!   with the channel keys the host holds, none merged;
//! - names each writer and sequence that holds two changes, and keeps for each
//!   the change that reached the record first, as every reader already does:
//!   that branch, with everything that builds on it, is the record as members
//!   read it now;
//! - reads each other branch (the change it left out and everything built on
//!   it) and carries what it wrote that the kept record lacks (messages,
//!   attachments, edits, deletions) forward onto the kept record;
//! - compacts the record's sealed history into the kept branch, as the host
//!   compacts any sealed history it governs (one sealed checkpoint every
//!   member takes in place of exactly the writes it covers), and then writes
//!   what it carried forward as the host's own change, through the host's
//!   normal authoring path, under a writer of its own: every member admits
//!   both as the host's.
//!
//! It prints what it found and did: each record, the writers and sequences
//! held twice, the changes it kept and those whose content it wrote again,
//! and how many messages the record shows before and after. It then starts
//! the host again on the repaired state and reads each repaired record back
//! as the host will serve it: whole, and with how many messages.
//!
//! A record whose history holds writes that build on changes it never received
//! (and so never read whole) is repaired the same way, with nothing to carry:
//! it is compacted into what does read.
//!
//! The compaction replaces the writes it covers in the host's state, as any
//! compaction does. Every change the repaired record leaves out (the other
//! branches' and the ones waiting on missing changes) is first kept, as it was
//! written, under `<state>/repaired-records/`, so none is lost: one a missing
//! change turns up for can be taken in again.
//!
//! Build it for the host, and run it as the user that owns the state, after
//! a fresh backup and with the host stopped, first with `--dry-run`, which
//! changes nothing:
//!
//! ```text
//! cargo build --release -p tilo-relay --features agents --example repair_record_sequences
//! repair_record_sequences --state /srv/lounge --root-file /etc/lounge/root --persistent lounge --dry-run
//! repair_record_sequences --state /srv/lounge --root-file /etc/lounge/root --persistent lounge
//! ```
//!
//! `--workspace ID` and `--app APP` narrow it to one workspace or one app's
//! records. Running it again is safe: a repaired record holds each sequence
//! once and is left alone. It exits non-zero when a record could not be
//! repaired, or does not read whole after its repair.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context as _;
use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ChangeHash, ObjId, ObjType, ReadDoc as _, hydrate};
use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use tilo_room_host::workspaces::Workspaces;
use tilo_user_identity::{DeviceCert, RevocationList, sign_device_cert};

const CHAT: &str = "so.tilo.chat";
/// Where the changes a repaired record leaves out are kept, under the host's
/// state: `<workspace>/<app>/<record>/<hash>.change`, each as it was written.
const ARCHIVE: &str = "repaired-records";

struct Args {
    state: PathBuf,
    root_file: PathBuf,
    /// The workspace's name, as `tilo-relay room --persistent` names it.
    persistent: String,
    workspace: Option<String>,
    app: Option<String>,
    dry_run: bool,
}

fn args() -> anyhow::Result<Args> {
    let mut state = None;
    let mut root_file = None;
    let mut persistent = None;
    let mut workspace = None;
    let mut app = None;
    let mut dry_run = false;
    let mut given = std::env::args().skip(1);
    while let Some(arg) = given.next() {
        match arg.as_str() {
            "--state" => state = given.next().map(PathBuf::from),
            "--root-file" => root_file = given.next().map(PathBuf::from),
            "--persistent" => persistent = given.next(),
            "--workspace" => workspace = given.next(),
            "--app" => app = given.next(),
            "--dry-run" => dry_run = true,
            other => anyhow::bail!(
                "unknown argument {other:?}: --state DIR --root-file FILE --persistent NAME [--workspace ID] [--app APP] [--dry-run]"
            ),
        }
    }
    Ok(Args {
        state: state.context("--state: the directory `tilo-relay room --state` names")?,
        root_file: root_file
            .context("--root-file: the file `tilo-relay room --root-file` names")?,
        persistent: persistent
            .context("--persistent: the name `tilo-relay room --persistent` gives the workspace")?,
        workspace,
        app,
        dry_run,
    })
}

fn log_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "warn,tilo_channels=info".into())
}

fn main() -> ExitCode {
    // The host's workspaces keep their documents in workers that re-enter
    // this executable; serve them first.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return ExitCode::from(code);
    }
    // So do the confined workers the host runs apps in.
    if let Some(code) = tilo_agent_participant::worker::dispatch_early() {
        return ExitCode::from(code);
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            // The channel service says at info when it upgrades the
            // channel state an earlier build saved, which the first program
            // to open the host's channels after an upgrade does.
            log_filter(),
        )
        .with_writer(std::io::stderr)
        .try_init();
    let run = args().and_then(|args| {
        tilo_rt::app()
            .block_on(Box::pin(run(&args)))
            .map_err(|error| anyhow::anyhow!("{error:?}"))?
    });
    match run {
        Ok(reports) => {
            if reports.iter().all(RecordReport::succeeded) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::from(2)
        }
    }
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "the host stays open until every record is repaired, then read back"
)]
async fn run(args: &Args) -> anyhow::Result<Vec<RecordReport>> {
    let relay_root = tilo_relay_protocol::metrics::room_relay_root(&args.state);
    // Nothing else runs on this state while it is repaired.
    let _lock = tilo_relay::state_lock::StateLock::acquire(&relay_root)
        .map_err(anyhow::Error::msg)
        .context("the host must be stopped")?;
    let root = read_root(&args.root_file)?;
    let host = read_host_key(&args.state.join("host-device.key"))?;
    let relay = open_host(args, &relay_root, &root, &host).await?;
    let workspaces = relay
        .workspaces()
        .context("the host governs no workspaces")?
        .clone();
    let mut reports = Vec::new();
    for summary in workspaces.list().await {
        if args
            .workspace
            .as_ref()
            .is_some_and(|only| *only != summary.workspace)
        {
            continue;
        }
        let found = repair_workspace(
            &workspaces,
            &summary.workspace,
            args.app.as_deref(),
            args.dry_run,
            &relay_root.join(ARCHIVE),
        )
        .await?;
        if found.is_empty() {
            println!(
                "{}: every record holds each writer's sequence once",
                summary.workspace
            );
        }
        reports.extend(found);
    }
    drop(workspaces);
    relay.shutdown().await;
    if reports.iter().any(|report| report.repaired) {
        // What the host serves from now on is what it reads when it starts
        // on the repaired state: started again, it is read back from there.
        println!("reading the repaired records back as the host starts on them");
        let relay = open_host(args, &relay_root, &root, &host).await?;
        let workspaces = relay
            .workspaces()
            .context("the host governs no workspaces")?
            .clone();
        verify(&workspaces, &mut reports).await;
        drop(workspaces);
        relay.shutdown().await;
    }
    Ok(reports)
}

/// The host's workspaces opened as `tilo-relay room --persistent` opens them,
/// under the certificate it signs, on an endpoint bound to this machine alone
/// and found by nobody: no member reaches the host while its records are
/// repaired.
async fn open_host(
    args: &Args,
    relay_root: &Path,
    root: &SigningKey,
    host: &SigningKey,
) -> anyhow::Result<tilo_relay::Relay> {
    let cert = host_cert(root, host, &args.persistent)?;
    let scratch = tempfile::tempdir()?;
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&host.to_bytes()))
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))?
        .bind()
        .await?;
    tilo_relay::Relay::start_on_endpoint_with_protocols(
        tilo_relay::RelayConfig {
            state_root: relay_root.to_path_buf(),
            identity_root: args.state.join("identity"),
            full_replica_status: scratch.path().join("status.json"),
            home_peer_bundle: scratch.path().join("home-peer.toml"),
            ..Default::default()
        },
        tilo_relay::daemon::RelayIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: host.clone(),
            device_cert: cert,
            revocations: RevocationList::new(),
        },
        endpoint,
        Vec::new(),
    )
    .await
    .map_err(|error| anyhow::anyhow!("open the host's workspaces: {error}"))
}

/// What one record held and what was done about it.
#[derive(Debug, Default)]
struct RecordReport {
    workspace: String,
    app: String,
    record: String,
    /// Distinct changes in the record's history.
    changes: usize,
    /// Writes of the history that do not open here, with why.
    unreadable: Vec<String>,
    /// Each writer (hex) and sequence held twice, with the change kept
    /// first and the changes left out of the record as it reads.
    collisions: Vec<Collision>,
    /// Changes of the left-out branches whose content was written again.
    carried: Vec<ChangeHash>,
    /// Changes in no branch: they build on changes nobody holds.
    waiting: usize,
    /// Messages (or, for a record that is no part of a chat, its keys at
    /// the top) as the record reads before, in every branch together, and
    /// after.
    before: Option<usize>,
    union: Option<usize>,
    after: Option<usize>,
    /// What the branches wrote differently at one place, kept as the kept
    /// branch has it.
    conflicts: Vec<String>,
    repaired: bool,
    /// How many left-out changes were kept, and where.
    archived: Option<(usize, PathBuf)>,
    reads_whole: Option<bool>,
    failed: Option<String>,
}

#[derive(Debug)]
struct Collision {
    actor: String,
    seq: u64,
    kept: ChangeHash,
    left_out: Vec<ChangeHash>,
    /// Each of its changes, kept first: when its writer says it wrote it
    /// (Unix milliseconds) and the write it reached the record in.
    when: Vec<String>,
}

impl RecordReport {
    fn succeeded(&self) -> bool {
        self.failed.is_none() && self.reads_whole != Some(false)
    }

    fn print(&self, dry_run: bool) {
        println!(
            "{} {}/{}: {} changes; {} writer sequences held twice",
            self.workspace,
            self.app,
            if self.record.is_empty() {
                "(primary)"
            } else {
                &self.record
            },
            self.changes,
            self.collisions.len()
        );
        for collision in &self.collisions {
            println!(
                "  writer {} sequence {}: kept {}, left out {}",
                collision.actor,
                collision.seq,
                short(&collision.kept),
                collision
                    .left_out
                    .iter()
                    .map(short)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            for when in &collision.when {
                println!("    {when}");
            }
        }
        println!(
            "  {} changes of the left-out branches {} again as the host's own change",
            self.carried.len(),
            if dry_run {
                "would have their content written"
            } else {
                "had their content written"
            },
        );
        for hash in &self.carried {
            println!("    {hash}");
        }
        if self.waiting > 0 {
            println!(
                "  {} changes build on changes no copy here holds; they are left out of the repaired record and kept beside it",
                self.waiting
            );
        }
        if let Some((count, at)) = &self.archived {
            println!("  {count} left-out changes kept in {}", at.display());
        }
        for unreadable in &self.unreadable {
            println!("  a write does not open here: {unreadable}");
        }
        for conflict in &self.conflicts {
            println!("  both branches wrote {conflict}: the kept branch's value stays");
        }
        let count = |count: Option<usize>| count.map_or_else(|| "-".to_owned(), |n| n.to_string());
        println!(
            "  messages: {} as it reads now, {} across every branch, {} {}",
            count(self.before),
            count(self.union),
            count(self.after),
            if dry_run {
                "once repaired"
            } else {
                "after the repair"
            },
        );
        match (&self.failed, self.repaired) {
            (Some(why), _) => println!("  NOT repaired: {why}"),
            (None, true) => println!("  repaired; read back once the host starts again"),
            (None, false) if dry_run => println!("  dry run: nothing changed"),
            (None, false) => println!("  nothing to do"),
        }
    }

    /// What the host started again on the repaired state reads of it.
    fn print_verified(&self) {
        let name = if self.record.is_empty() {
            "(primary)"
        } else {
            &self.record
        };
        let messages = self
            .after
            .map_or_else(|| "-".to_owned(), |after| after.to_string());
        match (&self.failed, self.reads_whole) {
            (Some(why), _) => println!(
                "{} {}/{name}: NOT whole after the repair: {why}",
                self.workspace, self.app
            ),
            (None, Some(true)) => println!(
                "{} {}/{name}: reads whole, {messages} messages",
                self.workspace, self.app
            ),
            (None, _) => println!("{} {}/{name}: was not read back", self.workspace, self.app),
        }
    }
}

fn short(hash: &ChangeHash) -> String {
    hash.to_string().chars().take(12).collect()
}

/// Find and repair each record of `workspace` (of `app` only, when named)
/// that holds a writer's sequence twice.
///
/// # Errors
/// The workspace is not governed here.
async fn repair_workspace(
    workspaces: &Workspaces,
    workspace: &str,
    app: Option<&str>,
    dry_run: bool,
    archive: &Path,
) -> anyhow::Result<Vec<RecordReport>> {
    let mut reports = Vec::new();
    let routes = workspaces
        .record_routes(workspace)
        .await
        .map_err(|error| anyhow::anyhow!("{workspace}: {error}"))?;
    for (record_app, record) in routes {
        if app.is_some_and(|only| only != record_app) {
            continue;
        }
        let history = match workspaces
            .record_history(workspace, &record_app, &record)
            .await
        {
            Ok(Some(history)) => history,
            Ok(None) => continue,
            // A person's own record, sealed for their devices alone (their
            // drafts), is not the host's to read, nor to repair.
            Err(_) if tilo_action_policy::member_record(&record).is_some() => continue,
            Err(error) => {
                let report = RecordReport {
                    workspace: workspace.to_owned(),
                    app: record_app.clone(),
                    record: record.clone(),
                    failed: Some(format!("its history does not read here: {error}")),
                    ..RecordReport::default()
                };
                report.print(dry_run);
                reports.push(report);
                continue;
            }
        };
        let mut report = RecordReport {
            workspace: workspace.to_owned(),
            app: record_app.clone(),
            record: record.clone(),
            ..RecordReport::default()
        };
        let analysis = match Analysis::of(&history) {
            Ok(analysis) => analysis,
            Err(error) => {
                report.failed = Some(format!("its history does not decode: {error}"));
                report.print(dry_run);
                reports.push(report);
                continue;
            }
        };
        // A record holding each sequence once, and every change of it in
        // place, reads whole: nothing to do.
        if analysis.collisions.is_empty() && analysis.waiting == 0 {
            continue;
        }
        analysis.describe(&mut report);
        if !report.unreadable.is_empty() {
            report.failed = Some(
                "writes of its history do not open with the keys this host holds; repairing it would lose them"
                    .into(),
            );
        } else if !dry_run
            && let Err(error) = apply(workspaces, &analysis, &mut report, archive).await
        {
            report.failed = Some(format!("{error:#}"));
        }
        report.print(dry_run);
        reports.push(report);
    }
    Ok(reports)
}

/// Found the record again from its kept branch, then write what the other
/// branches carried as the host's own change, and read it back.
async fn apply(
    workspaces: &Workspaces,
    analysis: &Analysis,
    report: &mut RecordReport,
    archive: &Path,
) -> anyhow::Result<()> {
    let (workspace, app, record) = (
        report.workspace.as_str(),
        report.app.as_str(),
        report.record.as_str(),
    );
    // Every change the repaired record leaves out is kept first, as it was
    // written: a change it waits for may still turn up, and the compaction
    // below takes them out of the host's state.
    let kept_at = archive
        .join(workspace)
        .join(app)
        .join(if record.is_empty() {
            "(primary)"
        } else {
            record
        });
    std::fs::create_dir_all(&kept_at).with_context(|| format!("create {}", kept_at.display()))?;
    for change in &analysis.left_out {
        let path = kept_at.join(format!("{}.change", change.hash()));
        std::fs::write(&path, change.raw_bytes())
            .with_context(|| format!("keep {}", path.display()))?;
    }
    report.archived = Some((analysis.left_out.len(), kept_at.clone()));
    let mut kept = analysis.kept.clone();
    workspaces
        .compact_record_into(workspace, app, record, kept.save())
        .await
        .map_err(|error| anyhow::anyhow!("compact the record into its kept branch: {error}"))?;
    let governor = workspaces.serving_certificate().clone();
    let branches = analysis.branches.clone();
    workspaces
        .author_room_record(workspace, &governor, (app, record), |doc| {
            for branch in &branches {
                carry(doc, &branch.ancestor, &branch.document)
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        })
        .await
        .map_err(|error| anyhow::anyhow!("write what the other branches carried: {error}"))?;
    report.repaired = true;
    Ok(())
}

/// Read each repaired record back from the host started again on the
/// repaired state, as it serves it from now on: whether it reads whole, and
/// how many messages it shows.
async fn verify(workspaces: &Workspaces, reports: &mut [RecordReport]) {
    for report in reports.iter_mut().filter(|report| report.repaired) {
        let (workspace, app, record) = (
            report.workspace.as_str(),
            report.app.as_str(),
            report.record.as_str(),
        );
        let only = BTreeSet::from([(app.to_owned(), record.to_owned())]);
        match workspaces.whole_records(workspace, Some(&only)).await {
            Ok(whole) => {
                report.reads_whole = Some(whole.is_whole());
                if let Some((_, why)) = whole.unread.first() {
                    report.failed = Some(format!(
                        "the record does not read whole after the repair: {why}"
                    ));
                }
            }
            Err(error) => report.failed = Some(format!("the record does not read: {error}")),
        }
        if let Ok(Some(bytes)) = workspaces.record_document(workspace, app, record).await
            && let Ok(after) = AutoCommit::load(&bytes)
        {
            report.after = Some(count(app, record, &after).len());
        }
        report.print_verified();
    }
}

/// One branch a writer's repeated sequence left out of the record: the record
/// as that branch has it, and as both it and the kept branch have it.
#[derive(Clone)]
struct Branch {
    document: AutoCommit,
    ancestor: AutoCommit,
    /// Its changes the kept branch lacks.
    own: Vec<ChangeHash>,
}

/// A record's history sorted out.
struct Analysis {
    changes: usize,
    unreadable: Vec<String>,
    collisions: Vec<Collision>,
    /// The record with the change of each sequence that reached it first,
    /// and everything that builds on those.
    kept: AutoCommit,
    branches: Vec<Branch>,
    /// Changes no branch holds: they wait for changes nobody has.
    waiting: usize,
    /// Every change the kept record leaves out, as it was written: kept
    /// beside the state before the history is compacted, so none is lost.
    left_out: Vec<automerge::Change>,
}

/// Every distinct change of `history`, in the order it reached the record,
/// with where each reached it, and the writes that do not open here.
type Read = (
    Vec<automerge::Change>,
    BTreeMap<ChangeHash, String>,
    Vec<String>,
);

fn read_history(history: &tilo_peer_state::OpenedHistory) -> anyhow::Result<Read> {
    let mut changes: Vec<automerge::Change> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut unreadable = Vec::new();
    // Where each change reached the record: its base, or a write by
    // author and sequence (a member's sequence is the time it wrote).
    let mut origin: BTreeMap<ChangeHash, String> = BTreeMap::new();
    let mut take = |found: Vec<automerge::Change>, from: &str| {
        for change in found {
            if seen.insert(change.hash()) {
                origin.insert(change.hash(), from.to_owned());
                changes.push(change);
            }
        }
    };
    if !history.base.is_empty() {
        take(
            AutoCommit::load(&history.base)
                .context("the record's base")?
                .get_changes(&[]),
            "the record's base",
        );
    }
    for write in &history.writes {
        let from = format!(
            "write {}#{}",
            hex::encode(write.author.get(..8).unwrap_or_default()),
            write.seq
        );
        match &write.mutation {
            Ok(DocumentMutation::Snapshot(bytes)) => {
                take(
                    AutoCommit::load(bytes)
                        .context("a checkpoint")?
                        .get_changes(&[]),
                    &from,
                );
            }
            Ok(DocumentMutation::Incremental(bytes)) => {
                take(
                    automerge::Automerge::parse_changes(bytes).context("a write")?,
                    &from,
                );
            }
            Err(why) => unreadable.push(format!(
                "{}#{}: {why}",
                hex::encode(write.author.get(..8).unwrap_or_default()),
                write.seq
            )),
        }
    }
    Ok((changes, origin, unreadable))
}

impl Analysis {
    fn of(history: &tilo_peer_state::OpenedHistory) -> anyhow::Result<Self> {
        let (changes, origin, unreadable) = read_history(history)?;
        // Each writer's sequence, in the order its changes reached the record.
        let mut by_sequence: BTreeMap<(Vec<u8>, u64), Vec<usize>> = BTreeMap::new();
        for (index, change) in changes.iter().enumerate() {
            by_sequence
                .entry((change.actor_id().to_bytes().to_vec(), change.seq()))
                .or_default()
                .push(index);
        }
        by_sequence.retain(|_, held| held.len() > 1);
        let repeated: BTreeSet<usize> = by_sequence.values().flatten().copied().collect();
        let widest = by_sequence.values().map(Vec::len).max().unwrap_or(0);
        // The `k`th change of every repeated sequence, and everything else.
        let choose = |k: usize| -> BTreeSet<usize> {
            (0..changes.len())
                .filter(|index| !repeated.contains(index))
                .chain(
                    by_sequence
                        .values()
                        .filter_map(|held| held.get(k).or_else(|| held.first()).copied()),
                )
                .collect()
        };
        let (kept, kept_held) = build(&changes, &choose(0));
        let mut branches = Vec::new();
        let mut held_anywhere = kept_held.clone();
        for k in 1..widest {
            let (document, held) = build(&changes, &choose(k));
            let common: BTreeSet<usize> = kept_held.intersection(&held).copied().collect();
            let (ancestor, _) = build(&changes, &common);
            let own = held
                .difference(&kept_held)
                .filter_map(|index| changes.get(*index).map(automerge::Change::hash))
                .collect();
            held_anywhere.extend(held.iter().copied());
            branches.push(Branch {
                document,
                ancestor,
                own,
            });
        }
        let collisions = by_sequence
            .iter()
            .filter_map(|((actor, seq), held)| {
                let (first, rest) = held.split_first()?;
                Some(Collision {
                    actor: hex::encode(actor),
                    seq: *seq,
                    kept: changes.get(*first)?.hash(),
                    left_out: rest
                        .iter()
                        .filter_map(|index| changes.get(*index).map(automerge::Change::hash))
                        .collect(),
                    when: held
                        .iter()
                        .filter_map(|index| changes.get(*index))
                        .map(|change| {
                            format!(
                                "{} written at {} ms, reached the record in {}",
                                short(&change.hash()),
                                change.timestamp(),
                                origin.get(&change.hash()).map_or("?", String::as_str)
                            )
                        })
                        .collect(),
                })
            })
            .collect();
        Ok(Self {
            changes: changes.len(),
            unreadable,
            collisions,
            kept,
            branches,
            waiting: changes.len().saturating_sub(held_anywhere.len()),
            left_out: outside(&changes, &kept_held),
        })
    }

    fn describe(&self, report: &mut RecordReport) {
        report.changes = self.changes;
        report.unreadable.clone_from(&self.unreadable);
        report.collisions = self
            .collisions
            .iter()
            .map(|collision| Collision {
                actor: collision.actor.clone(),
                seq: collision.seq,
                kept: collision.kept,
                left_out: collision.left_out.clone(),
                when: collision.when.clone(),
            })
            .collect();
        report.carried = self
            .branches
            .iter()
            .flat_map(|branch| branch.own.iter().copied())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        report.waiting = self.waiting;
        let (app, record) = (report.app.as_str(), report.record.as_str());
        let mut union = count(app, record, &self.kept);
        report.before = Some(union.len());
        for branch in &self.branches {
            union.extend(count(app, record, &branch.document));
        }
        report.union = Some(union.len());
        // What the repair would make of it, worked out on a copy.
        let mut repaired = self.kept.clone();
        for branch in &self.branches {
            match carry(&mut repaired, &branch.ancestor, &branch.document) {
                Ok(conflicts) => report.conflicts.extend(conflicts),
                Err(error) => report
                    .conflicts
                    .push(format!("(cannot be carried: {error})")),
            }
        }
        report.after = Some(count(app, record, &repaired).len());
    }
}

/// The changes of `changes` outside `held`, as they were written.
fn outside(changes: &[automerge::Change], held: &BTreeSet<usize>) -> Vec<automerge::Change> {
    changes
        .iter()
        .enumerate()
        .filter(|(index, _)| !held.contains(index))
        .map(|(_, change)| change.clone())
        .collect()
}

/// The record made of the changes `include` names, applied in causal order;
/// a change whose dependencies are not among them stays out. With the
/// indexes of the changes it holds.
fn build(
    changes: &[automerge::Change],
    include: &BTreeSet<usize>,
) -> (AutoCommit, BTreeSet<usize>) {
    let mut document = AutoCommit::new();
    let mut held = BTreeSet::new();
    let mut hashes: BTreeSet<ChangeHash> = BTreeSet::new();
    let mut waiting: Vec<usize> = include.iter().copied().collect();
    loop {
        let before = waiting.len();
        waiting.retain(|index| {
            let Some(change) = changes.get(*index) else {
                return false;
            };
            if !change.deps().iter().all(|dep| hashes.contains(dep)) {
                return true;
            }
            if tilo_automerge::ApplyIncoming::apply_incoming(&mut document, [change.clone()])
                .is_ok()
            {
                hashes.insert(change.hash());
                held.insert(*index);
            }
            false
        });
        if waiting.len() == before {
            break;
        }
    }
    (document, held)
}

/// The messages of a part of a chat's messages, by id; for any other record,
/// the keys at its top.
fn count(app: &str, record: &str, doc: &AutoCommit) -> BTreeSet<String> {
    let part = ui::record::RecordId::parse(record)
        .is_some_and(|id| app == CHAT && chat::parts::part_index(&id).is_some());
    if part {
        return chat::messages::read_part(doc)
            .messages
            .ids()
            .map(ToString::to_string)
            .collect();
    }
    doc.keys(automerge::ROOT).collect()
}

/// Write onto `target` what `branch` changed since `ancestor`, the record as
/// both the target's branch and `branch` had it: what `branch` added that
/// `target` lacks, what it changed where `target` left the ancestor's value,
/// and what it deleted where `target` kept the ancestor's value. Where both
/// changed one place differently, `target` keeps its own and the place is
/// named. Returns those places.
fn carry(
    target: &mut AutoCommit,
    ancestor: &AutoCommit,
    branch: &AutoCommit,
) -> Result<Vec<String>, automerge::AutomergeError> {
    let read = |doc: &AutoCommit| -> Result<hydrate::Map, automerge::AutomergeError> {
        match doc.hydrate(automerge::ROOT, None)? {
            hydrate::Value::Map(map) => Ok(map),
            _ => Ok(hydrate::Map::default()),
        }
    };
    let ours = read(target)?;
    let base = read(ancestor)?;
    let theirs = read(branch)?;
    let mut conflicts = Vec::new();
    merge_map(
        target,
        &automerge::ROOT,
        (&ours, Some(&base), &theirs),
        "",
        &mut conflicts,
    )?;
    Ok(conflicts)
}

fn merge_map(
    target: &mut AutoCommit,
    object: &ObjId,
    (ours, base, theirs): (&hydrate::Map, Option<&hydrate::Map>, &hydrate::Map),
    path: &str,
    conflicts: &mut Vec<String>,
) -> Result<(), automerge::AutomergeError> {
    let mut keys: Vec<&String> = theirs.iter().map(|(key, _)| key).collect();
    keys.sort();
    for key in keys {
        let Some(their) = theirs.get(key) else {
            continue;
        };
        let at = format!("{path}/{key}");
        let base_value = base.and_then(|base| base.get(key));
        match ours.get(key) {
            None if base_value.is_none() => put(target, object, key.as_str(), their)?,
            // Deleted on the kept branch: it stays deleted.
            None => {}
            Some(our) if our == their => {}
            Some(our) => {
                if let (hydrate::Value::Map(our_map), hydrate::Value::Map(their_map)) = (our, their)
                {
                    let Some((automerge::Value::Object(ObjType::Map), child)) =
                        target.get(object, key.as_str())?
                    else {
                        continue;
                    };
                    let base_map = match base_value {
                        Some(hydrate::Value::Map(map)) => Some(map),
                        _ => None,
                    };
                    merge_map(
                        target,
                        &child,
                        (our_map, base_map, their_map),
                        &at,
                        conflicts,
                    )?;
                } else if base_value == Some(our) {
                    // Changed only on their branch.
                    put(target, object, key.as_str(), their)?;
                } else if base_value == Some(their) {
                    // Changed only on ours.
                } else if let (hydrate::Value::List(our_list), hydrate::Value::List(their_list)) =
                    (our, their)
                {
                    let Some((automerge::Value::Object(ObjType::List), child)) =
                        target.get(object, key.as_str())?
                    else {
                        continue;
                    };
                    let base_list = match base_value {
                        Some(hydrate::Value::List(list)) => Some(list),
                        _ => None,
                    };
                    merge_list(target, &child, (our_list, base_list, their_list))?;
                } else {
                    conflicts.push(at);
                }
            }
        }
    }
    for (key, our) in ours.iter() {
        if theirs.get(key).is_none() && base.and_then(|base| base.get(key)) == Some(&our.value) {
            // Deleted on their branch, untouched on ours.
            target.delete(object, key.as_str())?;
        }
    }
    Ok(())
}

/// A list both branches changed: what their branch added is appended, and
/// what it removed of the ancestor's is removed where ours still has it.
fn merge_list(
    target: &mut AutoCommit,
    object: &ObjId,
    (ours, base, theirs): (&hydrate::List, Option<&hydrate::List>, &hydrate::List),
) -> Result<(), automerge::AutomergeError> {
    let values = |list: &hydrate::List| -> Vec<hydrate::Value> {
        list.iter().map(|item| item.value.clone()).collect()
    };
    let (ours, theirs) = (values(ours), values(theirs));
    let base = base.map(values).unwrap_or_default();
    let mut current = ours;
    for removed in base.iter().filter(|value| !theirs.contains(value)) {
        if let Some(index) = current.iter().position(|value| value == removed) {
            target.delete(object, index)?;
            current.remove(index);
        }
    }
    for added in &theirs {
        if base.contains(added) || current.contains(added) {
            continue;
        }
        insert(target, object, current.len(), added)?;
        current.push(added.clone());
    }
    Ok(())
}

/// Write `value` under `key` of the map `object`.
fn put(
    target: &mut AutoCommit,
    object: &ObjId,
    key: &str,
    value: &hydrate::Value,
) -> Result<(), automerge::AutomergeError> {
    match value {
        hydrate::Value::Scalar(scalar) => target.put(object, key, scalar.clone()),
        hydrate::Value::Map(map) => {
            let child = target.put_object(object, key, ObjType::Map)?;
            fill_map(target, &child, map)
        }
        hydrate::Value::List(list) => {
            let child = target.put_object(object, key, ObjType::List)?;
            fill_list(target, &child, list)
        }
        hydrate::Value::Text(text) => {
            let child = target.put_object(object, key, ObjType::Text)?;
            target.splice_text(&child, 0, 0, &text.to_string())
        }
    }
}

/// Insert `value` at `index` of the list `object`.
fn insert(
    target: &mut AutoCommit,
    object: &ObjId,
    index: usize,
    value: &hydrate::Value,
) -> Result<(), automerge::AutomergeError> {
    match value {
        hydrate::Value::Scalar(scalar) => target.insert(object, index, scalar.clone()),
        hydrate::Value::Map(map) => {
            let child = target.insert_object(object, index, ObjType::Map)?;
            fill_map(target, &child, map)
        }
        hydrate::Value::List(list) => {
            let child = target.insert_object(object, index, ObjType::List)?;
            fill_list(target, &child, list)
        }
        hydrate::Value::Text(text) => {
            let child = target.insert_object(object, index, ObjType::Text)?;
            target.splice_text(&child, 0, 0, &text.to_string())
        }
    }
}

fn fill_map(
    target: &mut AutoCommit,
    object: &ObjId,
    map: &hydrate::Map,
) -> Result<(), automerge::AutomergeError> {
    for (key, value) in map.iter() {
        put(target, object, key.as_str(), &value.value)?;
    }
    Ok(())
}

fn fill_list(
    target: &mut AutoCommit,
    object: &ObjId,
    list: &hydrate::List,
) -> Result<(), automerge::AutomergeError> {
    for (index, item) in list.iter().enumerate() {
        insert(target, object, index, &item.value)?;
    }
    Ok(())
}

fn read_root(path: &Path) -> anyhow::Result<SigningKey> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read the workspace root {}", path.display()))?;
    let seed: [u8; 32] = hex::decode(text.trim())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .context("the workspace root file holds 64 hex characters")?;
    Ok(SigningKey::from_bytes(&seed))
}

fn read_host_key(path: &Path) -> anyhow::Result<SigningKey> {
    let bytes =
        std::fs::read(path).with_context(|| format!("read the host key {}", path.display()))?;
    let seed: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("{} is not a host key", path.display()))?;
    Ok(SigningKey::from_bytes(&seed))
}

/// The certificate `tilo-relay room --persistent <name>` serves with, signed
/// afresh from the workspace root, the host's key and the workspace's name,
/// exactly as the host signs it at every start: the host's workspaces name it
/// among their hosts, so the repair acts as the host itself and changes
/// nothing in their governance.
fn host_cert(root: &SigningKey, host: &SigningKey, name: &str) -> anyhow::Result<DeviceCert> {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: host.verifying_key().to_bytes(),
        device_label: format!("{name} workspace host"),
        capabilities: tilo_user_identity::cloud_enrollment::host_capabilities(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).context("sign the host's certificate")?;
    Ok(cert)
}

// The workspaces the tests make keep their documents in workers this test
// executable serves, as `main` does.
#[cfg(test)]
tilo_automerge_isolate::test_worker!();
#[cfg(test)]
tilo_agent_participant::app_worker!();

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::significant_drop_tightening,
        reason = "in a test these are the assertion"
    )]

    use tilo_room_host::workspaces::fixtures::{TestChannelKeys, governor, test_host_holding};
    use tilo_room_host::workspaces::sealed_fixtures::{SealingMember, member_device};

    use super::*;

    fn part() -> String {
        chat::parts::part(0).as_str().to_owned()
    }

    fn texts(doc: &AutoCommit) -> BTreeSet<String> {
        chat::messages::read_part(doc)
            .messages
            .values()
            .map(|message| message.text.clone())
            .collect()
    }

    /// A member's page posts `text` in `channel`, writing under `writer` as
    /// the page that reused the writer a saved row carried did.
    async fn posts(
        page: &SealingMember,
        workspaces: &Workspaces,
        workspace: &str,
        (chat, channel): (&chat::Chat, &str),
        (author, writer): (&str, &automerge::ActorId),
        text: &str,
    ) {
        page.writes(workspaces, workspace, |doc| {
            doc.set_actor(writer.clone());
            chat::demo::say_in_part(doc, chat, author, channel, text).unwrap();
        })
        .await;
    }

    /// Ada's two pages post in the Lounge's chat under the one writer: her
    /// first page starts the messages' first part and posts, her second takes
    /// that in, then each posts twice more from there.
    async fn two_pages_write_one_writer(
        workspaces: &Workspaces,
        workspace: &str,
        (key, cert): (&SigningKey, &DeviceCert),
    ) {
        let catalogue = tilo_agent_participant::AppCatalogue::native();
        workspaces
            .prepare_application(workspace, cert, &catalogue, CHAT, "")
            .await
            .unwrap();
        let primary = workspaces
            .record_document(workspace, CHAT, "")
            .await
            .unwrap()
            .unwrap();
        let chat =
            <chat::Chat as tilo_resource::Resource>::read(&AutoCommit::load(&primary).unwrap())
                .unwrap();
        let channel = chat
            .channels
            .iter()
            .find(|(_, channel)| channel.is_channel())
            .map(|(id, _)| id.to_string())
            .expect("the chat has a channel");
        let ada = hex::encode([9; 32]);

        // Ada's first page starts the part and posts; her second page takes
        // that in. Both write under the one writer.
        let (first_key, _) = member_device(workspaces, workspace, (key, cert), 9).await;
        let (second_key, _) = member_device(workspaces, workspace, (key, cert), 10).await;
        let joined = SealingMember::join(workspaces, workspace, (CHAT, ""), first_key).await;
        let first = joined.part(workspaces, workspace, &part()).await;
        let writer = automerge::ActorId::random();
        posts(
            &first,
            workspaces,
            workspace,
            (&chat, &channel),
            (&ada, &writer),
            "hello",
        )
        .await;
        let joined = SealingMember::join(workspaces, workspace, (CHAT, ""), second_key).await;
        let second = joined.part(workspaces, workspace, &part()).await;
        second.catch_up(workspaces, workspace).await;
        for text in ["first page one", "first page two"] {
            posts(
                &first,
                workspaces,
                workspace,
                (&chat, &channel),
                (&ada, &writer),
                text,
            )
            .await;
        }
        for text in ["second page one", "second page two"] {
            posts(
                &second,
                workspaces,
                workspace,
                (&chat, &channel),
                (&ada, &writer),
                text,
            )
            .await;
        }
    }

    /// Two pages of one person write the chat's messages under the one writer
    /// a saved row carried, each from the same messages: both number their
    /// next changes the same, and the relay keeps both writes in the record's
    /// sealed history. The record then no longer reads whole. The repair
    /// keeps one branch, writes what the other posted again as the host, and
    /// every message of both is in the record, which reads whole and is
    /// served whole to a member who joins after.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_record_that_holds_a_sequence_twice_is_repaired_with_every_message() {
        let scratch = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let _ = tilo_agent_participant::AppCatalogue::native();
        // The device's channel keys outlive the host, as its channel service
        // keeps them across a restart.
        let keys = std::sync::Arc::new(TestChannelKeys::default());
        let relay =
            test_host_holding(scratch.path(), &key, &cert, std::sync::Arc::clone(&keys)).await;
        let workspaces = relay.workspaces().clone();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;
        Box::pin(two_pages_write_one_writer(
            &workspaces,
            &workspace,
            (&key, &cert),
        ))
        .await;
        let route = BTreeSet::from([(CHAT.to_owned(), part())]);
        assert!(
            !workspaces
                .whole_records(&workspace, Some(&route))
                .await
                .unwrap()
                .is_whole(),
            "the record holds a sequence twice and does not read whole"
        );

        // A dry run changes nothing.
        let reports = repair_workspace(
            &workspaces,
            &workspace,
            Some(CHAT),
            true,
            &scratch.path().join(ARCHIVE),
        )
        .await
        .unwrap();
        let [report] = reports.as_slice() else {
            panic!("one record holds a sequence twice: {reports:?}");
        };
        assert_eq!(report.record, part());
        assert_eq!(
            report.collisions.len(),
            2,
            "both later posts of each page: {report:?}"
        );
        assert_eq!(
            report.carried.len(),
            2,
            "the second page's posts: {report:?}"
        );
        assert_eq!(
            (report.before, report.union, report.after),
            (Some(3), Some(5), Some(5))
        );
        assert!(!report.repaired);
        assert!(
            !workspaces
                .whole_records(&workspace, Some(&route))
                .await
                .unwrap()
                .is_whole()
        );

        let mut reports = repair_workspace(
            &workspaces,
            &workspace,
            Some(CHAT),
            false,
            &scratch.path().join(ARCHIVE),
        )
        .await
        .unwrap();
        assert!(
            matches!(reports.as_slice(), [report] if report.repaired && report.failed.is_none()),
            "{reports:?}"
        );
        assert!(
            matches!(&reports[0].archived, Some((kept, _)) if *kept >= 2),
            "the left-out branch is kept beside the state: {:?}",
            reports[0].archived
        );

        // The host started again on the repaired state reads it whole.
        drop(workspaces);
        relay.shutdown().await;
        let relay = test_host_holding(scratch.path(), &key, &cert, keys).await;
        let workspaces = relay.workspaces().clone();
        verify(&workspaces, &mut reports).await;
        let [report] = reports.as_slice() else {
            panic!("one record repaired: {reports:?}");
        };
        assert!(report.succeeded(), "{report:?}");
        assert_eq!(report.reads_whole, Some(true));
        assert_eq!(report.after, Some(5));
        let all: BTreeSet<String> = [
            "hello",
            "first page one",
            "first page two",
            "second page one",
            "second page two",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let read = workspaces
            .record_document(&workspace, CHAT, &part())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(texts(&AutoCommit::load(&read).unwrap()), all);

        // A member who joins now is served the record whole.
        let (later_key, _) = member_device(&workspaces, &workspace, (&key, &cert), 11).await;
        let later = SealingMember::join(&workspaces, &workspace, (CHAT, ""), later_key).await;
        let later = later.part(&workspaces, &workspace, &part()).await;
        later.catch_up(&workspaces, &workspace).await;
        assert_eq!(texts(&later.document()), all);

        // Run again, it finds nothing to repair.
        assert!(
            repair_workspace(
                &workspaces,
                &workspace,
                Some(CHAT),
                false,
                &scratch.path().join(ARCHIVE)
            )
            .await
            .unwrap()
            .is_empty()
        );
    }

    /// A page's write never reaches the host, and the page writes on top of
    /// it: the record's history holds a write that waits for a change nobody
    /// sends, and the record does not read whole. The repair keeps what reads,
    /// keeps the waiting write beside the state, and the record reads whole.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_record_waiting_on_a_change_it_never_got_reads_whole_once_repaired() {
        let scratch = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let _ = tilo_agent_participant::AppCatalogue::native();
        let keys = std::sync::Arc::new(TestChannelKeys::default());
        let relay =
            test_host_holding(scratch.path(), &key, &cert, std::sync::Arc::clone(&keys)).await;
        let workspaces = relay.workspaces().clone();
        let workspace = workspaces.create("Lounge").await.unwrap().workspace;
        let catalogue = tilo_agent_participant::AppCatalogue::native();
        workspaces
            .prepare_application(&workspace, &cert, &catalogue, CHAT, "")
            .await
            .unwrap();
        let primary = workspaces
            .record_document(&workspace, CHAT, "")
            .await
            .unwrap()
            .unwrap();
        let chat =
            <chat::Chat as tilo_resource::Resource>::read(&AutoCommit::load(&primary).unwrap())
                .unwrap();
        let channel = chat
            .channels
            .iter()
            .find(|(_, channel)| channel.is_channel())
            .map(|(id, _)| id.to_string())
            .unwrap();
        let ada = hex::encode([9; 32]);
        let (page_key, _) = member_device(&workspaces, &workspace, (&key, &cert), 9).await;
        let joined = SealingMember::join(&workspaces, &workspace, (CHAT, ""), page_key).await;
        let page = joined.part(&workspaces, &workspace, &part()).await;
        let writer = automerge::ActorId::random();
        let say = |text: &'static str| {
            let (chat, channel, ada, writer) = (&chat, &channel, &ada, &writer);
            move |doc: &mut AutoCommit| {
                doc.set_actor(writer.clone());
                chat::demo::say_in_part(doc, chat, ada, channel, text).unwrap();
            }
        };
        page.writes(&workspaces, &workspace, say("hello")).await;
        // Written, never sent.
        let ((), _never_sent) = page.seal(&workspace, say("lost on the way"));
        page.writes(&workspaces, &workspace, say("after it")).await;
        let route = BTreeSet::from([(CHAT.to_owned(), part())]);
        assert!(
            !workspaces
                .whole_records(&workspace, Some(&route))
                .await
                .unwrap()
                .is_whole()
        );

        let archive = scratch.path().join(ARCHIVE);
        let mut reports = repair_workspace(&workspaces, &workspace, Some(CHAT), false, &archive)
            .await
            .unwrap();
        let [report] = reports.as_slice() else {
            panic!("one record waits: {reports:?}");
        };
        assert!(
            report.collisions.is_empty() && report.waiting > 0,
            "{report:?}"
        );
        assert!(report.repaired && report.failed.is_none(), "{report:?}");
        let (kept, at) = report.archived.clone().unwrap();
        assert_eq!(std::fs::read_dir(&at).unwrap().count(), kept);
        for entry in std::fs::read_dir(&at).unwrap() {
            let bytes = std::fs::read(entry.unwrap().path()).unwrap();
            automerge::Change::from_bytes(bytes).expect("a kept change reads as written");
        }

        drop(workspaces);
        relay.shutdown().await;
        let relay = test_host_holding(scratch.path(), &key, &cert, keys).await;
        let workspaces = relay.workspaces().clone();
        verify(&workspaces, &mut reports).await;
        assert_eq!(reports[0].reads_whole, Some(true), "{:?}", reports[0]);
        let read = workspaces
            .record_document(&workspace, CHAT, &part())
            .await
            .unwrap()
            .unwrap();
        assert!(texts(&AutoCommit::load(&read).unwrap()).contains("hello"));
    }
}
