//! Sealing what the rooms a relay stewards kept in the clear, checked.
//!
//! A new record of a room whose channels the relay stewards is sealed from
//! its first write. Records written before that keep their history in the
//! clear until the relay's operator decides to seal them: [`apply`] keeps a
//! verbatim copy of the state beside it, records the decision in the state
//! ([`DECISION`]), and seals them. From then on the relay seals them at
//! every start, and its compactor never keeps a channel it stewards in the
//! clear. Without the decision the relay leaves them as they are.
//! [`scratch`] does the same to a copy of a state, leaving the state as it
//! is. Both say, per record, whether it is sealed whole: nothing is left in
//! the clear, and every change the history held in the clear opens from
//! what is retained now, with its author and sequence.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::daemon::{Relay, RelayIdentity};

/// One record channel after `reseal`.
#[derive(Clone, Debug)]
pub struct SealedRecord {
    pub workspace_id: u64,
    pub app: String,
    pub record: String,
    /// Entries holding a change that were retained in the clear before.
    pub cleartext_before: usize,
    /// Changes the history in the clear held.
    pub changes_before: usize,
    /// Entries holding a change still retained in the clear.
    pub cleartext_after: usize,
    /// Changes of the history in the clear that no longer open.
    pub lost: usize,
}

impl SealedRecord {
    /// Nothing is left in the clear and nothing was lost.
    #[must_use]
    pub const fn whole(&self) -> bool {
        self.cleartext_after == 0 && self.lost == 0
    }
}

/// What the stores under `state_root` retain in the clear, per workspace
/// and channel.
fn in_the_clear(
    state_root: &Path,
) -> Result<BTreeMap<(u64, [u8; 32]), crate::compaction::Retained>, String> {
    let payloads = crate::payloads::neighbour();
    let mut out = BTreeMap::new();
    let workspaces = match std::fs::read_dir(state_root.join("workspaces")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(error) => return Err(error.to_string()),
    };
    for entry in workspaces {
        let entry = entry.map_err(|error| error.to_string())?;
        let Some(workspace_id) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u64>().ok())
        else {
            continue;
        };
        let store = crate::store::WorkspaceStore::open(state_root, workspace_id, true)
            .map_err(|error| error.to_string())?;
        for channel in store
            .retained_channels()
            .map_err(|error| error.to_string())?
        {
            let entries = store
                .channel_entries(&channel)
                .map_err(|error| error.to_string())?;
            out.insert(
                (workspace_id, channel),
                crate::compaction::retained(payloads.clone(), &entries, None),
            );
        }
    }
    Ok(out)
}

/// The file in a relay's state recording that its operator decided to seal
/// what the rooms it stewards hold in the clear.
pub const DECISION: &str = "reseal.toml";

/// The decision [`DECISION`] records.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Decision {
    /// When it was decided, in seconds since the Unix epoch.
    decided_at_unix: u64,
    /// The verbatim copy of the state kept before anything was sealed; none
    /// for a scratch copy.
    backup: Option<PathBuf>,
}

/// Whether the operator of the relay whose state is `state_root` decided to
/// seal what its rooms hold in the clear.
#[must_use]
pub fn decided(state_root: &Path) -> bool {
    state_root.join(DECISION).is_file()
}

