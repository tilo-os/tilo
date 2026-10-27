//! The tree document on disk: a compacted base (`state.automerge`) and a log
//! of the changes made since it was written (`state.automerge.log`).
//!
//! A save hands the writer only the changes since the save before it, cut
//! from the document on the thread that owns it ([`DocLog::cut`]). The
//! writer appends them to the log ([`DocLog::write`]), so a save costs what
//! changed rather than the whole history, and folds the log into the base
//! once it grows past [`COMPACT_AFTER_CHUNKS`] or [`COMPACT_AFTER_BYTES`].
//!
//! The log is [`MAGIC`] followed by frames of a little-endian `u32` length,
//! the payload's blake3 and the payload: the changes of one cut. A frame cut
//! short or failing its checksum ends the log. What precedes it loads, and
//! the next append writes over it, so a crash mid-append loses at most the
//! changes it was appending.
//!
//! Whoever writes the base also keeps its change hashes beside it
//! (`state.automerge.hashes`, [`hint_path`]), so session start loads it
//! without rebuilding and hashing every change: one per workspace switch, for
//! the life of the profile. The log's frames are applied in one batch, not
//! one apply (one walk of the desktop's history) per frame.
//!
//! Cuts are appended in the order they were cut, whichever save drains them:
//! a save whose state file was superseded, or whose queued write was
//! replaced by a newer one, still leaves its changes for the next drain.

use automerge::{AutoCommit, ChangeHash, LoadOptions};
use parking_lot::Mutex;
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use tilo_automerge::ApplyIncoming;

/// First bytes of a log this build writes.
const MAGIC: &[u8; 8] = b"TILOAML1";
/// Length and checksum ahead of each frame's payload.
const FRAME_HEADER: usize = 4 + 32;
/// Fold the log into the base once it holds this many frames...
pub(crate) const COMPACT_AFTER_CHUNKS: usize = 512;
/// ...or this many bytes.
pub(crate) const COMPACT_AFTER_BYTES: u64 = 8 * 1024 * 1024;
/// Changes kept for a writer that keeps failing. Past this they are dropped
/// and the next cut carries the whole history instead.
const MAX_PENDING_BYTES: usize = 64 * 1024 * 1024;

/// The log beside the base document at `base`.
pub(crate) fn log_path(base: &Path) -> PathBuf {
    let mut path = base.as_os_str().to_os_string();
    path.push(".log");
    PathBuf::from(path)
}

/// The change hashes of the base document at `base`, kept beside it by the
/// writer that wrote it so a load need not rebuild and hash every change
/// ([`app_state::load_hint`]). Bound to the base's bytes by digest:
/// a base written after it, or without one, makes it stale and ignored.
pub(crate) fn hint_path(base: &Path) -> PathBuf {
    let mut path = base.as_os_str().to_os_string();
    path.push(".hashes");
    PathBuf::from(path)
}

