//! Durable history of encrypted document ops, served page by page.
//!
//! The broker's in-memory op log keeps only the most recent ops of each
//! artifact. Content-key ciphertext (encryption scheme version 2) is
//! readable by every member of the channel, including members added later,
//! so it is worth keeping all of it: this store appends every such op to a
//! per-artifact log on disk and answers scrollback pages from it, however
//! far back a member scrolls. The broker never holds a content key; the
//! store only ever sees signed ciphertext and its clear routing metadata.
//!
//! Layout: `<root>/<artifact uuid>.ops`, a sequence of records
//! `len (u32 LE) || postcard(SignedOp)`. The index (seq → offset) is rebuilt
//! by scanning on first use; a torn final record from a crash is cut off.
//! Beside each log, `<artifact uuid>.room` names the room whose members may
//! page through it, and `<artifact uuid>.record` the record it is: room, app
//! and record id, one per line, so a later session still knows which
//! records of an app this device holds in a room.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::PathBuf;

use collab_types::{ArtifactId, SignedOp};

/// Upper bound on one stored record; a document frame plus envelope is far
/// below it, so anything larger is corruption.
const MAX_RECORD_BYTES: u32 = 8 * 1024 * 1024;

/// History written to its files but not yet synced to disk. The broker
/// writes history under the lock its channels share, which the compositor's
/// main loop takes too, so the data sync, which can wait on a busy disk for
/// a long time, is left to the caller once it has released that lock. A
/// caller syncs before it hands what it recorded on (fans an op out to apps
/// or peers, answers a page request), so it still delivers an op only once
/// its history is on disk.
#[must_use = "recorded history is durable only once synced"]
#[derive(Debug, Default)]
pub struct Unsynced(Vec<File>);

impl Unsynced {
    /// Leave `file`'s data sync to [`Self::sync`]. A file that cannot be
    /// handed on (no descriptor left to duplicate it) is synced now.
    fn defer(&mut self, file: &File) -> std::io::Result<()> {
        match file.try_clone() {
            Ok(handle) => {
                self.0.push(handle);
                Ok(())
            }
            Err(_) => file.sync_data(),
        }
    }

    pub(crate) fn join(mut self, other: Self) -> Self {
        self.0.extend(other.0);
        self
    }

    /// Sync every file written, so what was recorded survives a crash.
    ///
    /// # Errors
    ///
    /// Returns the first sync's I/O error; the files after it are still
    /// synced.
    pub fn sync(self) -> std::io::Result<()> {
        let mut first = Ok(());
        for file in self.0 {
            if let Err(error) = file.sync_data()
                && first.is_ok()
            {
                first = Err(error);
            }
        }
        first
    }

    /// [`Self::sync`], logging a failure: the op it recorded is still
    /// delivered live, as one whose append failed is.
    pub fn finish(self) {
        if let Err(error) = self.sync() {
            tracing::warn!(%error, "collab history: sync failed");
        }
    }
}

struct ArtifactLog {
    file: File,
    /// `(seq, offset, len)`, sorted by `seq`.
    index: Vec<(u64, u64, u32)>,
    /// `(author, seq)` of every stored op; unique per the validator's
    /// replay rule, so it identifies an op across peers.
    known: std::collections::HashSet<([u8; 32], u64)>,
    end: u64,
}