fn recorded(state_root: &Path) -> Result<Option<Decision>, String> {
    let path = state_root.join(DECISION);
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text)
            .map(Some)
            .map_err(|error| format!("{}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// Record `decision` in `state_root`, durably.
fn record(state_root: &Path, decision: &Decision) -> Result<(), String> {
    let text = toml::to_string(decision).map_err(|error| error.to_string())?;
    let path = state_root.join(DECISION);
    let staged = state_root.join(format!("{DECISION}.new"));
    let io = |error: std::io::Error| format!("{}: {error}", path.display());
    let mut file = std::fs::File::create(&staged).map_err(io)?;
    std::io::Write::write_all(&mut file, text.as_bytes()).map_err(io)?;
    file.sync_all().map_err(io)?;
    std::fs::rename(&staged, &path).map_err(io)?;
    std::fs::File::open(state_root)
        .and_then(|dir| dir.sync_all())
        .map_err(io)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Copy the directory `from` to `to`, which must not exist yet, every file
/// and directory synced.
fn copy_state(from: &Path, to: &Path) -> Result<(), String> {
    let io = |path: &Path| {
        let path = path.to_owned();
        move |error: std::io::Error| format!("{}: {error}", path.display())
    };
    std::fs::create_dir(to).map_err(io(to))?;
    for entry in std::fs::read_dir(from).map_err(io(from))? {
        let entry = entry.map_err(io(from))?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type().map_err(io(&source))?;
        if kind.is_dir() {
            copy_state(&source, &target)?;
        } else if kind.is_file() {
            std::fs::copy(&source, &target).map_err(io(&source))?;
            std::fs::File::open(&target)
                .and_then(|file| file.sync_all())
                .map_err(io(&target))?;
        } else {
            return Err(format!(
                "{}: neither a file nor a directory",
                source.display()
            ));
        }
    }
    std::fs::File::open(to)
        .and_then(|dir| dir.sync_all())
        .map_err(io(to))
}

/// What [`apply`] did.
#[derive(Debug)]
pub struct Applied {
    /// The verbatim copy of the state kept before anything was sealed.
    pub backup: PathBuf,
    pub records: Vec<SealedRecord>,
}

/// Decide to seal what the rooms of the relay whose state is `state_root`
/// hold in the clear, and seal it. First the state is copied verbatim
/// beside it (`<state_root>.before-reseal-<seconds>`), kept until someone
/// removes it, then the decision is recorded in the state, naming that
/// copy, and the relay seals its records as it does at every start from
/// then on. The relay must be stopped: this holds the state's lock
/// throughout. Run again after it stopped, it keeps the copy it recorded
/// and finishes.
///
/// # Errors
///
/// A relay runs on the state, the copy or the decision cannot be written,
/// or the relay does not start on the state.
pub async fn apply(state_root: &Path, identity: RelayIdentity) -> Result<Applied, String> {
    let _lock = crate::state_lock::StateLock::acquire(state_root)?;
    let recorded = recorded(state_root)?.and_then(|decision| decision.backup);
    let backup = if let Some(backup) = recorded {
        backup
    } else {
        let at = unix_now();
        let mut name = state_root
            .file_name()
            .ok_or_else(|| format!("{} names no directory", state_root.display()))?
            .to_owned();
        name.push(format!(".before-reseal-{at}"));
        let backup = state_root.with_file_name(name);
        copy_state(state_root, &backup)?;
        record(
            state_root,
            &Decision {
                decided_at_unix: at,
                backup: Some(backup.clone()),
            },
        )?;
        backup
    };
    let records = reseal(state_root, identity).await?;
    Ok(Applied { backup, records })
}

/// Copy the state of the relay at `state_root` to `copy`, a new directory,
/// and seal the copy as [`apply`] seals a state, leaving the state as it is.
///
/// # Errors
///
/// The copy cannot be made, or the relay does not start on it.
pub async fn scratch(
    state_root: &Path,
    copy: &Path,
    identity: RelayIdentity,
) -> Result<Vec<SealedRecord>, String> {
    copy_state(state_root, copy)?;
    record(
        copy,
        &Decision {
            decided_at_unix: unix_now(),
            backup: None,
        },
    )?;
    reseal(copy, identity).await
}

/// Start `identity`'s relay on the state under `state_root`, which records
/// the decision, let it seal what the rooms it stewards hold in the clear,
/// stop it, and report each record of those rooms.
#[expect(
    clippy::significant_drop_tightening,
    reason = "Relay::shutdown consumes the relay after the report is read"
)]
async fn reseal(state_root: &Path, identity: RelayIdentity) -> Result<Vec<SealedRecord>, String> {
    if !decided(state_root) {
        return Err(format!("{}: no re-seal is decided", state_root.display()));
    }
    let before = in_the_clear(state_root)?;
    let relay = Relay::start_with_relay_mode(
        crate::RelayConfig {
            state_root: state_root.to_owned(),
            // Nothing outside this host reaches the relay while it seals.
            iroh_listen: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            ..Default::default()
        },
        identity,
        iroh::RelayMode::Disabled,
    )
    .await
    .map_err(|error| error.to_string())?;
    relay.resealed().await;
    let after = match relay.rooms() {
        Some(rooms) => rooms.stewarded_channels().await,
        None => Vec::new(),
    };
    relay.shutdown().await;
    Ok(after
        .into_iter()
        .map(|(workspace_id, channel, (app, record), after)| {
            let before = before
                .get(&(workspace_id, channel))
                .cloned()
                .unwrap_or_default();
            SealedRecord {
                workspace_id,
                app,
                record,
                cleartext_before: before.cleartext,
                changes_before: before.changes.len(),
                cleartext_after: after.cleartext,
                lost: before.changes.difference(&after.changes).count(),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every file under `root` and its bytes, by path within it, but the
    /// state's lock and the stores' staging files (named with a leading
    /// dot), which no reader takes and which a store reclaims in the
    /// background once it opens.
    fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(root, &path, out);
                } else if path.file_name().is_some_and(|name| {
                    name != crate::state_lock::LOCK_FILE && !name.to_string_lossy().starts_with('.')
                }) {
                    let bytes = std::fs::read(&path).unwrap();
                    out.insert(path.strip_prefix(root).unwrap().to_owned(), bytes);
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    /// The paths whose bytes differ between `before` and `after`.
    fn changed(
        before: &BTreeMap<PathBuf, Vec<u8>>,
        after: &BTreeMap<PathBuf, Vec<u8>>,
    ) -> Vec<PathBuf> {
        before
            .keys()
            .chain(after.keys())
            .filter(|path| before.get(*path) != after.get(*path))
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// A Lounge as the relay kept it before a stewarded room's records were
    /// sealed from their first write (`tests/fixtures/legacy-lounge`, made
    /// by that relay), in `dir/state`, and its relay's identity: the host and
    /// two members wrote its records in the clear, members later wrote two
    /// of them sealed, one never got a channel.
    fn legacy_lounge(dir: &Path) -> (PathBuf, RelayIdentity) {
        let state = dir.join("state");
        std::fs::create_dir(&state).unwrap();
        let mut untar = std::process::Command::new("sh")
            .args(["-c", "base64 -d | tar xzf - -C \"$0\""])
            .arg(&state)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(
            &mut untar.stdin.take().unwrap(),
            include_bytes!("../tests/fixtures/legacy-lounge/state.tar.gz.b64"),
        )
        .unwrap();
        assert!(untar.wait().unwrap().success());
        let (key, cert) = crate::rooms::fixtures::governor_with(
            tilo_user_identity::cloud_enrollment::host_capabilities(),
        );
        let identity = RelayIdentity {
            user_public_key: cert.user_public_key,
            device_signing_key: key,
            device_cert: cert,
            revocations: tilo_user_identity::RevocationList::new(),
        };
        (state, identity)
    }

    /// Until its operator decides to seal it, a relay leaves what the
    /// Lounge kept in the clear as it is: starting on it changes none of its
    /// records' stores, and nothing seals it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[expect(
        clippy::significant_drop_tightening,
        reason = "Relay::shutdown consumes the relay after it started"
    )]
    async fn a_lounge_kept_in_the_clear_stays_as_it_is_until_its_operator_decides() {
        let dir = tempfile::tempdir().unwrap();
        let (state, identity) = legacy_lounge(dir.path());
        let before = files(&state);
        let relay = Relay::start_with_relay_mode(
            crate::RelayConfig {
                state_root: state.clone(),
                iroh_listen: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
                ..Default::default()
            },
            identity.clone(),
            iroh::RelayMode::Disabled,
        )
        .await
        .unwrap();
        relay.resealed().await;
        relay.shutdown().await;
        // The relay keeps its rooms' governance as it starts them; the
        // records' writes are in the workspaces' stores.
        let stores = |files: BTreeMap<PathBuf, Vec<u8>>| -> BTreeMap<PathBuf, Vec<u8>> {
            files
                .into_iter()
                .filter(|(path, _)| path.starts_with("workspaces"))
                .collect()
        };
        assert_eq!(
            changed(&stores(before.clone()), &stores(files(&state))),
            Vec::<PathBuf>::new()
        );
        assert!(reseal(&state, identity).await.is_err());
        assert_eq!(
            changed(&stores(before), &stores(files(&state))),
            Vec::<PathBuf>::new()
        );
    }

    /// A scratch copy of the Lounge is sealed whole, and the Lounge's own
    /// state is left as it is.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_scratch_copy_of_the_lounge_is_sealed_whole_and_the_lounge_left_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let (state, identity) = legacy_lounge(dir.path());
        let before = files(&state);
        let sealed = scratch(&state, &dir.path().join("copy"), identity)
            .await
            .unwrap();
        assert_eq!(sealed.len(), 3, "{sealed:?}");
        assert!(sealed.iter().all(SealedRecord::whole), "{sealed:?}");
        assert_eq!(changed(&before, &files(&state)), Vec::<PathBuf>::new());
        assert!(!decided(&state));
    }

    /// Once its operator decides it, the relay keeps a verbatim copy of the
    /// Lounge beside its state and seals every record of it whole; deciding
    /// again keeps that copy and changes nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_lounge_kept_in_the_clear_is_sealed_whole_once_its_operator_decides() {
        let dir = tempfile::tempdir().unwrap();
        let (state, identity) = legacy_lounge(dir.path());
        let before = files(&state);
        let records = |sealed: &[SealedRecord]| -> Vec<(u64, String, String)> {
            sealed
                .iter()
                .map(|record| {
                    (
                        record.workspace_id,
                        record.app.clone(),
                        record.record.clone(),
                    )
                })
                .collect()
        };

        let first = apply(&state, identity.clone()).await.unwrap();
        assert_eq!(first.backup.parent(), Some(dir.path()));
        assert_eq!(
            changed(&before, &files(&first.backup)),
            Vec::<PathBuf>::new()
        );
        assert!(decided(&state));
        assert_eq!(first.records.len(), 3, "{first:?}");
        assert!(first.records.iter().all(SealedRecord::whole), "{first:?}");
        assert!(
            first
                .records
                .iter()
                .all(|record| record.cleartext_before > 0 && record.changes_before > 0),
            "{first:?}"
        );

        let sealed = files(&state);
        let second = apply(&state, identity).await.unwrap();
        assert_eq!(second.backup, first.backup);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
        assert_eq!(records(&second.records), records(&first.records));
        assert!(
            second
                .records
                .iter()
                .all(|record| record.whole() && record.cleartext_before == 0),
            "{second:?}"
        );
        // What the records' channels retain; the relay writes elsewhere at
        // every start.
        let channels: Vec<String> = first
            .records
            .iter()
            .map(|record| {
                hex::encode(neighbour_wire::app_document_channel(
                    &record.app,
                    &record.record,
                ))
            })
            .collect();
        let entries = |files: &BTreeMap<PathBuf, Vec<u8>>| -> BTreeMap<PathBuf, Vec<u8>> {
            files
                .iter()
                .filter(|(path, _)| {
                    let path = path.to_string_lossy();
                    channels
                        .iter()
                        .any(|channel| path.contains(channel.as_str()))
                })
                .map(|(path, bytes)| (path.clone(), bytes.clone()))
                .collect()
        };
        assert!(!entries(&sealed).is_empty());
        assert_eq!(entries(&files(&state)), entries(&sealed));
    }
}
