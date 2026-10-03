//! Durable state of the native data plane, in `meta.redb`.
//!
//! A restart must continue where the last run stopped without re-reading
//! the room: the last capture epoch (its retained snapshot, the journal
//! position it covers and the captured state of every path) and, for every
//! path the daemon wrote itself, which record version it materialised and
//! what the local inode looked like afterwards. Each epoch is persisted in
//! one transaction after its changeset has been applied, so a crash
//! replays at most one epoch, and replaying is idempotent.

use std::collections::HashMap;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs_native::capture::{CaptureState, NodeKind, NodeState};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

const EPOCH: TableDefinition<&str, &[u8]> = TableDefinition::new("native_epoch");
const NODES: TableDefinition<&[u8], &[u8]> = TableDefinition::new("native_nodes");
const MATERIALIZED: TableDefinition<&[u8], &[u8]> = TableDefinition::new("native_materialized");
const LEASES: TableDefinition<&[u8], &[u8]> = TableDefinition::new("native_leases");

const CAPTURE_KEY: &str = "capture";

#[derive(Serialize, Deserialize)]
struct EpochRecord {
    next_epoch: u64,
    snapshot: Vec<u8>,
    journal_through: u64,
}

#[derive(Serialize, Deserialize)]
struct StoredNode {
    /// 0 file, 1 directory, 2 symlink.
    kind: u8,
    link: Vec<u8>,
    mode: u32,
    size: u64,
    mtime_ns: i128,
    ctime_ns: i128,
    ino: u64,
}

impl StoredNode {
    fn from_state(n: &NodeState) -> Self {
        let (kind, link) = match &n.kind {
            NodeKind::File => (0, Vec::new()),
            NodeKind::Dir => (1, Vec::new()),
            NodeKind::Symlink(target) => (2, target.as_os_str().as_bytes().to_vec()),
        };
        Self {
            kind,
            link,
            mode: n.mode,
            size: n.size,
            mtime_ns: n.mtime_ns,
            ctime_ns: n.ctime_ns,
            ino: n.ino,
        }
    }

    fn into_state(self) -> NodeState {
        NodeState {
            kind: match self.kind {
                1 => NodeKind::Dir,
                2 => NodeKind::Symlink(PathBuf::from(std::ffi::OsString::from_vec(self.link))),
                _ => NodeKind::File,
            },
            mode: self.mode,
            size: self.size,
            mtime_ns: self.mtime_ns,
            ctime_ns: self.ctime_ns,
            ino: self.ino,
        }
    }
}

/// What the local inode looked like right after the daemon wrote it. A path
/// whose inode no longer matches has been changed locally since, and a
/// remote version never overwrites it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalStat {
    pub ino: u64,
    pub size: u64,
    pub mtime_ns: i128,
}

impl LocalStat {
    pub fn of(path: &Path) -> Option<Self> {
        use std::os::unix::fs::MetadataExt as _;
        let meta = std::fs::symlink_metadata(path).ok()?;
        Some(Self {
            ino: meta.ino(),
            size: meta.len(),
            mtime_ns: i128::from(meta.mtime())
                .saturating_mul(1_000_000_000)
                .saturating_add(i128::from(meta.mtime_nsec())),
        })
    }
}

/// The record version a path of the live tree corresponds to.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent facts about one materialised path, persisted as they are"
)]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Materialized {
    pub dir: bool,
    pub hash: Option<[u8; 32]>,
    pub size: u64,
    /// The version's time, as the record plane stamped it.
    pub mtime_ms: u64,
    /// Created as a placeholder (it may since have become resident).
    pub placeholder: bool,
    pub local: Option<LocalStat>,
    /// The record this path is, so a rename moves the local file instead
    /// of writing it again.
    pub record: Option<[u8; 16]>,
    /// Replicated permission bits the path had when the daemon last wrote
    /// or published it; a local `chmod` since is a change of its own.
    pub mode: Option<u32>,
    /// Materialised as a symbolic link.
    pub symlink: bool,
    /// The tenancy the version was written in, for a lease-class file.
    pub fence: Option<crate::namespace::VersionFence>,
    /// This device published the version.
    pub own: bool,
}

/// The row format before records, modes and fences were kept.
#[derive(Deserialize)]
struct MaterializedV1 {
    dir: bool,
    hash: Option<[u8; 32]>,
    size: u64,
    mtime_ms: u64,
    placeholder: bool,
    local: Option<LocalStat>,
}

fn decode_materialized(raw: &[u8]) -> Result<Materialized> {
    if let Ok(m) = postcard::from_bytes::<Materialized>(raw) {
        return Ok(m);
    }
    let old: MaterializedV1 = postcard::from_bytes(raw)?;
    Ok(Materialized {
        dir: old.dir,
        hash: old.hash,
        size: old.size,
        mtime_ms: old.mtime_ms,
        placeholder: old.placeholder,
        local: old.local,
        ..Materialized::default()
    })
}