impl ArtifactLog {
    fn open(path: &std::path::Path) -> std::io::Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        let mut bytes = Vec::new();
        file.seek(SeekFrom::Start(0))?;
        file.read_to_end(&mut bytes)?;
        let mut index = Vec::new();
        let mut known = std::collections::HashSet::new();
        let mut offset: usize = 0;
        // Why the tail is cut, for the warning below.
        let mut torn = "a record runs past the end of the file".to_owned();
        while let Some(header) = bytes.get(offset..offset.saturating_add(4)) {
            let len = u32::from_le_bytes(<[u8; 4]>::try_from(header).unwrap_or([0xff; 4]));
            let start = offset.saturating_add(4);
            let end = start.saturating_add(len as usize);
            let Some(record) = bytes.get(start..end).filter(|_| len <= MAX_RECORD_BYTES) else {
                break;
            };
            let op = match postcard::from_bytes::<SignedOp>(record) {
                Ok(op) => op,
                Err(error) => {
                    torn = format!("the record at byte {offset} does not decode: {error}");
                    break;
                }
            };
            index.push((op.seq, start as u64, len));
            known.insert((op.author.0, op.seq));
            offset = end;
        }
        if offset != bytes.len() {
            tracing::warn!(
                path = %path.display(),
                kept = offset,
                found = bytes.len(),
                reason = %torn,
                "collab history: cutting off a torn or corrupt tail"
            );
            file.set_len(offset as u64)?;
        }
        index.sort_by_key(|(seq, _, _)| *seq);
        Ok(Self {
            file,
            index,
            known,
            end: offset as u64,
        })
    }

    fn append(&mut self, op: &SignedOp) -> std::io::Result<Unsynced> {
        self.append_all(std::slice::from_ref(&op))
            .map(|(_, unsynced)| unsynced)
    }

    /// Append every op not already stored with one write, leaving its one
    /// data sync to the caller; returns how many were new.
    fn append_all(&mut self, ops: &[&SignedOp]) -> std::io::Result<(usize, Unsynced)> {
        let mut records = Vec::new();
        let mut staged: Vec<(u64, [u8; 32], u64, u32)> = Vec::new();
        let mut batch = std::collections::HashSet::new();
        for op in ops {
            let key = (op.author.0, op.seq);
            if self.known.contains(&key) || !batch.insert(key) {
                continue;
            }
            let encoded = postcard::to_stdvec(op).map_err(std::io::Error::other)?;
            let len = u32::try_from(encoded.len())
                .ok()
                .filter(|len| *len <= MAX_RECORD_BYTES)
                .ok_or_else(|| std::io::Error::other("history record too large"))?;
            records.extend_from_slice(&len.to_le_bytes());
            let offset = self.end.saturating_add(records.len() as u64);
            records.extend_from_slice(&encoded);
            staged.push((op.seq, op.author.0, offset, len));
        }
        if staged.is_empty() {
            return Ok((0, Unsynced::default()));
        }
        self.file.write_all(&records)?;
        let mut unsynced = Unsynced::default();
        unsynced.defer(&self.file)?;
        self.end = self.end.saturating_add(records.len() as u64);
        for (seq, author, offset, len) in &staged {
            let position = self.index.partition_point(|(stored, _, _)| stored <= seq);
            self.index.insert(position, (*seq, *offset, *len));
            self.known.insert((*author, *seq));
        }
        Ok((staged.len(), unsynced))
    }

    fn read(&mut self, offset: u64, len: u32) -> std::io::Result<SignedOp> {
        let mut record = vec![0; len as usize];
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(&mut record)?;
        postcard::from_bytes(&record).map_err(std::io::Error::other)
    }

    /// The newest `max` ops with `seq < before_seq`, ascending, plus whether
    /// nothing older remains.
    fn page_before(&mut self, before_seq: u64, max: u16) -> std::io::Result<(Vec<SignedOp>, bool)> {
        let below = self.index.partition_point(|(seq, _, _)| *seq < before_seq);
        let start = below.saturating_sub(usize::from(max));
        let entries: Vec<(u64, u32)> = self
            .index
            .get(start..below)
            .unwrap_or_default()
            .iter()
            .map(|(_, offset, len)| (*offset, *len))
            .collect();
        let mut ops = Vec::with_capacity(entries.len());
        for (offset, len) in entries {
            ops.push(self.read(offset, len)?);
        }
        Ok((ops, start == 0))
    }
}

