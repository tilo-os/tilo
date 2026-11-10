//! Move a workspace host's content into one store per workspace, once, at the
//! upgrade that gives each workspace its own.
//!
//! A host used to keep content in three places:
//!
//! - `<state>/relay/blobs/<hash>`: one flat directory for every workspace it
//!   governs (Chat attachments, bridged avatars, the files members wrote);
//! - `<state>/relay/workspace-content/<workspace>/<xx>/<hash>`: what apps and
//!   uploads kept in a workspace's records;
//! - `<state>/agents/content/<workspace>/<xx>/<hash>`: what its hosted agents
//!   kept.
//!
//! It now keeps all of it in `<state>/relay/content/<workspace>/<xx>/<hash>`
//! and reads nothing from the old places. With the host stopped, this:
//!
//! - opens the host's workspaces as the host does (from its state, its root
//!   file and its device key) on an endpoint nothing outside can reach, and
//!   reads every record of each, as the host's collection does, for the
//!   content addresses they name;
//! - hashes every old item, and moves only bytes whose BLAKE3 hash is their
//!   name; one that does not match is named and left where it is;
//! - moves an item of a workspace's directory into that workspace's store;
//! - links an item of the flat `blobs` into the store of each workspace whose
//!   records name it, and of no other, so a workspace's store never holds
//!   another's bytes; an item no workspace names goes to
//!   `<state>/relay/unassigned/`, which nothing serves, and is listed;
//! - counts each moved item as written now, so the host keeps it at least its
//!   grace period before it may let go of what nothing names;
//! - removes an old item only with `--remove-sources`, and only once it is
//!   intact everywhere it went.
//!
//! A workspace whose records do not all read whole names nothing here; its
//! blobs go to `unassigned/` and it is listed, so a later run (the items stay
//! where they were without `--remove-sources`) can place them.
//!
//! Build it for the host, and run it as the user that owns the state, after a
//! fresh backup and with the host stopped:
//!
//! ```text
//! rcargo build --release -p tilo-relay --features agents --example move_relay_content \
//!     --fetch release/examples/move_relay_content
//! move_relay_content --state /srv/lounge --root-file /etc/lounge/root --dry-run
//! move_relay_content --state /srv/lounge --root-file /etc/lounge/root
//! # once the upgraded host serves its workspaces' files and pictures:
//! move_relay_content --state /srv/lounge --root-file /etc/lounge/root --remove-sources
//! ```
//!
//! `--state` and `--root-file` are what `tilo-relay room` runs with. Running it
//! again is safe: what is in place and intact is left alone. It exits
//! non-zero when an item did not match its name or could not be moved, or a
//! workspace's records did not read whole.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context as _;
use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCert, RevocationList, sign_device_cert};

const BLOBS: &str = "blobs";
const WORKSPACE_CONTENT: &str = "workspace-content";
const UNASSIGNED: &str = "unassigned";

struct Args {
    state: PathBuf,
    root_file: PathBuf,
    dry_run: bool,
    remove_sources: bool,
}

fn args() -> anyhow::Result<Args> {
    let mut state = None;
    let mut root_file = None;
    let mut dry_run = false;
    let mut remove_sources = false;
    let mut given = std::env::args().skip(1);
    while let Some(arg) = given.next() {
        match arg.as_str() {
            "--state" => state = given.next().map(PathBuf::from),
            "--root-file" => root_file = given.next().map(PathBuf::from),
            "--dry-run" => dry_run = true,
            "--remove-sources" => remove_sources = true,
            other => anyhow::bail!(
                "unknown argument {other:?}: --state DIR --root-file FILE [--dry-run] [--remove-sources]"
            ),
        }
    }
    Ok(Args {
        state: state.context("--state: the directory `tilo-relay room --state` names")?,
        root_file: root_file
            .context("--root-file: the file `tilo-relay room --root-file` names")?,
        dry_run,
        remove_sources,
    })
}

#[derive(Default)]
struct Report {
    moved: u64,
    already: u64,
    bytes: u64,
    removed: u64,
    unassigned: Vec<(String, u64)>,
    mismatched: Vec<PathBuf>,
    failed: Vec<(PathBuf, String)>,
    unread: Vec<String>,
}