/// Read `path`, refusing one longer than `cap`.
pub(crate) fn read_capped(path: &Path, cap: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take((cap as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > cap {
        return Err(io::Error::other("tree document exceeds its size cap"));
    }
    Ok(bytes)
}

/// Changes handed from the document's thread to the writer.
enum Cut {
    /// What changed since the previous cut.
    Append(Vec<u8>),
    /// The whole history, for files this writer knows nothing about.
    Replace(Vec<u8>),
}

impl Cut {
    fn len(&self) -> usize {
        match self {
            Self::Append(bytes) | Self::Replace(bytes) => bytes.len(),
        }
    }
}

#[derive(Default)]
struct Pending {
    /// Heads of everything cut so far; `None` until this writer knows what
    /// the files hold, so the next cut carries the whole history.
    cursor: Option<Vec<ChangeHash>>,
    /// Cut and not yet written, oldest first.
    cuts: Vec<Cut>,
    /// The document's actor at the latest cut, for the meta.
    actor: Option<String>,
}

#[derive(Default)]
struct Files {
    base: Option<PathBuf>,
    /// Length of the log's valid frames. Anything past it is a torn frame
    /// the next append overwrites.
    log_len: u64,
    /// Frames in the log.
    chunks: usize,
    /// Actor the meta beside the base names.
    meta_actor: Option<String>,
    /// Compaction failed this session: not retried on every save.
    compaction_failed: bool,
}

/// What the log on disk held when the document was loaded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LogExtent {
    /// Bytes of valid frames, with the magic; 0 for no log.
    pub(crate) len: u64,
    /// Valid frames.
    pub(crate) chunks: usize,
    /// The log ended in a frame cut short or failing its checksum.
    pub(crate) torn: bool,
}

/// A document loaded from its base and log.
pub(crate) struct Loaded {
    pub(crate) doc: AutoCommit,
    pub(crate) extent: LogExtent,
}

/// One state file's document on disk, shared by every copy of the tree that
/// writes it.
#[derive(Default)]
pub(crate) struct DocLog {
    /// Taken briefly by cuts and drains.
    pending: Mutex<Pending>,
    /// Held across a drain and its writes, so cuts reach the files in the
    /// order they were cut.
    files: Mutex<Files>,
}

impl DocLog {
    /// Hand the writer the changes `doc` has made since the previous cut.
    /// Proportional to those changes, not to the history: no copy of the
    /// document is made. An open transaction is not part of it; it is in the
    /// next cut after it commits. A document that does not contain the
    /// previous cut (a stale copy of the tree) cuts nothing.
    pub(crate) fn cut(&self, doc: &AutoCommit) {
        let document = doc.committed_document();
        let heads = document.get_heads();
        let actor = doc.get_actor().to_hex_string();
        let mut pending = self.pending.lock();
        let cut = match &pending.cursor {
            Some(cursor) if *cursor == heads => None,
            Some(cursor) => {
                if cursor
                    .iter()
                    .any(|head| document.get_change_meta_by_hash(head).is_none())
                {
                    return;
                }
                Some(Cut::Append(document.save_after(cursor)))
            }
            None if heads.is_empty() => return,
            None => Some(Cut::Replace(document.save_after(&[]))),
        };
        pending.cursor = Some(heads);
        pending.actor = Some(actor);
        if let Some(cut) = cut.filter(|cut| cut.len() > 0) {
            pending.cuts.push(cut);
        }
        drop(pending);
    }

    /// Continue the files `doc` was just loaded from (see [`load`]): the next
    /// cut carries only what changes after this.
    pub(crate) fn attach(
        &self,
        base: &Path,
        doc: &AutoCommit,
        extent: LogExtent,
        meta_actor: Option<String>,
    ) {
        {
            let mut files = self.files.lock();
            *files = Files {
                base: Some(base.to_path_buf()),
                log_len: extent.len,
                chunks: extent.chunks,
                meta_actor,
                compaction_failed: false,
            };
        }
        let mut pending = self.pending.lock();
        *pending = Pending {
            cursor: Some(doc.committed_document().get_heads()),
            cuts: Vec::new(),
            actor: Some(doc.get_actor().to_hex_string()),
        };
        drop(pending);
    }

    /// Write every pending cut to the document at `base` and its log, then
    /// `meta` with the document's actor if it changed. A cut that fails stays
    /// pending, ahead of any cut since, for the next write.
    ///
    /// # Errors
    /// The first write that failed.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the files lock is held across every write so cuts land in the order they were cut"
    )]
    pub(crate) fn write(
        &self,
        base: &Path,
        cap: usize,
        meta: impl FnOnce(&str) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut files = self.files.lock();
        let (mut cuts, actor) = {
            let mut pending = self.pending.lock();
            if files.base.as_deref().is_some_and(|known| known != base) {
                // What this writer knows describes another file: start this
                // one over from the whole history at the next cut.
                pending.cursor = None;
                pending.cuts.clear();
                *files = Files::default();
                return Ok(());
            }
            (std::mem::take(&mut pending.cuts), pending.actor.clone())
        };
        files.base = Some(base.to_path_buf());
        // A replacement makes every cut before it redundant.
        if let Some(last) = cuts.iter().rposition(|cut| matches!(cut, Cut::Replace(_))) {
            cuts.drain(..last);
        }
        let mut cuts = cuts.into_iter();
        while let Some(cut) = cuts.next() {
            let written = match &cut {
                Cut::Replace(changes) => replace(&mut files, base, changes, cap),
                Cut::Append(changes) => append(&mut files, base, changes, cap),
            };
            if let Err(error) = written {
                self.requeue(std::iter::once(cut).chain(cuts).collect());
                return Err(error);
            }
        }
        if (files.chunks > COMPACT_AFTER_CHUNKS || files.log_len > COMPACT_AFTER_BYTES)
            && !files.compaction_failed
            && let Err(error) = compact(&mut files, base, cap)
        {
            tracing::warn!(path = %base.display(), %error, "could not fold the tree document's log into it");
            files.compaction_failed = true;
        }
        if let Some(actor) = actor
            && files.meta_actor.as_ref() != Some(&actor)
        {
            meta(&actor)?;
            files.meta_actor = Some(actor);
        }
        Ok(())
    }

    /// Put cuts that failed to write back ahead of the ones cut since.
    fn requeue(&self, mut failed: Vec<Cut>) {
        let mut pending = self.pending.lock();
        failed.append(&mut pending.cuts);
        if failed.iter().map(Cut::len).sum::<usize>() > MAX_PENDING_BYTES {
            tracing::warn!(
                "the tree document could not be written for too long; its next save rewrites it whole"
            );
            failed.clear();
            pending.cursor = None;
        }
        pending.cuts = failed;
        drop(pending);
    }

    #[cfg(test)]
    pub(crate) fn pending_bytes(&self) -> usize {
        self.pending.lock().cuts.iter().map(Cut::len).sum()
    }
}

