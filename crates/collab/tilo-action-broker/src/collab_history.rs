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

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::PathBuf;

use collab_types::{ArtifactId, SignedOp};

/// Upper bound on one stored record; a document frame plus envelope is far
/// below it, so anything larger is corruption.
const MAX_RECORD_BYTES: u32 = 8 * 1024 * 1024;

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
        while let Some(header) = bytes.get(offset..offset.saturating_add(4)) {
            let len = u32::from_le_bytes(<[u8; 4]>::try_from(header).unwrap_or([0xff; 4]));
            let start = offset.saturating_add(4);
            let end = start.saturating_add(len as usize);
            let Some(record) = bytes.get(start..end).filter(|_| len <= MAX_RECORD_BYTES) else {
                break;
            };
            let Ok(op) = postcard::from_bytes::<SignedOp>(record) else {
                break;
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

    fn append(&mut self, op: &SignedOp) -> std::io::Result<()> {
        self.append_all(std::slice::from_ref(&op)).map(|_| ())
    }

    /// Append every op not already stored with one write and one data sync;
    /// returns how many were new.
    fn append_all(&mut self, ops: &[&SignedOp]) -> std::io::Result<usize> {
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
            return Ok(0);
        }
        self.file.write_all(&records)?;
        self.file.sync_data()?;
        self.end = self.end.saturating_add(records.len() as u64);
        for (seq, author, offset, len) in &staged {
            let position = self.index.partition_point(|(stored, _, _)| stored <= seq);
            self.index.insert(position, (*seq, *offset, *len));
            self.known.insert((*author, *seq));
        }
        Ok(staged.len())
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
        })
    }

    /// The session store under the launcher-provided state root.
    ///
    /// # Errors
    ///
    /// Returns an error when `XDG_STATE_HOME` is absent or unusable.
    #[cfg(not(test))]
    pub fn open_for_session() -> std::io::Result<Self> {
        let state = xdg_paths::env_dir("XDG_STATE_HOME").ok_or_else(|| {
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

    /// Append a validated op, if it belongs in durable history.
    ///
    /// # Errors
    ///
    /// Returns the I/O error; the op is still fanned out live by the caller.
    pub fn record(&mut self, op: &SignedOp) -> std::io::Result<()> {
        if !Self::retains(op) {
            return Ok(());
        }
        self.log(op.artifact)?.append(op)
    }

    /// Append the ops of `artifact` that are not already stored, with one
    /// data sync for the whole batch; returns how many were new. Callers
    /// have already filtered the ops to retained ciphertext of `artifact`.
    ///
    /// # Errors
    ///
    /// Returns the I/O error of the append; then none of the batch is new.
    pub fn record_all_if_new(
        &mut self,
        artifact: ArtifactId,
        ops: &[&SignedOp],
    ) -> std::io::Result<usize> {
        if ops.iter().any(|op| op.artifact != artifact) {
            return Err(std::io::Error::other("history batch spans artifacts"));
        }
        self.log(artifact)?.append_all(ops)
    }

    /// One scrollback page from durable history.
    ///
    /// # Errors
    ///
    /// Returns the I/O error of reading the log.
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
        AttestationId, ChannelId, ContentRetention, Ed25519PublicKey, EncryptionMetadata, OpClass,
    };

    fn op(artifact: ArtifactId, seq: u64, encryption: Option<EncryptionMetadata>) -> SignedOp {
        let mut op = SignedOp::new_unsigned(
            seq.to_le_bytes().to_vec(),
            artifact,
            Ed25519PublicKey::from_bytes([4; 32]),
            AttestationId::new(),
            OpClass::Splice,
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
            assert_eq!(store.record_all_if_new(artifact, &batch).unwrap(), 10_000);
            assert_eq!(store.record_all_if_new(artifact, &batch).unwrap(), 0);
            store.record(&op(artifact, 10_000, sealed)).unwrap();
            // Cleartext and single-use MLS messages are not retained.
            store.record(&op(artifact, 20_000, None)).unwrap();
            store
                .record(&op(
                    artifact,
                    20_001,
                    Some(EncryptionMetadata::mls_application(1, ChannelId::new())),
                ))
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
            store.record(&op(artifact, 1, sealed)).unwrap();
            store.record(&op(artifact, 2, sealed)).unwrap();
        }
        let path = root.join(format!("{}.ops", artifact.0));
        let mut bytes = fs::read(&path).unwrap();
        bytes.truncate(bytes.len() - 3);
        fs::write(&path, bytes).unwrap();
        let mut store = CollabHistoryStore::open(root).unwrap();
        store.record(&op(artifact, 3, sealed)).unwrap();
        let (ops, exhausted) = store.page_before(artifact, u64::MAX, 10).unwrap();
        assert_eq!(ops.iter().map(|op| op.seq).collect::<Vec<_>>(), [1, 3]);
        assert!(exhausted);
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
            channels.record_history(&envelope);
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
            channels.accept_peer_history(artifact, &answer),
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
        assert_eq!(channels.accept_peer_history(artifact, &hostile), 0);
        assert_eq!(channels.accept_peer_history(artifact, &answer), 100);
        assert_eq!(
            channels.accept_peer_history(artifact, &answer),
            0,
            "a repeated answer adds nothing"
        );
        let (page, exhausted) = channels.scrollback_page(artifact, 101, 500);
        assert_eq!(page.len(), 100);
        assert_eq!(page.first().unwrap().seq, 1);
        assert!(exhausted);
    }
}