fn main() -> ExitCode {
    // The host's workspaces keep their documents in workers that re-enter
    // this executable; serve them first.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        return ExitCode::from(code);
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .try_init();
    match args().and_then(|args| run(&args)) {
        Ok(report) => {
            print(&report);
            if report.mismatched.is_empty() && report.failed.is_empty() && report.unread.is_empty()
            {
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

fn run(args: &Args) -> anyhow::Result<Report> {
    let relay_root = tilo_relay_protocol::metrics::room_relay_root(&args.state);
    // Nothing else runs on this state while it moves.
    let _lock = tilo_relay::state_lock::StateLock::acquire(&relay_root)
        .map_err(anyhow::Error::msg)
        .context("the host must be stopped")?;
    let named = tilo_rt::app()
        .block_on(Box::pin(named_by_workspace(args, &relay_root)))
        .map_err(|error| anyhow::anyhow!("{error:?}"))??;
    let mut report = Report::default();
    for (workspace, names) in &named {
        if names.is_none() {
            report.unread.push(workspace.clone());
        }
    }
    let content = relay_root.join(tilo_relay::content::DIRECTORY);
    for old in [
        relay_root.join(WORKSPACE_CONTENT),
        args.state.join("agents").join("content"),
    ] {
        for (workspace, hash, source) in sharded(&old) {
            let target = sharded_path(&content.join(&workspace), &hash);
            one(args, &mut report, &source, &hash, &[target]);
        }
    }
    let unassigned = relay_root.join(UNASSIGNED);
    for (hash, source) in flat(&relay_root.join(BLOBS)) {
        let mut targets: Vec<PathBuf> = named
            .iter()
            .filter(|(_, names)| names.as_ref().is_some_and(|names| names.contains(&hash)))
            .map(|(workspace, _)| sharded_path(&content.join(workspace), &hash))
            .collect();
        if targets.is_empty() {
            let len = std::fs::metadata(&source).map_or(0, |meta| meta.len());
            report.unassigned.push((hash.clone(), len));
            targets.push(unassigned.join(&hash));
        }
        one(args, &mut report, &source, &hash, &targets);
    }
    Ok(report)
}

/// What the records of each workspace the host governs name, by workspace;
/// `None` for one whose records do not all read whole.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the host is open until every workspace's records are read"
)]
async fn named_by_workspace(
    args: &Args,
    relay_root: &Path,
) -> anyhow::Result<BTreeMap<String, Option<BTreeSet<String>>>> {
    let root = read_root(&args.root_file)?;
    let host = read_host_key(&args.state.join("host-device.key"))?;
    let cert = host_cert(&root, &host)?;
    let scratch = tempfile::tempdir()?;
    // Bound to this machine alone, and found by nobody: no member reaches
    // the host while its content moves.
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&host.to_bytes()))
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))?
        .bind()
        .await?;
    let relay = tilo_relay::Relay::start_on_endpoint_with_protocols(
        tilo_relay::RelayConfig {
            state_root: relay_root.to_path_buf(),
            identity_root: args.state.join("identity"),
            full_replica_status: scratch.path().join("status.json"),
            home_peer_bundle: scratch.path().join("home-peer.toml"),
            ..Default::default()
        },
        tilo_relay::daemon::RelayIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: host,
            device_cert: cert,
            revocations: RevocationList::new(),
        },
        endpoint,
        Vec::new(),
    )
    .await
    .map_err(|error| anyhow::anyhow!("open the host's workspaces: {error}"))?;
    let workspaces = relay
        .workspaces()
        .context("the host governs no workspaces")?
        .clone();
    let blobs = relay_root.join(BLOBS);
    let mut named = BTreeMap::new();
    for summary in workspaces.list().await {
        let workspace = summary.workspace;
        let own = [
            relay_root.join(WORKSPACE_CONTENT).join(&workspace),
            args.state.join("agents").join("content").join(&workspace),
        ];
        let blobs = blobs.clone();
        // A manifest a record names is read where the move finds it: the
        // workspace's own old stores, or the flat blobs.
        let read = move |hash: &str| {
            let shard = hash.get(..2)?;
            std::iter::once(blobs.join(hash))
                .chain(own.iter().map(|dir| dir.join(shard).join(hash)))
                .find_map(|path| std::fs::read(path).ok())
        };
        let found = tilo_relay::content::collection::named(&workspaces, &workspace, read).await;
        println!(
            "{workspace}: {}",
            found.as_ref().map_or_else(
                || "its records do not all read whole; it names nothing here".to_owned(),
                |found| format!("names {} content addresses", found.len()),
            )
        );
        named.insert(workspace, found);
    }
    relay.shutdown().await;
    Ok(named)
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

/// The certificate `tilo-relay room` serves with: its workspaces' records
/// name it, and the host is the device they were made with.
fn host_cert(root: &SigningKey, host: &SigningKey) -> anyhow::Result<DeviceCert> {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: host.verifying_key().to_bytes(),
        device_label: "workspace host".into(),
        capabilities: tilo_user_identity::cloud_enrollment::host_capabilities(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).context("sign the host's certificate")?;
    Ok(cert)
}

/// Where `hash` sits in a sharded store at `store`.
fn sharded_path(store: &Path, hash: &str) -> PathBuf {
    store.join(hash.get(..2).unwrap_or_default()).join(hash)
}

