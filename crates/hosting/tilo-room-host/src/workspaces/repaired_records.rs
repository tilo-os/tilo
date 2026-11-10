//! The changes a record's repair left out, taken in again once the record
//! holds what they build on.
//!
//! A record whose history held writes it could not merge (built on changes
//! it never got, or writing a sequence another write already holds) was
//! repaired by compacting it into what reads. Every change the repair left
//! out is kept as it was written, under [`REPAIRED_RECORDS`] beside the host's
//! workspaces: `<workspace>/<app>/<record>/<hash>.change`, the primary
//! record's directory named [`PRIMARY`]. Those changes were folded out of the
//! record's history, so nothing else here holds them any longer.
//!
//! When the host starts, and whenever a member's write to such a record lands,
//! every kept change of it whose dependencies the record now holds is written
//! into the record as the host's own change, through the host's normal
//! authoring path ([`Workspaces::author_room_record`]): members admit it as
//! they admit any write of the workspace's host, and a kept change that waits
//! on another kept change follows it in the same write. A change the record
//! holds by then is done with too. Each change taken in, or held already,
//! leaves the archive; what still waits stays, and an empty record directory
//! goes.
//!
//! A kept change whose writer and sequence the record holds under another
//! change (the branch a repair carried forward rather than kept) can never
//! merge, and stays as kept.

use super::{RecordWrite, WeakWorkspaces, Workspaces};
use automerge::{ActorId, AutoCommit, Change, ChangeHash};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use tilo_automerge::ApplyIncoming as _;

/// The directory beside a host's workspaces, under the relay's state root,
/// where a record's repair keeps every change it left out.
pub const REPAIRED_RECORDS: &str = "repaired-records";

/// The directory a record's kept changes are under when the record is its
/// app's primary (`""` on the wire).
pub const PRIMARY: &str = "(primary)";

/// The file extension of a kept change.
const CHANGE: &str = "change";

/// Where a record's repair keeps the changes it left out, under the relay's
/// state root `root`: `<root>/repaired-records/<workspace>/<app>/<record>`.
#[must_use]
pub fn repaired_record_dir(root: &Path, workspace: &str, app: &str, record: &str) -> PathBuf {
    root.join(REPAIRED_RECORDS)
        .join(workspace)
        .join(app)
        .join(if record.is_empty() { PRIMARY } else { record })
}

/// One kept change, read back.
struct Kept {
    path: PathBuf,
    change: Change,
}

/// What taking a record's kept changes in did.
#[derive(Debug, Default, PartialEq, Eq)]
struct TakenIn {
    /// Kept changes written into the record now.
    merged: usize,
    /// Kept changes the record held already.
    held: usize,
    /// Kept changes that still wait, and stay.
    waiting: usize,
}

impl Workspaces {
    /// Where the changes this host's records' repairs left out are kept: beside
    /// its workspaces, under the relay's state root.
    fn repaired_records_root(&self) -> Option<PathBuf> {
        self.inner.dir.parent().map(Path::to_path_buf)
    }

    /// Take in every kept change of every repaired record now, then again for a
    /// record each time a member's write to it lands, for as long as the
    /// workspaces are governed here.
    pub(super) fn follow_repaired_records(&self) {
        let Some(root) = self.repaired_records_root() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        // Subscribed before the first pass: a write that lands during it is
        // followed after.
        let mut writes = self.record_writes();
        let workspaces = self.downgrade();
        runtime.spawn(async move {
            for (workspace, app, record) in kept_records(&root) {
                let Some(live) = workspaces.upgrade() else {
                    return;
                };
                live.take_in_kept(&root, &workspace, &app, &record).await;
            }
            follow_writes(&workspaces, &root, &mut writes).await;
        });
    }

    /// Write every kept change of `record` of `app` in `workspace` whose
    /// dependencies the record holds into it as the host's own change, and
    /// take it out of the archive under `root`, with any the record holds
    /// already. Logs what it could not do; the kept changes stay then.
    async fn take_in_kept(&self, root: &Path, workspace: &str, app: &str, record: &str) {
        let dir = repaired_record_dir(root, workspace, app, record);
        match self.take_in_repaired(&dir, workspace, app, record).await {
            Ok(taken) if taken.merged > 0 || taken.held > 0 => tracing::info!(
                %workspace,
                %app,
                %record,
                merged = taken.merged,
                held = taken.held,
                waiting = taken.waiting,
                "changes a record's repair left out are in the record again"
            ),
            Ok(_) => {}
            Err(error) => tracing::warn!(
                %workspace,
                %app,
                %record,
                %error,
                "changes a record's repair left out were not taken in; they stay kept"
            ),
        }
    }