/// A write lease this device knows about, local or remote.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredLease {
    pub holder: [u8; 32],
    pub epoch: u64,
    pub expires_ms: u64,
}

pub struct NativeState {
    db: Database,
}

fn key(path: &Path) -> &[u8] {
    path.as_os_str().as_bytes()
}

fn path_of(key: &[u8]) -> PathBuf {
    PathBuf::from(std::ffi::OsString::from_vec(key.to_vec()))
}

/// Changes to the captured node table, persisted with an epoch.
pub enum NodeUpdate {
    /// Only these paths changed.
    Delta(Vec<(PathBuf, Option<NodeState>)>),
    /// The table is replaced (after a full comparison).
    Replace(Vec<(PathBuf, NodeState)>),
}

impl NativeState {
    pub fn open(path: &Path) -> Result<Self> {
        let db = Database::create(path)
            .with_context(|| format!("opening native data plane state {}", path.display()))?;
        let txn = db.begin_write()?;
        {
            txn.open_table(EPOCH)?;
            txn.open_table(NODES)?;
            txn.open_table(MATERIALIZED)?;
            txn.open_table(LEASES)?;
        }
        txn.commit()?;
        Ok(Self { db })
    }

    /// The persisted capture epoch, and the journal position it covers.
    pub fn capture_state(&self) -> Result<Option<(CaptureState, u64)>> {
        let txn = self.db.begin_read()?;
        let epochs = txn.open_table(EPOCH)?;
        let Some(raw) = epochs.get(CAPTURE_KEY)? else {
            return Ok(None);
        };
        let record: EpochRecord = postcard::from_bytes(raw.value())?;
        let nodes = txn.open_table(NODES)?;
        let mut out = Vec::new();
        for row in nodes.iter()? {
            let (k, v) = row?;
            let node: StoredNode = postcard::from_bytes(v.value())?;
            out.push((path_of(k.value()), node.into_state()));
        }
        Ok(Some((
            CaptureState {
                next_epoch: record.next_epoch,
                snapshot: path_of(&record.snapshot),
                nodes: out,
            },
            record.journal_through,
        )))
    }

    /// Persists an applied epoch in one transaction.
    pub fn save_epoch(
        &self,
        next_epoch: u64,
        snapshot: &Path,
        journal_through: u64,
        nodes: NodeUpdate,
        materialized: &[(PathBuf, Option<Materialized>)],
    ) -> Result<()> {
        let txn = self.db.begin_write()?;
        {
            let mut epochs = txn.open_table(EPOCH)?;
            let record = EpochRecord {
                next_epoch,
                snapshot: key(snapshot).to_vec(),
                journal_through,
            };
            epochs.insert(CAPTURE_KEY, postcard::to_allocvec(&record)?.as_slice())?;
        }
        if let NodeUpdate::Replace(_) = &nodes {
            txn.delete_table(NODES)?;
        }
        {
            let mut table = txn.open_table(NODES)?;
            match nodes {
                NodeUpdate::Delta(changes) => {
                    for (path, node) in changes {
                        match node {
                            Some(n) => {
                                let v = postcard::to_allocvec(&StoredNode::from_state(&n))?;
                                table.insert(key(&path), v.as_slice())?;
                            }
                            None => {
                                table.remove(key(&path))?;
                            }
                        }
                    }
                }
                NodeUpdate::Replace(all) => {
                    for (path, n) in all {
                        let v = postcard::to_allocvec(&StoredNode::from_state(&n))?;
                        table.insert(key(&path), v.as_slice())?;
                    }
                }
            }
        }
        write_materialized(&txn, materialized)?;
        txn.commit()?;
        Ok(())
    }

    /// Forgets the capture epoch (its snapshot is gone): the next run
    /// starts with a full comparison.
    pub fn clear_capture(&self) -> Result<()> {
        let txn = self.db.begin_write()?;
        {
            txn.open_table(EPOCH)?.remove(CAPTURE_KEY)?;
        }
        txn.delete_table(NODES)?;
        txn.open_table(NODES)?;
        txn.commit()?;
        Ok(())
    }