fn is_hash(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// `(workspace, hash, path)` for each item of per-workspace stores under
/// `dir`.
fn sharded(dir: &Path) -> Vec<(String, String, PathBuf)> {
    let mut items = Vec::new();
    for workspace in entries(dir) {
        let Some(id) = workspace
            .file_name()
            .and_then(|name| uuid::Uuid::parse_str(name.to_str()?).ok())
        else {
            continue;
        };
        for shard in entries(&workspace) {
            for item in entries(&shard) {
                let name = item.file_name().and_then(|name| name.to_str());
                if let Some(hash) = name.filter(|name| is_hash(name))
                    && item.is_file()
                {
                    items.push((
                        id.hyphenated().to_string(),
                        hash.to_ascii_lowercase(),
                        item.clone(),
                    ));
                }
            }
        }
    }
    items
}

/// `(hash, path)` for each item of the flat `dir`.
fn flat(dir: &Path) -> Vec<(String, PathBuf)> {
    entries(dir)
        .into_iter()
        .filter(|item| item.is_file())
        .filter_map(|item| {
            let hash = item.file_name()?.to_str().filter(|name| is_hash(name))?;
            Some((hash.to_ascii_lowercase(), item.clone()))
        })
        .collect()
}

fn entries(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    found.sort();
    found
}

fn hash_of(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(hasher.finalize().to_hex().to_string());
        }
        hasher.update(buffer.get(..read).unwrap_or_default());
    }
}

fn intact(path: &Path, hash: &str) -> bool {
    path.is_file() && hash_of(path).is_ok_and(|found| found == hash)
}

/// Move `source`, named `hash`, to each of `targets`.
fn one(args: &Args, report: &mut Report, source: &Path, hash: &str, targets: &[PathBuf]) {
    if !intact(source, hash) {
        report.mismatched.push(source.to_path_buf());
        return;
    }
    let mut placed = true;
    for target in targets {
        match place(source, target, hash, args.dry_run) {
            Ok(true) => report.already = report.already.saturating_add(1),
            Ok(false) => {
                report.moved = report.moved.saturating_add(1);
                report.bytes = report
                    .bytes
                    .saturating_add(std::fs::metadata(source).map_or(0, |meta| meta.len()));
            }
            Err(error) => {
                report
                    .failed
                    .push((source.to_path_buf(), error.to_string()));
                placed = false;
            }
        }
    }
    if placed && args.remove_sources && !args.dry_run {
        match std::fs::remove_file(source) {
            Ok(()) => report.removed = report.removed.saturating_add(1),
            Err(error) => report
                .failed
                .push((source.to_path_buf(), error.to_string())),
        }
    }
}

/// Put `source` at `target`: `true` when it was there already.
fn place(source: &Path, target: &Path, hash: &str, dry_run: bool) -> std::io::Result<bool> {
    let already = intact(target, hash);
    if dry_run {
        return Ok(already);
    }
    if !already {
        let dir = target
            .parent()
            .ok_or_else(|| std::io::Error::other("no directory"))?;
        std::fs::create_dir_all(dir)?;
        let incoming = dir.join(format!(".incoming-move-{}-{hash}", std::process::id()));
        let _ = std::fs::remove_file(&incoming);
        let linked = std::fs::hard_link(source, &incoming).or_else(|_| {
            std::fs::copy(source, &incoming)?;
            File::open(&incoming)?.sync_all()
        });
        if let Err(error) = linked.and_then(|()| std::fs::rename(&incoming, target)) {
            let _ = std::fs::remove_file(&incoming);
            return Err(error);
        }
        File::open(dir)?.sync_all()?;
        if !intact(target, hash) {
            return Err(std::io::Error::other(format!(
                "{} does not read back intact",
                target.display()
            )));
        }
    }
    // Written now, as far as the host's grace period goes.
    File::options()
        .append(true)
        .open(target)?
        .set_modified(std::time::SystemTime::now())?;
    Ok(already)
}

fn print(report: &Report) {
    println!(
        "moved {} items ({} bytes), {} already in place, {} old items removed",
        report.moved, report.bytes, report.already, report.removed
    );
    if !report.unassigned.is_empty() {
        println!(
            "{} items no workspace names, in {UNASSIGNED}/ (nothing serves them):",
            report.unassigned.len()
        );
        for (hash, len) in &report.unassigned {
            println!("  {hash}  {len} bytes");
        }
    }
    for workspace in &report.unread {
        eprintln!("not every record of {workspace} read whole: run again once they do");
    }
    for source in &report.mismatched {
        eprintln!(
            "left in place, its bytes do not hash to its name: {}",
            source.display()
        );
    }
    for (source, reason) in &report.failed {
        eprintln!("not moved: {}: {reason}", source.display());
    }
}