/// Per-artifact durable logs of content-key ciphertext.
pub struct CollabHistoryStore {
    root: PathBuf,
    logs: HashMap<ArtifactId, ArtifactLog>,
    /// The `.record` files of the store, read once on first use and kept
    /// with every binding after.
    records: Option<HashMap<ArtifactId, HeldRecord>>,
}

/// The record an artifact's history is, as its `.record` file says.
#[derive(Clone, Debug, PartialEq, Eq)]
struct HeldRecord {
    room: String,
    app_id: String,
    record_id: String,
}

impl HeldRecord {
    fn encode(&self) -> String {
        format!("{}\n{}\n{}", self.room, self.app_id, self.record_id)
    }

    fn decode(text: &str) -> Option<Self> {
        let mut lines = text.split('\n');
        let (room, app_id, record_id) = (lines.next()?, lines.next()?, lines.next()?);
        let held = Self {
            room: room.to_owned(),
            app_id: app_id.to_owned(),
            record_id: record_id.to_owned(),
        };
        (lines.next().is_none() && !room.is_empty() && !app_id.is_empty()).then_some(held)
    }
}

impl CollabHistoryStore {
    /// Open (creating, mode 0700) the store at `root`.
    ///
    /// # Errors
    ///
    /// Returns the I/O error when the directory cannot be created.
    pub fn open(root: PathBuf) -> std::io::Result<Self> {
        fs::create_dir_all(&root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            root,
            logs: HashMap::new(),
            records: None,
        })
    }

    /// The session store under the launcher-provided state root.
    #[cfg(not(test))]
    pub fn open_for_session() -> std::io::Result<Self> {
        let state = tilo_paths::env_dir("XDG_STATE_HOME").ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "launcher did not provide XDG_STATE_HOME for collaboration history",
            )
        })?;
        Self::open(state.join("tilo/collaboration/history"))
    }

    fn log(&mut self, artifact: ArtifactId) -> std::io::Result<&mut ArtifactLog> {
        if !self.logs.contains_key(&artifact) {
            let log = ArtifactLog::open(&self.root.join(format!("{}.ops", artifact.0)))?;
            self.logs.insert(artifact, log);
        }
        self.logs
            .get_mut(&artifact)
            .ok_or_else(|| std::io::Error::other("history log vanished"))
    }

    /// Whether `op` belongs in durable history: content-key ciphertext only.
    /// Cleartext routes keep their existing in-memory window, and a
    /// single-use MLS application message could never be read back.
    pub fn retains(op: &SignedOp) -> bool {
        op.encryption
            .is_some_and(|metadata| metadata.retention().is_some())
    }

    /// Append a validated op, if it belongs in durable history, leaving its
    /// sync to the caller.
    ///
    /// # Errors
    ///
    /// Returns the I/O error; the op is still fanned out live by the caller.
    pub fn record(&mut self, op: &SignedOp) -> std::io::Result<Unsynced> {
        if !Self::retains(op) {
            return Ok(Unsynced::default());
        }
        self.log(op.artifact)?.append(op)
    }

    /// Append the ops of `artifact` that are not already stored, with one
    /// data sync for the whole batch, left to the caller; returns how many
    /// were new. Callers have already filtered the ops to retained
    /// ciphertext of `artifact`.
    ///
    /// # Errors
    ///
    /// Returns the I/O error of the append; then none of the batch is new.
    pub fn record_all_if_new(
        &mut self,
        artifact: ArtifactId,
        ops: &[&SignedOp],
    ) -> std::io::Result<(usize, Unsynced)> {
        if ops.iter().any(|op| op.artifact != artifact) {
            return Err(std::io::Error::other("history batch spans artifacts"));
        }
        self.log(artifact)?.append_all(ops)
    }

    /// Remember that `artifact`'s history belongs to the room `cluster_id`,
    /// beside its log, so a later session still knows which room's members
    /// may page through it. The binding's sync is left to the caller.
    ///
    /// # Errors
    ///
    /// Returns the I/O error of writing the binding.
    pub fn bind_room(
        &mut self,
        artifact: ArtifactId,
        cluster_id: &str,
    ) -> std::io::Result<Unsynced> {
        let path = self.room_path(artifact);
        if fs::read_to_string(&path).is_ok_and(|bound| bound == cluster_id) {
            return Ok(Unsynced::default());
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(cluster_id.as_bytes())?;
        let mut unsynced = Unsynced::default();
        unsynced.defer(&file)?;
        Ok(unsynced)
    }

    /// The room `artifact`'s history was bound to, if any session bound it.
    pub fn room_of(&self, artifact: ArtifactId) -> Option<String> {
        fs::read_to_string(self.room_path(artifact))
            .ok()
            .filter(|room| !room.is_empty())
    }

    fn room_path(&self, artifact: ArtifactId) -> PathBuf {
        self.root.join(format!("{}.room", artifact.0))
    }

    fn record_path(&self, artifact: ArtifactId) -> PathBuf {
        self.root.join(format!("{}.record", artifact.0))
    }

    /// The records this store holds history of, read from disk on first
    /// use. A file that does not parse (torn by a crash) names no record,
    /// and the log says which.
    fn held_records(&mut self) -> &mut HashMap<ArtifactId, HeldRecord> {
        let root = &self.root;
        self.records.get_or_insert_with(|| {
            let entries = match fs::read_dir(root) {
                Ok(entries) => entries,
                Err(error) => {
                    tracing::warn!(root = %root.display(), %error, "collab history: held records cannot be listed");
                    return HashMap::new();
                }
            };
            entries
                .filter_map(Result::ok)
                .filter_map(|entry| {
                    let path = entry.path();
                    if path
                        .extension()
                        .is_none_or(|extension| extension != "record")
                    {
                        return None;
                    }
                    let unreadable = |reason: &dyn std::fmt::Display| {
                        tracing::warn!(path = %path.display(), %reason, "collab history: held record names no record");
                    };
                    let stem = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default();
                    let artifact = match uuid::Uuid::parse_str(stem) {
                        Ok(artifact) => artifact,
                        Err(error) => {
                            unreadable(&format_args!("file name is not an artifact id: {error}"));
                            return None;
                        }
                    };
                    let text = match fs::read_to_string(&path) {
                        Ok(text) => text,
                        Err(error) => {
                            unreadable(&error);
                            return None;
                        }
                    };
                    let Some(held) = HeldRecord::decode(&text) else {
                        unreadable(&"contents are not room, app and record lines");
                        return None;
                    };
                    Some((ArtifactId(artifact), held))
                })
                .collect()
        })
    }

    /// Remember that `artifact`'s history is the record `record_id` of the
    /// app `app_id` in the room `cluster_id` ([`Self::bind_room`] included),
    /// so a later session still knows the records it holds there
    /// ([`Self::records_in`]). The binding's sync is left to the caller.
    ///
    /// # Errors
    ///
    /// Returns the I/O error of writing the binding.
    pub fn bind_record(
        &mut self,
        artifact: ArtifactId,
        cluster_id: &str,
        app_id: &str,
        record_id: &str,
    ) -> std::io::Result<Unsynced> {
        let held = HeldRecord {
            room: cluster_id.to_owned(),
            app_id: app_id.to_owned(),
            record_id: record_id.to_owned(),
        };
        if self.held_records().get(&artifact) == Some(&held) {
            return Ok(Unsynced::default());
        }
        let room = self.bind_room(artifact, cluster_id)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(self.record_path(artifact))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(held.encode().as_bytes())?;
        let mut unsynced = room;
        unsynced.defer(&file)?;
        self.held_records().insert(artifact, held);
        Ok(unsynced)
    }

    /// The records of the app `app_id` in the room `cluster_id` this store
    /// holds history of, this session or an earlier one.
    pub fn records_in(&mut self, app_id: &str, cluster_id: &str) -> Vec<String> {
        self.held_records()
            .values()
            .filter(|held| held.app_id == app_id && held.room == cluster_id)
            .map(|held| held.record_id.clone())
            .collect()
    }

    /// One scrollback page from durable history.
    pub fn page_before(
        &mut self,
        artifact: ArtifactId,
        before_seq: u64,
        max: u16,
    ) -> std::io::Result<(Vec<SignedOp>, bool)> {
        self.log(artifact)?.page_before(before_seq, max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::{
        AttestationId, ChannelId, ContentRetention, Ed25519PublicKey, EncryptionMetadata, OpKind,
    };

    fn op(artifact: ArtifactId, seq: u64, encryption: Option<EncryptionMetadata>) -> SignedOp {
        let mut op = SignedOp::new_unsigned(
            seq.to_le_bytes().to_vec(),
            artifact,
            Ed25519PublicKey::from_bytes([4; 32]),
            AttestationId::new(),
            OpKind::Change,
            std::time::SystemTime::UNIX_EPOCH,
            seq,
        );
        op.encryption = encryption;
        op
    }

    #[test]
    fn history_outlives_the_memory_window_and_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let artifact = ArtifactId::new();
        let sealed = Some(EncryptionMetadata::content_key(
            1,
            ChannelId::new(),
            ContentRetention::Delta,
        ));
        {
            let mut store = CollabHistoryStore::open(dir.path().join("history")).unwrap();
            // Far more than the broker's in-memory window, stored the way a
            // peer's history page is: one batch, one data sync.
            let ops: Vec<SignedOp> = (1..=10_000).map(|seq| op(artifact, seq, sealed)).collect();
            let batch: Vec<&SignedOp> = ops.iter().collect();
            let (new, unsynced) = store.record_all_if_new(artifact, &batch).unwrap();
            assert_eq!(new, 10_000);
            unsynced.sync().unwrap();
            let (new, unsynced) = store.record_all_if_new(artifact, &batch).unwrap();
            assert_eq!(new, 0);
            unsynced.sync().unwrap();
            store
                .record(&op(artifact, 10_000, sealed))
                .unwrap()
                .sync()
                .unwrap();
            // Cleartext and single-use MLS messages are not retained.
            store
                .record(&op(artifact, 20_000, None))
                .unwrap()
                .sync()
                .unwrap();
            store
                .record(&op(
                    artifact,
                    20_001,
                    Some(EncryptionMetadata::mls_application(1, ChannelId::new())),
                ))
                .unwrap()
                .sync()
                .unwrap();
        }
        let mut store = CollabHistoryStore::open(dir.path().join("history")).unwrap();
        let (newest, exhausted) = store.page_before(artifact, u64::MAX, 100).unwrap();
        assert_eq!(newest.len(), 100);
        assert_eq!(newest.first().unwrap().seq, 9_901);
        assert_eq!(newest.last().unwrap().seq, 10_000);
        assert!(!exhausted);
        let (oldest, exhausted) = store.page_before(artifact, 51, 100).unwrap();
        assert_eq!(oldest.len(), 50);
        assert_eq!(oldest.first().unwrap().seq, 1);
        assert!(exhausted);
        assert_eq!(oldest.first().unwrap().payload, 1_u64.to_le_bytes());
    }

    #[test]
    fn a_torn_tail_is_cut_off_and_appends_continue() {
        let dir = tempfile::tempdir().unwrap();
        let artifact = ArtifactId::new();
        let sealed = Some(EncryptionMetadata::content_key(
            1,
            ChannelId::new(),
            ContentRetention::Checkpoint,
        ));
        let root = dir.path().join("history");
        {
            let mut store = CollabHistoryStore::open(root.clone()).unwrap();
            store
                .record(&op(artifact, 1, sealed))
                .unwrap()
                .sync()
                .unwrap();
            store
                .record(&op(artifact, 2, sealed))
                .unwrap()
                .sync()
                .unwrap();
        }
        let path = root.join(format!("{}.ops", artifact.0));
        let mut bytes = fs::read(&path).unwrap();
        bytes.truncate(bytes.len() - 3);
        fs::write(&path, bytes).unwrap();
        let mut store = CollabHistoryStore::open(root).unwrap();
        store
            .record(&op(artifact, 3, sealed))
            .unwrap()
            .sync()
            .unwrap();
        let (ops, exhausted) = store.page_before(artifact, u64::MAX, 10).unwrap();
        assert_eq!(ops.iter().map(|op| op.seq).collect::<Vec<_>>(), [1, 3]);
        assert!(exhausted);
    }

    /// How many ops a peer page added, synced as a caller syncs them.
    fn stored((stored, unsynced): (usize, Unsynced)) -> usize {
        unsynced.sync().unwrap();
        stored
    }

    fn signed(
        key: &ed25519_dalek::SigningKey,
        artifact: ArtifactId,
        seq: u64,
        encryption: Option<EncryptionMetadata>,
    ) -> SignedOp {
        let mut envelope = op(artifact, seq, encryption);
        envelope.author = Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
        collab_types::sign_op(&mut envelope, key).unwrap();
        envelope
    }

    /// A device that joined later pages back past its own history: its
    /// broker asks peers once, keeps only signed content-key ciphertext of
    /// the requested range, and serves it from durable history.
    #[test]
    fn peer_history_fills_durable_history_for_later_pages() {
        let dir = tempfile::tempdir().unwrap();
        let (key, _) = collab_types::generate_keypair();
        let artifact = ArtifactId::new();
        let sealed = Some(EncryptionMetadata::content_key(
            4,
            ChannelId::new(),
            ContentRetention::Delta,
        ));
        let mut channels = crate::collab_core::BrokerChannels {
            history: Some(CollabHistoryStore::open(dir.path().join("history")).unwrap()),
            ..Default::default()
        };
        for seq in 101..=130 {
            let envelope = signed(&key, artifact, seq, sealed);
            channels
                .record_history(
                    &crate::route::CrdtRoute::record("so.tilo.notes", "room-a", "page"),
                    &envelope,
                )
                .sync()
                .unwrap();
            channels.state_for(artifact).record_validated_op(envelope);
        }
        let (page, exhausted) = channels.scrollback_page(artifact, 131, 10);
        assert_eq!(page.first().unwrap().seq, 121);
        assert_eq!(page.len(), 10, "memory and durable copies are not doubled");
        assert!(!exhausted);
        let (_, exhausted) = channels.scrollback_page(artifact, 101, 10);
        assert!(exhausted, "nothing older locally");

        let answer: Vec<SignedOp> = (1..=100)
            .map(|seq| signed(&key, artifact, seq, sealed))
            .collect();
        assert_eq!(
            stored(channels.accept_peer_history(artifact, &answer)),
            0,
            "unrequested pages are ignored"
        );
        assert!(channels.request_peer_history(artifact, 101));
        assert!(
            !channels.request_peer_history(artifact, 101),
            "an identical fresh request is not repeated"
        );
        let mut forged = signed(&key, artifact, 50, sealed);
        forged.payload.push(0);
        forged.seq = 99_999;
        let hostile = vec![
            forged,
            signed(&key, artifact, 60, None),
            signed(&key, ArtifactId::new(), 70, sealed),
            signed(&key, artifact, 200, sealed),
        ];
        assert_eq!(stored(channels.accept_peer_history(artifact, &hostile)), 0);
        assert_eq!(stored(channels.accept_peer_history(artifact, &answer)), 100);
        assert_eq!(
            stored(channels.accept_peer_history(artifact, &answer)),
            0,
            "a repeated answer adds nothing"
        );
        let (page, exhausted) = channels.scrollback_page(artifact, 101, 500);
        assert_eq!(page.len(), 100);
        assert_eq!(page.first().unwrap().seq, 1);
        assert!(exhausted);
    }
}