    /// [`Self::take_in_kept`] for the changes kept in `dir`.
    ///
    /// # Errors
    ///
    /// A kept change does not read, the record does not read, or the host's
    /// write of it was refused.
    async fn take_in_repaired(
        &self,
        dir: &Path,
        workspace: &str,
        app: &str,
        record: &str,
    ) -> Result<TakenIn, String> {
        let kept = read_kept(dir)?;
        if kept.is_empty() {
            return Ok(TakenIn::default());
        }
        let governor = self.serving_certificate().clone();
        let changes: Vec<Change> = kept.iter().map(|kept| kept.change.clone()).collect();
        let decided = self
            .author_room_record(workspace, &governor, (app, record), move |document| {
                merge_ready(document, changes)
            })
            .await
            .map_err(|error| error.to_string())?;
        let mut taken = TakenIn::default();
        for kept in &kept {
            let hash = kept.change.hash();
            if decided.merged.contains(&hash) {
                taken.merged = taken.merged.saturating_add(1);
            } else if decided.held.contains(&hash) {
                taken.held = taken.held.saturating_add(1);
            } else {
                taken.waiting = taken.waiting.saturating_add(1);
                continue;
            }
            std::fs::remove_file(&kept.path)
                .map_err(|error| format!("{}: {error}", kept.path.display()))?;
        }
        if taken.waiting == 0 {
            // Nothing of the record is kept any more: its directory goes, and
            // the ones above it that it left empty.
            let mut empty = Some(dir);
            while let Some(at) = empty
                && std::fs::remove_dir(at).is_ok()
            {
                empty = at
                    .parent()
                    .filter(|parent| !parent.ends_with(REPAIRED_RECORDS));
            }
        }
        Ok(taken)
    }
}

/// Take in, for each member write that lands, its record's kept changes, once
/// per record however many writes of it came at once.
async fn follow_writes(
    workspaces: &WeakWorkspaces,
    root: &Path,
    writes: &mut tokio::sync::broadcast::Receiver<RecordWrite>,
) {
    use tokio::sync::broadcast::error::{RecvError, TryRecvError};
    loop {
        let mut landed = BTreeSet::new();
        match writes.recv().await {
            Ok(write) => {
                landed.insert((write.workspace, write.app, write.record));
            }
            // Writes went by unread: every record kept is tried.
            Err(RecvError::Lagged(_)) => landed.extend(kept_records(root)),
            Err(RecvError::Closed) => return,
        }
        loop {
            match writes.try_recv() {
                Ok(write) => {
                    landed.insert((write.workspace, write.app, write.record));
                }
                Err(TryRecvError::Lagged(_)) => landed.extend(kept_records(root)),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Closed) => return,
            }
        }
        for (workspace, app, record) in landed {
            if !repaired_record_dir(root, &workspace, &app, &record).is_dir() {
                continue;
            }
            let Some(live) = workspaces.upgrade() else {
                return;
            };
            live.take_in_kept(root, &workspace, &app, &record).await;
        }
    }
}

/// Every record with changes kept under `root`'s archive, as
/// `(workspace, app, record)`.
fn kept_records(root: &Path) -> Vec<(String, String, String)> {
    let mut found = Vec::new();
    for workspace in directories(&root.join(REPAIRED_RECORDS)) {
        for app in directories(&workspace) {
            let mut records = Vec::new();
            holding_changes(&app, &mut records);
            for dir in records {
                let Ok(relative) = dir.strip_prefix(&app) else {
                    continue;
                };
                let record = relative.to_string_lossy().into_owned();
                let record = if record == PRIMARY {
                    String::new()
                } else {
                    record
                };
                found.push((name(&workspace), name(&app), record));
            }
        }
    }
    found
}

/// The directories under `dir`, `dir` included, that hold a kept change: a
/// record's name may hold a `/`, which nests its directory.
fn holding_changes(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut holds = false;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            holding_changes(&path, found);
        } else if path
            .extension()
            .is_some_and(|extension| extension == CHANGE)
        {
            holds = true;
        }
    }
    if holds {
        found.push(dir.to_path_buf());
    }
}

fn directories(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_dir())
                .collect()
        })
        .unwrap_or_default()
}