// The workspaces the tests make keep their documents in workers this test
// executable serves, as `main` does.
#[cfg(test)]
tilo_automerge_isolate::test_worker!();

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use automerge::transaction::Transactable as _;

    use super::*;

    fn kept(dir: &Path, bytes: &[u8]) -> String {
        let hash = blake3::hash(bytes).to_hex().to_string();
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(&hash), bytes).unwrap();
        hash
    }

    fn sharded_in(store: &Path, hash: &str) -> PathBuf {
        super::sharded_path(store, hash)
    }

    /// Each blob of the flat store goes to the workspaces whose records name
    /// it and to no other; one nothing names goes where nothing serves it.
    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the host is open until its workspaces are made"
    )]
    fn a_blob_goes_only_to_the_workspaces_that_name_it() {
        let state = tempfile::tempdir().unwrap();
        let root = SigningKey::from_bytes(&[71; 32]);
        let host = SigningKey::from_bytes(&[73; 32]);
        let root_file = state.path().join("root");
        std::fs::write(&root_file, hex::encode(root.to_bytes())).unwrap();
        std::fs::write(state.path().join("host-device.key"), host.to_bytes()).unwrap();
        let relay_root = tilo_relay_protocol::metrics::room_relay_root(state.path());
        let blobs = relay_root.join(BLOBS);
        let picture = kept(&blobs, b"a picture the lounge's notes show");
        let stray = kept(&blobs, b"an upload no record ever named");
        let (lounge, other) = tilo_rt::app()
            .block_on(Box::pin(async {
                let relay = tilo_relay::Relay::start(
                    tilo_relay::RelayConfig {
                        state_root: relay_root.clone(),
                        identity_root: state.path().join("identity"),
                        full_replica_status: state.path().join("status.json"),
                        home_peer_bundle: state.path().join("home-peer.toml"),
                        ..Default::default()
                    },
                    tilo_relay::daemon::RelayIdentity {
                        user_public_key: root.verifying_key().to_bytes(),
                        device_signing_key: host.clone(),
                        device_cert: host_cert(&root, &host).unwrap(),
                        revocations: RevocationList::new(),
                    },
                )
                .await
                .unwrap();
                let workspaces = relay.workspaces().unwrap().clone();
                let lounge = workspaces.create("Lounge").await.unwrap().workspace;
                let other = workspaces.create("Elsewhere").await.unwrap().workspace;
                let governor = workspaces.serving_certificate().clone();
                workspaces
                    .author_room_record(&lounge, &governor, ("so.tilo.notes", ""), |doc| {
                        doc.put(automerge::ROOT, "picture", picture.as_str())
                            .map_err(|error| error.to_string())
                    })
                    .await
                    .unwrap();
                relay.shutdown().await;
                (lounge, other)
            }))
            .unwrap();
        // An item of a workspace's own store sits in its shard.
        let upload_bytes = b"what an app of the other workspace kept";
        let upload_hash = blake3::hash(upload_bytes).to_hex().to_string();
        let own = relay_root.join(WORKSPACE_CONTENT).join(&other);
        kept(&own.join(upload_hash.get(..2).unwrap()), upload_bytes);

        let args = Args {
            state: state.path().to_path_buf(),
            root_file,
            dry_run: false,
            remove_sources: false,
        };
        let report = run(&args).unwrap();
        assert!(
            report.unread.is_empty(),
            "every record reads: {:?}",
            report.unread
        );
        assert!(report.mismatched.is_empty() && report.failed.is_empty());
        let content = relay_root.join(tilo_relay::content::DIRECTORY);
        assert!(intact(
            &sharded_in(&content.join(&lounge), &picture),
            &picture
        ));
        assert!(
            !sharded_in(&content.join(&other), &picture).exists(),
            "a workspace whose records do not name a blob is not given it"
        );
        assert!(intact(
            &sharded_in(&content.join(&other), &upload_hash),
            &upload_hash
        ));
        assert!(intact(&relay_root.join(UNASSIGNED).join(&stray), &stray));
        assert!(!sharded_in(&content.join(&lounge), &stray).exists());
        assert!(!sharded_in(&content.join(&other), &stray).exists());
        assert_eq!(report.unassigned.len(), 1);
        assert!(blobs.join(&picture).exists(), "nothing is removed unasked");

        let report = run(&Args {
            remove_sources: true,
            ..args
        })
        .unwrap();
        assert_eq!(report.moved, 0, "a second run finds everything in place");
        assert!(!blobs.join(&picture).exists() && !blobs.join(&stray).exists());
        assert!(intact(
            &sharded_in(&content.join(&lounge), &picture),
            &picture
        ));
        assert!(intact(&relay_root.join(UNASSIGNED).join(&stray), &stray));
    }
}