fn other(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

/// Make `doc` the base and drop the log it supersedes.
fn write_base(files: &mut Files, base: &Path, doc: &mut AutoCommit, cap: usize) -> io::Result<()> {
    let bytes = doc.save();
    if bytes.len() > cap {
        return Err(io::Error::other(
            "tree document exceeds its size cap; its history is not saved",
        ));
    }
    tilo_files::native::atomic::replace(
        base,
        &bytes,
        tilo_files::native::atomic::Privacy::OwnerOnly,
    )
    .map_err(tilo_files::native::atomic::PublicationError::into_io_error)?;
    keep_hint(base, &bytes, doc.committed_document().change_hashes());
    // A crash before this leaves frames the new base already holds; loading
    // them again changes nothing.
    match std::fs::remove_file(log_path(base)) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    files.log_len = 0;
    files.chunks = 0;
    Ok(())
}

/// Keep `hashes`, the change hashes of `bytes` just written as the base,
/// beside it ([`hint_path`]), or drop the hint an earlier base left when this
/// one is too small to need one. Written after the base and unsynced: a hint
/// lost to a crash, torn, or left from an earlier base names other bytes, and
/// the next load ignores it and rebuilds every change. Only the next load's
/// speed depends on it, so a failure is not the save's.
fn keep_hint(base: &Path, bytes: &[u8], hashes: &[ChangeHash]) {
    let path = hint_path(base);
    let kept = match app_state::load_hint::encode(bytes, hashes) {
        Some(hint) => tilo_files::native::atomic::write_regenerable_with_mode(&path, &hint, 0o600),
        None => match std::fs::remove_file(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            removed => removed,
        },
    };
    if let Err(error) = kept {
        tracing::debug!(path = %path.display(), %error, "tree document load hint not kept");
    }
}

/// The whole history in place of whatever the files held.
fn replace(files: &mut Files, base: &Path, changes: &[u8], cap: usize) -> io::Result<()> {
    let mut doc = AutoCommit::load(changes).map_err(other)?;
    write_base(files, base, &mut doc, cap)
}

fn compact(files: &mut Files, base: &Path, cap: usize) -> io::Result<()> {
    let Some(mut loaded) = load(base, cap)? else {
        return Ok(());
    };
    write_base(files, base, &mut loaded.doc, cap)
}

fn append(files: &mut Files, base: &Path, changes: &[u8], cap: usize) -> io::Result<()> {
    let path = log_path(base);
    let payload_len = u32::try_from(changes.len()).map_err(other)?;
    let fresh = files.log_len == 0;
    let mut frame = Vec::with_capacity(
        MAGIC
            .len()
            .saturating_add(FRAME_HEADER)
            .saturating_add(changes.len()),
    );
    if fresh {
        frame.extend_from_slice(MAGIC);
    }
    frame.extend_from_slice(&payload_len.to_le_bytes());
    frame.extend_from_slice(blake3::hash(changes).as_bytes());
    frame.extend_from_slice(changes);
    let end = files.log_len.saturating_add(frame.len() as u64);
    if end > cap as u64 {
        return Err(io::Error::other(
            "tree document log exceeds its size cap; its history is not saved",
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    file.set_len(files.log_len)?;
    file.seek(io::SeekFrom::Start(files.log_len))?;
    file.write_all(&frame)?;
    file.sync_data()?;
    if fresh && let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        tilo_files::native::atomic::sync_directory(parent)?;
    }
    files.log_len = end;
    files.chunks = files.chunks.saturating_add(1);
    Ok(())
}

/// Load the document at `base` and every valid frame of its log. `None`
/// when neither exists.
///
/// # Errors
/// Either file cannot be read, or the base is not a document.
pub(crate) fn load(base: &Path, cap: usize) -> io::Result<Option<Loaded>> {
    let read = |path: &Path| match read_capped(path, cap) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    };
    let base_bytes = read(base)?;
    let log = read(&log_path(base))?;
    if base_bytes.is_none() && log.is_none() {
        return Ok(None);
    }
    let open_base = || match &base_bytes {
        Some(bytes) => load_base(base, bytes, cap),
        None => Ok(AutoCommit::new()),
    };
    let mut doc = open_base()?;
    let Some(log) = log else {
        return Ok(Some(Loaded {
            doc,
            extent: LogExtent::default(),
        }));
    };
    let (frames, extent) = read_frames(&log);
    // Every frame in one apply: Automerge walks each object a batch touches
    // once per apply, so frame by frame, one change per workspace switch,
    // walked the desktop's whole history up to 512 times. Frames
    // are checksummed cuts of one document, each depending on the ones
    // before it, so a frame Automerge could not read loses what follows it
    // either way: those changes wait for dependencies that never arrive.
    if frames.is_empty() || doc.load_incoming(&frames.concat()).is_ok() {
        return Ok(Some(Loaded { doc, extent }));
    }
    // A frame Automerge refuses: start over from the base and keep every
    // frame before it, as a frame-by-frame load would.
    let mut doc = open_base()?;
    let extent = apply_frames(&mut doc, &frames, extent);
    Ok(Some(Loaded { doc, extent }))
}

/// Load the base document `bytes` read from `base`, taking its change hashes
/// from the hint its writer kept beside it when that hint names exactly these
/// bytes ([`keep_hint`]); otherwise every change is rebuilt and hashed. The
/// log's frames are never hinted: Automerge applies and verifies them.
fn load_base(base: &Path, bytes: &[u8], cap: usize) -> io::Result<AutoCommit> {
    let known = read_capped(&hint_path(base), cap)
        .ok()
        .and_then(|hint| app_state::load_hint::hashes_for(&hint, bytes));
    let options = match known.as_deref() {
        Some(hashes) => LoadOptions::new().known_change_hashes(hashes),
        None => LoadOptions::new(),
    };
    AutoCommit::load_with_options(bytes, options).map_err(other)
}

/// The payloads of `log`'s frames up to the first that is cut short or fails
/// its checksum, and the extent they span.
fn read_frames(log: &[u8]) -> (Vec<&[u8]>, LogExtent) {
    let mut frames = Vec::new();
    let Some(mut rest) = log.strip_prefix(MAGIC.as_slice()) else {
        let extent = LogExtent {
            len: 0,
            chunks: 0,
            torn: !log.is_empty(),
        };
        return (frames, extent);
    };
    let mut extent = LogExtent {
        len: MAGIC.len() as u64,
        chunks: 0,
        torn: false,
    };
    while !rest.is_empty() {
        let frame = rest.split_first_chunk::<4>().and_then(|(len, after)| {
            let (hash, after) = after.split_first_chunk::<32>()?;
            let len = usize::try_from(u32::from_le_bytes(*len)).ok()?;
            let payload = after.get(..len)?;
            let remaining = after.get(len..)?;
            (blake3::hash(payload).as_bytes() == hash).then_some((payload, remaining))
        });
        let Some((payload, remaining)) = frame else {
            extent.torn = true;
            break;
        };
        frames.push(payload);
        extent.len = extent.len.saturating_add(frame_len(payload));
        extent.chunks = extent.chunks.saturating_add(1);
        rest = remaining;
    }
    (frames, extent)
}

/// A frame's length in the log: its header and payload.
fn frame_len(payload: &[u8]) -> u64 {
    (FRAME_HEADER as u64).saturating_add(payload.len() as u64)
}

/// Load `frames` (spanning `read`) into `doc` one at a time, stopping at the
/// first that is not changes: what precedes it loads, and the next append
/// writes over it.
fn apply_frames(doc: &mut AutoCommit, frames: &[&[u8]], read: LogExtent) -> LogExtent {
    let mut extent = LogExtent {
        len: MAGIC.len() as u64,
        chunks: 0,
        torn: read.torn,
    };
    for payload in frames {
        if doc.load_incoming(payload).is_err() {
            extent.torn = true;
            break;
        }
        extent.len = extent.len.saturating_add(frame_len(payload));
        extent.chunks = extent.chunks.saturating_add(1);
    }
    extent
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{ROOT, ReadDoc, transaction::Transactable};

    const CAP: usize = 64 * 1024 * 1024;

    fn put(doc: &mut AutoCommit, key: &str, value: i64) {
        doc.put(ROOT, key, value).expect("put");
        doc.commit();
    }

    fn value(doc: &AutoCommit, key: &str) -> Option<i64> {
        doc.get(ROOT, key)
            .expect("get")
            .and_then(|(value, _)| value.as_scalar().and_then(automerge::ScalarValue::as_i64))
    }

    fn save(log: &DocLog, doc: &AutoCommit, base: &Path) {
        log.cut(doc);
        log.write(base, CAP, |_| Ok(())).expect("write");
    }

    #[test]
    fn incremental_doc_log_round_trips_and_survives_a_torn_tail() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().join("state.automerge");
        let log = DocLog::default();
        let mut doc = AutoCommit::new();
        put(&mut doc, "base", 0);
        save(&log, &doc, &base);
        assert!(base.exists(), "the first save writes the base");
        assert!(!log_path(&base).exists(), "and no log");

        for (index, key) in ["one", "two", "three"].into_iter().enumerate() {
            put(&mut doc, key, i64::try_from(index).expect("small"));
            save(&log, &doc, &base);
        }
        let whole = load(&base, CAP).expect("load").expect("saved");
        assert_eq!(whole.extent.chunks, 3, "each save appends one frame");
        assert!(!whole.extent.torn);
        let mut reloaded = whole.doc;
        assert_eq!(reloaded.get_heads(), doc.get_heads());

        let log_file = log_path(&base);
        let len = std::fs::metadata(&log_file).expect("log").len();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&log_file)
            .expect("open log")
            .set_len(len.saturating_sub(5))
            .expect("truncate");

        let torn = load(&base, CAP)
            .expect("a torn log still loads")
            .expect("saved");
        assert!(torn.extent.torn);
        assert_eq!(torn.extent.chunks, 2);
        assert_eq!(value(&torn.doc, "base"), Some(0));
        assert_eq!(value(&torn.doc, "one"), Some(0));
        assert_eq!(value(&torn.doc, "two"), Some(1));
        assert_eq!(
            value(&torn.doc, "three"),
            None,
            "only the torn frame is lost"
        );

        let mut restarted = torn.doc;
        let next = DocLog::default();
        next.attach(&base, &restarted, torn.extent, None);
        put(&mut restarted, "four", 4);
        save(&next, &restarted, &base);
        let after = load(&base, CAP).expect("load").expect("saved");
        assert!(!after.extent.torn, "the append replaced the torn frame");
        assert_eq!(after.extent.chunks, 3);
        assert_eq!(value(&after.doc, "two"), Some(1));
        assert_eq!(value(&after.doc, "four"), Some(4));
    }

    #[test]
    fn a_save_hands_over_only_what_changed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().join("state.automerge");
        let log = DocLog::default();
        let mut doc = AutoCommit::new();
        for index in 0..2_000 {
            put(&mut doc, "counter", index);
        }
        save(&log, &doc, &base);
        put(&mut doc, "counter", -1);
        log.cut(&doc);
        let whole = doc.save().len();
        let cut = log.pending_bytes();
        assert!(cut > 0);
        assert!(
            cut.saturating_mul(20) < whole,
            "a cut carries the new change ({cut} bytes), not the history ({whole} bytes)"
        );
        log.write(&base, CAP, |_| Ok(())).expect("write");
        let mut loaded = load(&base, CAP).expect("load").expect("saved").doc;
        assert_eq!(loaded.get_heads(), doc.get_heads());
    }

    #[test]
    fn a_stale_copy_cuts_nothing_and_a_fresh_writer_replaces_the_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().join("state.automerge");
        let log = DocLog::default();
        let mut doc = AutoCommit::new();
        put(&mut doc, "a", 1);
        let mut stale = doc.fork();
        put(&mut doc, "b", 2);
        save(&log, &doc, &base);
        put(&mut doc, "c", 3);
        save(&log, &doc, &base);
        put(&mut stale, "stale", 9);
        log.cut(&stale);
        assert_eq!(
            log.pending_bytes(),
            0,
            "a copy that lacks the saved heads adds nothing"
        );

        let other_log = DocLog::default();
        let mut other = AutoCommit::new();
        put(&mut other, "other", 7);
        save(&other_log, &other, &base);
        assert!(!log_path(&base).exists(), "a replacement drops the log");
        let loaded = load(&base, CAP).expect("load").expect("saved").doc;
        assert_eq!(value(&loaded, "other"), Some(7));
        assert_eq!(value(&loaded, "c"), None);
    }

    #[test]
    fn a_long_log_is_folded_into_the_base() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().join("state.automerge");
        let log = DocLog::default();
        let mut doc = AutoCommit::new();
        put(&mut doc, "counter", 0);
        save(&log, &doc, &base);
        for index in 1..=i64::try_from(COMPACT_AFTER_CHUNKS).expect("small") {
            put(&mut doc, "counter", index);
            save(&log, &doc, &base);
        }
        assert!(log_path(&base).exists());
        put(&mut doc, "counter", -1);
        save(&log, &doc, &base);
        assert!(
            !log_path(&base).exists(),
            "past {COMPACT_AFTER_CHUNKS} frames the log is folded into the base"
        );
        let mut loaded = load(&base, CAP).expect("load").expect("saved").doc;
        assert_eq!(loaded.get_heads(), doc.get_heads());
        put(&mut doc, "counter", -2);
        save(&log, &doc, &base);
        let mut loaded = load(&base, CAP).expect("load").expect("saved");
        assert_eq!(loaded.extent.chunks, 1, "appends continue after folding");
        assert_eq!(loaded.doc.get_heads(), doc.get_heads());
    }

    /// A desktop document's shape: a peer's concurrent change merged in, then
    /// one small change per workspace switch, enough to be hinted.
    fn switched_many_times() -> AutoCommit {
        let mut doc = AutoCommit::new();
        put(&mut doc, "base", 0);
        let mut peer = doc.fork();
        put(&mut peer, "peer", 1);
        put(&mut doc, "local", 1);
        doc.load_incoming(&peer.save()).expect("merge the peer");
        for index in 0..app_state::load_hint::MIN_HINTED_CHANGES {
            put(&mut doc, "active", i64::try_from(index % 6).expect("small"));
        }
        doc
    }

    /// The change hashes a full rebuild of `bytes` lists.
    fn rebuilt_hashes(bytes: &[u8]) -> Vec<ChangeHash> {
        AutoCommit::load(bytes)
            .expect("load")
            .committed_document()
            .change_hashes()
            .to_vec()
    }

    fn loaded_hashes(base: &Path) -> Vec<ChangeHash> {
        load(base, CAP)
            .expect("load")
            .expect("saved")
            .doc
            .committed_document()
            .change_hashes()
            .to_vec()
    }

    #[test]
    fn a_written_base_keeps_a_hint_the_next_load_takes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().join("state.automerge");
        let log = DocLog::default();
        let mut doc = switched_many_times();
        save(&log, &doc, &base);
        let bytes = std::fs::read(&base).expect("base");
        assert!(bytes.len() < app_state::load_hint::MIN_HINTED_BYTES);
        let hint = std::fs::read(hint_path(&base)).expect("the base keeps a hint");
        let rebuilt = rebuilt_hashes(&bytes);
        assert_eq!(
            app_state::load_hint::hashes_for(&hint, &bytes),
            Some(rebuilt.clone()),
            "the hint lists what a full rebuild of the base would"
        );
        let mut hinted = load(&base, CAP).expect("load").expect("saved").doc;
        assert_eq!(hinted.committed_document().change_hashes(), rebuilt);
        assert_eq!(hinted.save(), bytes, "a hinted load is the same document");
        assert_eq!(hinted.get_heads(), doc.get_heads());

        // Phase `the_load_takes_the_hint_as_given`: two local changes listed
        // in swapped order (heads unchanged) come back swapped, which a
        // rebuild never produces.
        let mut swapped = rebuilt.clone();
        let last = swapped.len().saturating_sub(2);
        swapped.swap(last.saturating_sub(1), last);
        std::fs::write(
            hint_path(&base),
            app_state::load_hint::encode(&bytes, &swapped).expect("hinted"),
        )
        .expect("swapped hint");
        assert_eq!(loaded_hashes(&base), swapped);
        std::fs::write(hint_path(&base), &hint).expect("restore the hint");

        put(&mut doc, "after", 1);
        save(&log, &doc, &base);
        let loaded = load(&base, CAP).expect("load").expect("saved");
        assert_eq!(loaded.extent.chunks, 1);
        assert_eq!(value(&loaded.doc, "after"), Some(1));
        let mut expected = rebuilt;
        expected.extend(doc.get_heads());
        assert_eq!(loaded.doc.committed_document().change_hashes(), expected);

        // Phase `folding_the_log_rewrites_the_hint_for_the_new_base`.
        // One frame is in the log; past COMPACT_AFTER_CHUNKS the write folds it.
        for index in 0..i64::try_from(COMPACT_AFTER_CHUNKS).expect("small") {
            put(&mut doc, "counter", index);
            save(&log, &doc, &base);
        }
        assert!(!log_path(&base).exists(), "the log was folded");
        let folded = std::fs::read(&base).expect("base");
        assert_ne!(folded, bytes);
        let hint = std::fs::read(hint_path(&base)).expect("the folded base keeps a hint");
        let rebuilt = rebuilt_hashes(&folded);
        assert_eq!(
            app_state::load_hint::hashes_for(&hint, &folded),
            Some(rebuilt.clone())
        );
        assert_eq!(loaded_hashes(&base), rebuilt);
    }

    #[test]
    fn the_log_loads_in_one_batch_as_frame_by_frame() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().join("state.automerge");
        let log = DocLog::default();
        let mut doc = switched_many_times();
        save(&log, &doc, &base);
        for index in 0..200 {
            put(&mut doc, "active", index % 6);
            save(&log, &doc, &base);
        }
        let mut batched = load(&base, CAP).expect("load").expect("saved");
        assert_eq!(batched.extent.chunks, 200);
        assert_eq!(batched.doc.get_heads(), doc.get_heads());

        let bytes = std::fs::read(log_path(&base)).expect("log");
        let (frames, read) = read_frames(&bytes);
        assert_eq!(read, batched.extent);
        let mut one_by_one =
            load_base(&base, &std::fs::read(&base).expect("base"), CAP).expect("base");
        assert_eq!(apply_frames(&mut one_by_one, &frames, read), read);
        assert_eq!(one_by_one.get_heads(), doc.get_heads());
        assert_eq!(one_by_one.save(), batched.doc.save());
    }

    #[test]
    fn a_stale_orphaned_or_torn_hint_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().join("state.automerge");
        let log = DocLog::default();
        let doc = switched_many_times();
        save(&log, &doc, &base);
        let hint = std::fs::read(hint_path(&base)).expect("hint");
        let bytes = std::fs::read(&base).expect("base");

        let mut torn = hint.clone();
        torn.truncate(hint.len().saturating_sub(7));
        std::fs::write(hint_path(&base), &torn).expect("torn hint");
        assert_eq!(loaded_hashes(&base), rebuilt_hashes(&bytes));
        let mut wrong = hint.clone();
        if let Some(byte) = wrong.last_mut() {
            *byte ^= 1;
        }
        std::fs::write(hint_path(&base), &wrong).expect("wrong hint");
        assert_eq!(
            loaded_hashes(&base),
            rebuilt_hashes(&bytes),
            "a hash Automerge cannot place falls back to the rebuild"
        );

        std::fs::write(hint_path(&base), &hint).expect("restore the hint");
        let other_log = DocLog::default();
        let mut other = AutoCommit::new();
        put(&mut other, "other", 7);
        save(&other_log, &other, &base);
        assert!(
            !hint_path(&base).exists(),
            "a base written without a hint drops its predecessor's"
        );

        // Phase `a_stale_hint_is_ignored`: one left beside a base it was not
        // made for (a crash between the two writes, a restored file).
        std::fs::write(hint_path(&base), &hint).expect("stale hint");
        let loaded = load(&base, CAP).expect("load").expect("saved");
        assert_eq!(value(&loaded.doc, "other"), Some(7));
        assert_eq!(value(&loaded.doc, "active"), None);
        let mut loaded = loaded.doc;
        assert_eq!(loaded.get_heads(), other.get_heads());

        std::fs::remove_file(&base).expect("remove the base");
        assert!(load(&base, CAP).expect("load").is_none());
    }

    #[test]
    fn the_meta_is_written_when_the_actor_changes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().join("state.automerge");
        let log = DocLog::default();
        let mut doc = AutoCommit::new();
        let written = std::cell::RefCell::new(Vec::new());
        for index in 0..3 {
            put(&mut doc, "counter", index);
            log.cut(&doc);
            log.write(&base, CAP, |actor| {
                written.borrow_mut().push(actor.to_owned());
                Ok(())
            })
            .expect("write");
        }
        assert_eq!(*written.borrow(), vec![doc.get_actor().to_hex_string()]);
    }
}