    pub fn materialized(&self) -> Result<HashMap<PathBuf, Materialized>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(MATERIALIZED)?;
        let mut out = HashMap::new();
        for row in table.iter()? {
            let (k, v) = row?;
            out.insert(path_of(k.value()), decode_materialized(v.value())?);
        }
        Ok(out)
    }

    pub fn set_materialized(&self, updates: &[(PathBuf, Option<Materialized>)]) -> Result<()> {
        if updates.is_empty() {
            return Ok(());
        }
        let txn = self.db.begin_write()?;
        write_materialized(&txn, updates)?;
        txn.commit()?;
        Ok(())
    }

    pub fn leases(&self) -> Result<HashMap<PathBuf, StoredLease>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(LEASES)?;
        let mut out = HashMap::new();
        for row in table.iter()? {
            let (k, v) = row?;
            out.insert(path_of(k.value()), postcard::from_bytes(v.value())?);
        }
        Ok(out)
    }

    /// Follows a rename: leases of `from` and below are kept under `to`.
    pub fn move_lease(&self, from: &Path, to: &Path) -> Result<()> {
        let moved: Vec<(PathBuf, StoredLease)> = self
            .leases()?
            .into_iter()
            .filter(|(p, _)| p.starts_with(from))
            .collect();
        if moved.is_empty() {
            return Ok(());
        }
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(LEASES)?;
            for (old, lease) in moved {
                let Some(new) = super::project::rebase(&old, from, to) else {
                    continue;
                };
                table.remove(key(&old))?;
                table.insert(key(&new), postcard::to_allocvec(&lease)?.as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    pub fn set_lease(&self, path: &Path, lease: Option<&StoredLease>) -> Result<()> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(LEASES)?;
            match lease {
                Some(l) => {
                    table.insert(key(path), postcard::to_allocvec(l)?.as_slice())?;
                }
                None => {
                    table.remove(key(path))?;
                }
            }
        }
        txn.commit()?;
        Ok(())
    }
}

fn write_materialized(
    txn: &redb::WriteTransaction,
    updates: &[(PathBuf, Option<Materialized>)],
) -> Result<()> {
    let mut table = txn.open_table(MATERIALIZED)?;
    for (path, value) in updates {
        match value {
            Some(m) => {
                table.insert(key(path), postcard::to_allocvec(m)?.as_slice())?;
            }
            None => {
                table.remove(key(path))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(ino: u64) -> NodeState {
        NodeState {
            kind: NodeKind::File,
            mode: 0o644,
            size: 3,
            mtime_ns: -5,
            ctime_ns: 1 << 70,
            ino,
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn native_state_rows_survive_reopen_and_decode_older_layouts() {
        // Phase `epochs_nodes_and_materialized_paths_survive_a_reopen`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("meta.redb");
            {
                let state = NativeState::open(&path).unwrap();
                assert!(state.capture_state().unwrap().is_none());
                state
                    .save_epoch(
                        2,
                        Path::new("/store/.epochs/r/0000000000000001"),
                        41,
                        NodeUpdate::Replace(vec![
                            ("a".into(), node(1)),
                            (
                                "l".into(),
                                NodeState {
                                    kind: NodeKind::Symlink("t".into()),
                                    ..node(2)
                                },
                            ),
                        ]),
                        &[(
                            "a".into(),
                            Some(Materialized {
                                dir: false,
                                hash: Some([9; 32]),
                                size: 3,
                                mtime_ms: 7,
                                placeholder: true,
                                local: Some(LocalStat {
                                    ino: 1,
                                    size: 3,
                                    mtime_ns: 8,
                                }),
                                record: Some([4; 16]),
                                mode: Some(0o644),
                                symlink: false,
                                fence: None,
                                own: true,
                            }),
                        )],
                    )
                    .unwrap();
                state
                    .save_epoch(
                        3,
                        Path::new("/store/.epochs/r/0000000000000002"),
                        50,
                        NodeUpdate::Delta(vec![("b".into(), Some(node(3))), ("l".into(), None)]),
                        &[],
                    )
                    .unwrap();
            }
            let state = NativeState::open(&path).unwrap();
            let (capture, through) = state.capture_state().unwrap().unwrap();
            assert_eq!(through, 50);
            assert_eq!(capture.next_epoch, 3);
            assert_eq!(
                capture.snapshot,
                Path::new("/store/.epochs/r/0000000000000002")
            );
            let mut nodes = capture.nodes;
            nodes.sort_by(|a, b| a.0.cmp(&b.0));
            assert_eq!(nodes, vec![("a".into(), node(1)), ("b".into(), node(3))]);
            assert_eq!(
                state.materialized().unwrap()[Path::new("a")].hash,
                Some([9; 32])
            );

            state.clear_capture().unwrap();
            assert!(state.capture_state().unwrap().is_none());
            assert_eq!(state.materialized().unwrap().len(), 1);
            assert_eq!(
                state.materialized().unwrap()[Path::new("a")].record,
                Some([4; 16])
            );
        }
        // Phase `rows_written_before_records_were_kept_still_decode`.
        {
            #[derive(Serialize)]
            struct Old {
                dir: bool,
                hash: Option<[u8; 32]>,
                size: u64,
                mtime_ms: u64,
                placeholder: bool,
                local: Option<LocalStat>,
            }
            let raw = postcard::to_allocvec(&Old {
                dir: false,
                hash: Some([1; 32]),
                size: 5,
                mtime_ms: 6,
                placeholder: false,
                local: None,
            })
            .unwrap();
            let m = decode_materialized(&raw).unwrap();
            assert_eq!(
                (m.hash, m.size, m.mtime_ms, m.record),
                (Some([1; 32]), 5, 6, None)
            );
        }
    }
}