fn name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The changes kept in `dir`, each as it was written.
fn read_kept(dir: &Path) -> Result<Vec<Kept>, String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(Vec::new());
    };
    let mut kept = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().is_none_or(|extension| extension != CHANGE) {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let change =
            Change::from_bytes(bytes).map_err(|error| format!("{}: {error}", path.display()))?;
        kept.push(Kept { path, change });
    }
    Ok(kept)
}

/// Which kept changes [`merge_ready`] merged, and which the record held.
#[derive(Default)]
struct Decided {
    merged: BTreeSet<ChangeHash>,
    held: BTreeSet<ChangeHash>,
}

/// Merge into `document` every change of `changes` whose dependencies it
/// holds, round by round, so one that builds on another kept change follows
/// it. A change the document holds is named held; one whose writer and
/// sequence the document holds under another change, or that waits for one
/// it lacks, is left out.
#[allow(
    clippy::needless_pass_by_value,
    reason = "the authoring closure owns the kept changes it is handed"
)]
fn merge_ready(document: &mut AutoCommit, changes: Vec<Change>) -> Result<Decided, String> {
    let mut decided = Decided::default();
    let mut written: HashMap<ActorId, u64> = HashMap::new();
    for change in document.get_changes(&[]) {
        let seq = written.entry(change.actor_id().clone()).or_default();
        *seq = (*seq).max(change.seq());
    }
    let mut waiting: BTreeMap<ChangeHash, Change> = BTreeMap::new();
    for change in changes {
        if document.get_change_by_hash(&change.hash()).is_some() {
            decided.held.insert(change.hash());
        } else {
            waiting.insert(change.hash(), change);
        }
    }
    loop {
        let ready: Vec<ChangeHash> = waiting
            .iter()
            .filter(|(_, change)| {
                let next = written
                    .get(change.actor_id())
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(1);
                change.seq() == next
                    && change
                        .deps()
                        .iter()
                        .all(|dep| document.get_change_by_hash(dep).is_some())
            })
            .map(|(hash, _)| *hash)
            .collect();
        if ready.is_empty() {
            return Ok(decided);
        }
        for hash in ready {
            let Some(change) = waiting.remove(&hash) else {
                continue;
            };
            written.insert(change.actor_id().clone(), change.seq());
            document
                .apply_incoming([change])
                .map_err(|error| error.to_string())?;
            decided.merged.insert(hash);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test these are the assertion"
    )]

    use super::*;
    use automerge::transaction::Transactable as _;
    use automerge::{ROOT, ReadDoc as _};

    fn edit(document: &mut AutoCommit, key: &str) -> Change {
        document.put(ROOT, key, key).unwrap();
        document.commit();
        document.get_last_local_change().unwrap()
    }

    #[test]
    fn a_kept_change_merges_once_what_it_builds_on_is_in_the_record() {
        let mut writer = AutoCommit::new();
        let base = edit(&mut writer, "base");
        let missing = edit(&mut writer, "missing");
        let waiting = edit(&mut writer, "waiting");
        let after = edit(&mut writer, "after");
        let mut record = AutoCommit::new();
        record.apply_incoming([base]).unwrap();

        let decided = merge_ready(&mut record, vec![after.clone(), waiting.clone()]).unwrap();
        assert!(decided.merged.is_empty() && decided.held.is_empty());
        assert!(record.get(ROOT, "waiting").unwrap().is_none());

        record.apply_incoming([missing]).unwrap();
        let decided = merge_ready(&mut record, vec![after.clone(), waiting.clone()]).unwrap();
        assert_eq!(
            decided.merged,
            BTreeSet::from([waiting.hash(), after.hash()]),
            "the kept change that builds on another kept one follows it"
        );
        assert!(record.get(ROOT, "after").unwrap().is_some());
        let again = merge_ready(&mut record, vec![after.clone()]).unwrap();
        assert_eq!(again.held, BTreeSet::from([after.hash()]));
    }

    #[test]
    fn a_kept_change_of_a_sequence_the_record_holds_otherwise_stays() {
        let mut writer = AutoCommit::new();
        let base = edit(&mut writer, "base");
        let mut fork = writer.fork();
        fork.set_actor(writer.get_actor().clone());
        let kept = edit(&mut writer, "kept");
        let other = edit(&mut fork, "other");
        assert_eq!(
            (kept.actor_id(), kept.seq()),
            (other.actor_id(), other.seq())
        );
        let mut record = AutoCommit::new();
        record.apply_incoming([base, kept]).unwrap();
        let decided = merge_ready(&mut record, vec![other]).unwrap();
        assert!(decided.merged.is_empty() && decided.held.is_empty());
        assert!(record.get(ROOT, "other").unwrap().is_none());
    }
}
