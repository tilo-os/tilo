//! A hosted workspace's durable copy of its peer state, stored in parts.
//!
//! The host's copy of a workspace is mostly its records' history, and a
//! governance change (a person let in, a role changed) touches little of it.
//! Writing the whole copy into the workspace file with every change made each
//! change cost the workspace's whole history, and a crowd joining a long-running
//! chat queued behind those writes. The copy is kept as the parts
//! [`StatePeer::durable_parts`] makes instead: small parts inline in the
//! workspace file, large ones each in a file of its own named by its content's
//! hash. A part is written once, before the workspace file or change that names
//! it, and removed once nothing written names it any more.
//!
//! A member's write changes a few parts of thousands. It is kept by appending what changed to
//! the workspace's change log (`<workspace>.changes/`), beside the workspace file: the parts
//! the list holds now, as runs of the list before it and the parts it did not hold. The
//! appended change is on disk before the write is answered, and one flush keeps every change
//! appended while the one before it was flushed ([`PartFiles::sync`]). Once the log holds as
//! much as the workspace file, the file is written again whole, away from the writes, and the
//! log's segments it holds are removed ([`PartFiles::write`]).
//!
//! The log is in numbered segments: a whole write names the segments it holds, and the changes
//! appended from then on go to the next. Before the workspace file is replaced, the last segment
//! it holds is marked with the hash of the file that holds it. Opening a workspace reads its
//! file, takes the changes of every segment after the last one marked with that file's hash,
//! checks each stored part against its hash, and restores the peer from the parts'
//! concatenation: the same bytes [`StatePeer::durable`] makes, validated the same way.
//!
//! Every opening counts itself in `<workspace>.opened`, under that file's lock, and a whole
//! write replaces the file and takes the log's segments under the same lock, only while the
//! count is the one its own opening made: a copy opened earlier, whose whole write is still
//! on its way, never replaces what a later opening reads and writes.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;

use serde::{Deserialize, Serialize};
use tilo_peer_state::StatePeer;

/// Parts at least this large get a file of their own.
const STORED_PART: usize = 16 * 1024;

/// How far ahead of the last part it found a change's diff looks for the next part nothing
/// changed, before it indexes the whole list.
const LOOKAHEAD: usize = 32;

/// The fewest bytes of changes the log holds before the workspace file is written whole again:
/// below this, a write of the file costs more than reading the changes back at a start.
const LOGGED_BEFORE_WHOLE: u64 = 1024 * 1024;

/// One part of a workspace's durable peer state, as the workspace file names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum PeerPart {
    Inline(Vec<u8>),
    /// In the workspace's parts directory, named by the hex of `digest`, the
    /// BLAKE3 hash of its `len` bytes.
    Stored {
        digest: [u8; 32],
        len: u64,
    },
}

/// A step of a change the log keeps: a run of the list of parts as it stood, or a part it did
/// not hold. The list after the change is its steps in order.
#[derive(Debug, Serialize, Deserialize)]
enum Step {
    Same { from: u64, len: u64 },
    Part(PeerPart),
}

/// A workspace's parts directory and what this process knows it holds.
pub(super) struct PeerParts {
    files: PartFiles,
    /// The large parts of the last state named, by their allocation: the
    /// peer hands out the same allocation for a part nothing changed, so
    /// such a part is neither hashed nor written again. Holding them keeps
    /// their addresses from being reused, and keeps every part the workspace
    /// file names at hand until it is written.
    staged: HashMap<usize, (Arc<[u8]>, [u8; 32])>,
    /// Every part of the last state named or logged, in order: what the next change is told
    /// apart from, by allocation.
    last: Vec<Arc<[u8]>>,
    /// Whether the record's list of parts names [`Self::last`], position for position: a
    /// change is logged against that list, and only then.
    based: bool,
    /// The order of the last write of the workspace file handed out.
    ordered: u64,
}

fn address(part: &Arc<[u8]>) -> usize {
    part.as_ptr() as usize
}

/// A workspace's parts directory, its workspace file and its change log, written by whoever
/// holds them: under the workspace's lock, or after it, when a change of the members' writes is
/// flushed or the file is written whole in the background
/// ([`super::HostedWorkspace::keep_later`]). The disk can take seconds to write a large file,
/// and nothing that reads the workspace or takes a member's write in waits for that.
#[derive(Clone)]
pub(super) struct PartFiles {
    dir: PathBuf,
    on_disk: Arc<Mutex<OnDisk>>,
    log: Arc<Mutex<Log>>,
    /// Held while the log is flushed: a writer that waited here finds its change flushed by the
    /// one before it, and one flush keeps them all.
    syncing: Arc<Mutex<()>>,
    /// Held while the workspace file is written, with the order of the newest written: an older
    /// one, written late, would name parts its successor no longer holds.
    writing: Arc<Mutex<u64>>,
    /// The file counting the parts that opened the workspace (`<workspace>.opened`), and the
    /// count this part's opening made: none for a reader, which writes nothing.
    opened: Option<(PathBuf, u64)>,
}

struct OnDisk {
    /// Every part file in the directory.
    parts: BTreeSet<[u8; 32]>,
}

/// The workspace's change log, as this process appends to it.
struct Log {
    /// The directory of its segments: `<n>` each, and `<n>.done` beside the last segment a
    /// written workspace file holds, naming that file's hash.
    dir: PathBuf,
    /// The segment changes are appended to now.
    segment: u64,
    /// Its file, once a change was appended to it, and how long it is.
    file: Option<(std::fs::File, u64)>,
    /// Segments changes were appended to that are not flushed yet.
    unflushed: Vec<std::fs::File>,
    /// A segment file was made since the directory was flushed.
    unflushed_dir: bool,
    /// The segments on disk.
    segments: BTreeSet<u64>,
    /// The large parts the changes of each segment name: kept while their segment is.
    pinned: BTreeMap<u64, BTreeSet<[u8; 32]>>,
    /// Changes appended, and of them flushed, in this process.
    appended: u64,
    flushed: u64,
    /// Bytes appended since the workspace file was last written whole, and its size then.
    logged: u64,
    whole: u64,
    /// A whole write of the workspace file is on its way.
    writing_whole: bool,
}

/// A large part by its digest, with its content where it is at hand.
type Stored = ([u8; 32], Option<Arc<[u8]>>);

/// A workspace file to write: its bytes, the large parts it names with their
/// content, its place in the order the workspace's state was named in, and the last segment
/// of the change log it holds.
pub(super) struct FileWrite {
    pub(super) path: PathBuf,
    pub(super) bytes: Vec<u8>,
    order: u64,
    /// Every large part it names, with its content where it is at hand.
    stored: Vec<Stored>,
    pub(super) holds: u64,
}

/// A change appended to the log, to be flushed before its write is answered.
#[derive(Clone, Copy, Debug)]
pub(super) struct Appended(u64);

/// What logging a workspace's change under its lock leaves to do without it: flush the change,
/// and write the file whole when that is due.
pub(super) struct Kept {
    pub(super) files: PartFiles,
    pub(super) change: Appended,
    pub(super) whole: Option<FileWrite>,
}

impl Kept {
    /// Do what is left: the change is on disk once this answers. A whole write that is due is
    /// made apart from the answer, on a thread of its own.
    pub(super) fn finish(self) -> Result<(), String> {
        let Self {
            files,
            change,
            whole,
        } = self;
        files.sync(change)?;
        if let Some(whole) = whole {
            std::thread::spawn(move || {
                if let Err(error) = files.write(&whole) {
                    tracing::warn!(%error, "a workspace file was not written whole; its change log keeps its changes");
                }
                files.log().writing_whole = false;
            });
        }
        Ok(())
    }
}

/// The frame of `change` as the log keeps it: its length (four bytes, little-endian), the first
/// eight bytes of its BLAKE3 hash, then its postcard bytes.
fn frame(change: &[Step]) -> Result<Vec<u8>, String> {
    let body = postcard::to_stdvec(change).map_err(|error| error.to_string())?;
    let len = u32::try_from(body.len()).map_err(|error| error.to_string())?;
    let mut framed = Vec::with_capacity(body.len().saturating_add(12));
    framed.extend_from_slice(&len.to_le_bytes());
    framed.extend_from_slice(&check(&body));
    framed.extend_from_slice(&body);
    Ok(framed)
}

/// The first eight bytes of the BLAKE3 hash of `body`: what tells a change appended whole from
/// one cut short.
fn check(body: &[u8]) -> [u8; 8] {
    let hash = *blake3::hash(body).as_bytes();
    hash.first_chunk::<8>().copied().unwrap_or_default()
}

/// The changes `bytes`, a segment, holds, up to the first that is not whole: a change appended
/// as the process ended was never answered, and a segment is never appended to after one.
fn changes(mut bytes: &[u8]) -> Vec<Vec<Step>> {
    let mut changes = Vec::new();
    while let Some((len, rest)) = bytes.split_first_chunk::<4>() {
        let Ok(len) = usize::try_from(u32::from_le_bytes(*len)) else {
            break;
        };
        let Some((hash, rest)) = rest.split_first_chunk::<8>() else {
            break;
        };
        let Some((body, rest)) = rest.split_at_checked(len) else {
            break;
        };
        if check(body) != *hash {
            break;
        }
        let Ok(change) = postcard::from_bytes(body) else {
            break;
        };
        changes.push(change);
        bytes = rest;
    }
    changes
}

/// `list` as `change` leaves it.
fn apply(list: Vec<PeerPart>, change: Vec<Step>) -> Result<Vec<PeerPart>, String> {
    let mut held: Vec<Option<PeerPart>> = list.into_iter().map(Some).collect();
    let mut after = Vec::with_capacity(held.len());
    for step in change {
        match step {
            Step::Part(part) => after.push(part),
            Step::Same { from, len } => {
                let from = usize::try_from(from).map_err(|error| error.to_string())?;
                let len = usize::try_from(len).map_err(|error| error.to_string())?;
                let run = held
                    .get_mut(from..from.saturating_add(len))
                    .filter(|run| run.len() == len)
                    .ok_or("a logged change names parts its list does not hold")?;
                for part in run {
                    after.push(part.take().ok_or("a logged change names a part twice")?);
                }
            }
        }
    }
    Ok(after)
}

/// The count of the parts that opened the workspace, in the file at `path`, and the file,
/// locked against every other part's opening and whole write until it is dropped.
fn opened_count(path: &Path) -> Result<(advisory_lock::FileLock, u64), String> {
    use std::io::Read as _;
    let failed = |error: std::io::Error| format!("{}: {error}", path.display());
    let mut file =
        advisory_lock::FileLock::open(path, advisory_lock::LockMode::Exclusive).map_err(failed)?;
    let mut count = [0u8; 8];
    // A file not written yet counts none.
    let read = file.read(&mut count).map_err(failed)?;
    let count = if read == count.len() {
        u64::from_le_bytes(count)
    } else {
        0
    };
    Ok((file, count))
}

/// Count this part's opening of the workspace whose count is in the file at `path`, once a
/// whole write an earlier part started is done: from now on, that part's whole writes are
/// refused ([`PartFiles::still_opened`]).
fn opening(path: &Path) -> Result<(PathBuf, u64), String> {
    use std::io::{Seek as _, Write as _};
    let (mut file, count) = opened_count(path)?;
    let count = count.wrapping_add(1);
    file.seek(std::io::SeekFrom::Start(0))
        .and_then(|_| file.write_all(&count.to_le_bytes()))
        .map_err(|error| format!("{}: {error}", path.display()))?;
    Ok((path.to_owned(), count))
}

/// The segments of a change log, and the hash each marked one names.
type Segments = (BTreeSet<u64>, BTreeMap<u64, Vec<u8>>);

/// The segments in the log directory `dir`, and the hash each marked one names.
fn segments(dir: &Path) -> Result<Segments, String> {
    let (mut found, mut marked) = (BTreeSet::new(), BTreeMap::new());
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((found, marked)),
        Err(error) => return Err(format!("{}: {error}", dir.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let (number, mark) = match name.strip_suffix(".done") {
            Some(number) => (number, true),
            None => (name, false),
        };
        let Some(segment) = tilo_failure::ignored!(
            "a name that is no segment's is an interrupted write's temporary file",
            number.parse::<u64>().ok()
        ) else {
            continue;
        };
        if mark {
            let hash = std::fs::read(entry.path()).map_err(|error| error.to_string())?;
            marked.insert(segment, hash);
        } else {
            found.insert(segment);
        }
    }
    Ok((found, marked))
}

impl PeerParts {
    /// The parts directory and change log of the workspace file at `workspace_path`, as found.
    pub(super) fn open(workspace_path: &Path) -> Result<Self, String> {
        let dir = workspace_path.with_extension("parts");
        let mut on_disk = BTreeSet::new();
        match std::fs::read_dir(&dir) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(|error| error.to_string())?;
                    let digest = entry
                        .file_name()
                        .to_str()
                        .and_then(|name| {
                            tilo_failure::ignored!(
                                "a name that is not a digest is a temporary file, removed below",
                                hex::decode(name).ok()
                            )
                        })
                        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok());
                    match digest {
                        Some(digest) => {
                            on_disk.insert(digest);
                        }
                        // An interrupted write's temporary file, or not ours.
                        None => {
                            let _ = std::fs::remove_file(entry.path());
                        }
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        let log = workspace_path.with_extension("changes");
        let (found, _) = segments(&log)?;
        let mut parts = Self::over(dir, on_disk, log, found);
        parts.files.opened = Some(opening(&workspace_path.with_extension("opened"))?);
        Ok(parts)
    }

    /// The parts directory and change log of the workspace file at `workspace_path`, for a
    /// reader that writes nothing: no part is listed, and a file that is no part is left where
    /// it is, as [`Self::open`] does not leave it. Only [`Self::load`] and [`Self::logged`] may
    /// be called on it.
    pub(super) fn reading(workspace_path: &Path) -> Self {
        Self::over(
            workspace_path.with_extension("parts"),
            BTreeSet::new(),
            workspace_path.with_extension("changes"),
            BTreeSet::new(),
        )
    }

    fn over(dir: PathBuf, parts: BTreeSet<[u8; 32]>, log: PathBuf, found: BTreeSet<u64>) -> Self {
        Self {
            files: PartFiles {
                dir,
                on_disk: Arc::new(Mutex::new(OnDisk { parts })),
                log: Arc::new(Mutex::new(Log {
                    dir: log,
                    segment: found.last().map_or(0, |last| last.saturating_add(1)),
                    file: None,
                    unflushed: Vec::new(),
                    unflushed_dir: false,
                    segments: found,
                    pinned: BTreeMap::new(),
                    appended: 0,
                    flushed: 0,
                    logged: 0,
                    whole: 0,
                    writing_whole: false,
                })),
                syncing: Arc::new(Mutex::new(())),
                writing: Arc::new(Mutex::new(0)),
                opened: None,
            },
            staged: HashMap::new(),
            last: Vec::new(),
            based: false,
            ordered: 0,
        }
    }

    /// `parts`, the list the workspace file of hash `file` names, as the changes its log kept
    /// after that file leave it. Whether the log held any.
    pub(super) fn logged(
        &self,
        file: &blake3::Hash,
        parts: &mut Vec<PeerPart>,
    ) -> Result<bool, String> {
        let dir = self.files.log().dir.clone();
        let (found, marked) = segments(&dir)?;
        let hash = file;
        let held = marked
            .iter()
            .rev()
            .find(|(_, marked)| marked.as_slice() == hash.as_bytes())
            .map(|(segment, _)| *segment);
        let mut any = false;
        for segment in found
            .into_iter()
            .filter(|segment| held.is_none_or(|held| *segment > held))
        {
            let bytes = std::fs::read(dir.join(segment.to_string()))
                .map_err(|error| format!("{}: {error}", dir.display()))?;
            for change in changes(&bytes) {
                *parts = apply(std::mem::take(parts), change)
                    .map_err(|error| format!("{}: segment {segment}: {error}", dir.display()))?;
                any = true;
            }
        }
        Ok(any)
    }

    /// `peer`'s durable state as parts for the workspace file, in memory: what
    /// [`Self::stage`] writes, or a [`FileWrite`] made of them later.
    pub(super) fn name(&mut self, peer: &StatePeer) -> Result<Vec<PeerPart>, String> {
        let parts = peer.durable_parts()?;
        let mut staged = HashMap::new();
        let mut named = Vec::with_capacity(parts.len());
        for part in &parts {
            named.push(self.name_part(part, &mut staged));
        }
        self.staged = staged;
        self.last = parts;
        self.based = false;
        Ok(named)
    }

    /// `part` as a list names it, a large one staged in `staged`.
    fn name_part(
        &self,
        part: &Arc<[u8]>,
        staged: &mut HashMap<usize, (Arc<[u8]>, [u8; 32])>,
    ) -> PeerPart {
        if part.len() < STORED_PART {
            return PeerPart::Inline(part.to_vec());
        }
        let digest = match self.staged.get(&address(part)) {
            Some((known, digest)) if Arc::ptr_eq(known, part) => *digest,
            _ => *blake3::hash(part).as_bytes(),
        };
        staged.insert(address(part), (Arc::clone(part), digest));
        PeerPart::Stored {
            digest,
            len: part.len() as u64,
        }
    }

    /// Note whether `parts`, the list just written, names the state last named position for
    /// position: changes are logged against it from then on only if it does.
    pub(super) fn written(&mut self, parts: &[PeerPart]) {
        self.based = parts.len() == self.last.len()
            && parts
                .iter()
                .zip(&self.last)
                .all(|(named, part)| match named {
                    PeerPart::Inline(bytes) => bytes.as_slice() == &part[..],
                    PeerPart::Stored { digest, len } => {
                        *len == part.len() as u64
                            && self
                                .staged
                                .get(&address(part))
                                .is_some_and(|(_, staged)| staged == digest)
                    }
                });
    }

    /// Append what `peer` changed since the state last named or logged to the change log, and
    /// take it into `list`, the record's list of parts: every large part it adds in its file
    /// first. `None` when nothing changed.
    ///
    /// # Errors
    /// The list does not name the last state ([`Self::written`]), a part could not be written
    /// or the change could not be appended: the state is written whole instead.
    pub(super) fn log(
        &mut self,
        peer: &StatePeer,
        list: &mut Vec<PeerPart>,
    ) -> Result<Option<Appended>, String> {
        if !self.based {
            return Err("the workspace file does not name the state its changes follow".into());
        }
        let parts = peer.durable_parts()?;
        // The parts nothing changed come in the order they stood in, each the allocation it was:
        // each is looked for a few places ahead of the last one found, and the whole list is
        // indexed only once that missed many times running, as after a large record went.
        let mut next = 0usize;
        let mut misses = 0usize;
        let mut index: Option<HashMap<usize, usize>> = None;
        let mut staged = HashMap::new();
        let mut change: Vec<Step> = Vec::new();
        let mut added = Vec::new();
        for part in &parts {
            let ahead = self
                .last
                .get(next..)
                .unwrap_or_default()
                .iter()
                .take(LOOKAHEAD)
                .position(|last| Arc::ptr_eq(last, part))
                .map(|skipped| next.saturating_add(skipped));
            let held = ahead.or_else(|| {
                if misses < LOOKAHEAD {
                    return None;
                }
                let index = index.get_or_insert_with(|| {
                    self.last
                        .iter()
                        .enumerate()
                        .map(|(at, part)| (address(part), at))
                        .collect()
                });
                index.get(&address(part)).copied().filter(|&at| {
                    at >= next
                        && self
                            .last
                            .get(at)
                            .is_some_and(|last| Arc::ptr_eq(last, part))
                })
            });
            if let Some(at) = held {
                misses = 0;
                next = at.saturating_add(1);
                if part.len() >= STORED_PART
                    && let Some(known) = self.staged.get(&address(part))
                {
                    staged.insert(address(part), known.clone());
                }
                match change.last_mut() {
                    Some(Step::Same { from, len }) if from.saturating_add(*len) == at as u64 => {
                        *len = len.saturating_add(1);
                    }
                    _ => change.push(Step::Same {
                        from: at as u64,
                        len: 1,
                    }),
                }
            } else {
                misses = misses.saturating_add(1);
                let named = self.name_part(part, &mut staged);
                if let PeerPart::Stored { digest, .. } = &named {
                    added.push((*digest, Some(Arc::clone(part))));
                }
                change.push(Step::Part(named));
            }
        }
        let unchanged = parts.len() == self.last.len()
            && matches!(change.as_slice(), [Step::Same { from: 0, .. }] | []);
        if unchanged {
            return Ok(None);
        }
        self.based = false;
        let framed = frame(&change)?;
        // Its parts are written and kept from removal before the change that names them is
        // appended, and both before a whole write's removal reads what is named.
        let appended = {
            let mut held = self.files.held();
            self.files.write_parts(&mut held, &added)?;
            self.files.append(&framed)?
        };
        *list = apply(std::mem::take(list), change)?;
        self.staged = staged;
        self.last = parts;
        self.based = true;
        Ok(Some(appended))
    }

    /// Whether the workspace file is due to be written whole: its log holds as much as it does.
    /// Answered once until that write is done, or [`Self::whole_dropped`].
    pub(super) fn whole_due(&self) -> bool {
        let mut log = self.files.log();
        let due = !log.writing_whole && log.logged >= log.whole.max(LOGGED_BEFORE_WHOLE);
        if due {
            log.writing_whole = true;
        }
        due
    }

    /// Changes are not logged against the record's list until it is written whole again: a
    /// change logged last may not be on disk.
    pub(super) fn unbase(&mut self) {
        self.based = false;
    }

    /// A whole write [`Self::whole_due`] answered for is not made: the next is due at once.
    pub(super) fn whole_dropped(&self) {
        self.files.log().writing_whole = false;
    }

    /// `peer`'s durable state as parts for the workspace file, every large part
    /// in its file before this returns.
    pub(super) fn stage(&mut self, peer: &StatePeer) -> Result<Vec<PeerPart>, String> {
        let named = self.name(peer)?;
        let stored = self.stored(&named);
        self.files.write_parts(&mut self.files.held(), &stored)?;
        Ok(named)
    }

    /// The workspace file `bytes`, naming `parts`, to write at `path` in the
    /// order this is called in: it holds every change logged so far, and those logged from now
    /// on follow it.
    pub(super) fn file_write(
        &mut self,
        path: &Path,
        bytes: Vec<u8>,
        parts: &[PeerPart],
    ) -> FileWrite {
        self.ordered = self.ordered.saturating_add(1);
        FileWrite {
            path: path.to_owned(),
            bytes,
            order: self.ordered,
            stored: self.stored(parts),
            holds: self.files.next_segment(),
        }
    }

    /// The files the workspace's writes go to, for a write made without the
    /// workspace.
    pub(super) fn files(&self) -> PartFiles {
        self.files.clone()
    }

    /// Every large part of `parts`, with its content where the state last
    /// named holds it: a part of an earlier state is on disk already.
    fn stored(&self, parts: &[PeerPart]) -> Vec<Stored> {
        let at_hand: HashMap<&[u8; 32], &Arc<[u8]>> = self
            .staged
            .values()
            .map(|(part, digest)| (digest, part))
            .collect();
        parts
            .iter()
            .filter_map(|part| match part {
                PeerPart::Stored { digest, .. } => {
                    Some((*digest, at_hand.get(digest).map(|part| Arc::clone(part))))
                }
                PeerPart::Inline(_) => None,
            })
            .collect()
    }

    /// The durable peer state `parts` name, whole.
    pub(super) fn load(&self, parts: &[PeerPart]) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        for part in parts {
            match part {
                PeerPart::Inline(inline) => bytes.extend_from_slice(inline),
                PeerPart::Stored { digest, len } => {
                    let stored = std::fs::read(self.files.path(digest))
                        .map_err(|error| format!("workspace state part missing: {error}"))?;
                    if stored.len() as u64 != *len || blake3::hash(&stored).as_bytes() != digest {
                        return Err("workspace state part does not match its hash".into());
                    }
                    bytes.extend_from_slice(&stored);
                }
            }
        }
        Ok(bytes)
    }

    /// Remove every part file `parts`, the workspace file as written, does not
    /// name.
    pub(super) fn collect(&self, parts: &[PeerPart]) {
        let named = parts
            .iter()
            .filter_map(|part| match part {
                PeerPart::Stored { digest, .. } => Some(*digest),
                PeerPart::Inline(_) => None,
            })
            .collect();
        self.files.collect(&mut self.files.held(), &named);
    }
    #[cfg(test)]
    pub(super) fn file_count(&self) -> usize {
        std::fs::read_dir(&self.files.dir).map_or(0, Iterator::count)
    }
}

impl PartFiles {
    fn held(&self) -> std::sync::MutexGuard<'_, OnDisk> {
        self.on_disk.lock_recover()
    }

    fn log(&self) -> std::sync::MutexGuard<'_, Log> {
        self.log.lock_recover()
    }

    fn path(&self, digest: &[u8; 32]) -> PathBuf {
        self.dir.join(hex::encode(digest))
    }

    /// Write every part of `stored` not on disk yet, each kept from removal until a workspace
    /// file written from now on names what it still needs.
    fn write_parts(&self, held: &mut OnDisk, stored: &[Stored]) -> Result<(), String> {
        if !stored.is_empty() {
            let mut log = self.log();
            let segment = log.segment;
            log.pinned
                .entry(segment)
                .or_default()
                .extend(stored.iter().map(|(digest, _)| *digest));
        }
        for (digest, part) in stored {
            if held.parts.contains(digest) {
                continue;
            }
            let part = part
                .as_ref()
                .ok_or("a workspace state part is neither on disk nor at hand")?;
            std::fs::create_dir_all(&self.dir)
                .and_then(|()| tilo_files::native::atomic::write(&self.path(digest), part))
                .map_err(|error| error.to_string())?;
            held.parts.insert(*digest);
        }
        Ok(())
    }

    /// Append `framed`, a change, to the log's segment: on disk once [`Self::sync`] answers for
    /// it. A segment an append failed in takes no more: the next change starts the next.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the log is held from the segment's opening to its count: an append is one step"
    )]
    fn append(&self, framed: &[u8]) -> Result<Appended, String> {
        let mut log = self.log();
        let log = &mut *log;
        if log.file.is_none() {
            let path = log.dir.join(log.segment.to_string());
            let file = std::fs::create_dir_all(&log.dir)
                .and_then(|()| {
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)
                })
                .map_err(|error| format!("{}: {error}", path.display()))?;
            let len = file.metadata().map_err(|error| error.to_string())?.len();
            log.file = Some((file, len));
            log.segments.insert(log.segment);
            log.unflushed_dir = true;
        }
        let Some((file, len)) = log.file.as_mut() else {
            return Err("the workspace's change log has no segment open".into());
        };
        if let Err(error) = file.write_all(framed) {
            // What was appended of this change is cut, so a start reads every change before it;
            // the segment takes nothing more either way, as a change after one cut short would
            // never be read.
            let _ = file.set_len(*len);
            if let Some((file, _)) = log.file.take() {
                log.unflushed.push(file);
            }
            log.segment = log.segment.saturating_add(1);
            return Err(format!("the workspace's change was not logged: {error}"));
        }
        *len = len.saturating_add(framed.len() as u64);
        log.logged = log.logged.saturating_add(framed.len() as u64);
        log.appended = log.appended.saturating_add(1);
        Ok(Appended(log.appended))
    }

    /// Make `change` durable: every change appended before it is flushed with it, so writers
    /// that append while another flushes are kept by the next flush, one for them all.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the count of what is flushed moves under the log's hold"
    )]
    pub(super) fn sync(&self, change: Appended) -> Result<(), String> {
        let _one = self.syncing.lock_recover();
        let (files, upto, dir) = {
            let mut log = self.log();
            if log.flushed >= change.0 {
                return Ok(());
            }
            let mut files = std::mem::take(&mut log.unflushed);
            if let Some((file, _)) = &log.file {
                files.push(file.try_clone().map_err(|error| error.to_string())?);
            }
            let dir = std::mem::take(&mut log.unflushed_dir).then(|| log.dir.clone());
            (files, log.appended, dir)
        };
        let flushed = files
            .iter()
            .try_for_each(std::fs::File::sync_data)
            .and_then(|()| match &dir {
                Some(dir) => std::fs::File::open(dir).and_then(|dir| dir.sync_all()),
                None => Ok(()),
            });
        let mut log = self.log();
        if let Err(error) = flushed {
            log.unflushed_dir |= dir.is_some();
            return Err(format!("the workspace's change was not flushed: {error}"));
        }
        log.flushed = log.flushed.max(upto);
        Ok(())
    }

    /// The last segment of the log a workspace file named now holds: changes logged from now on
    /// go to the next.
    fn next_segment(&self) -> u64 {
        let mut log = self.log();
        let holds = log.segment;
        if let Some((file, _)) = log.file.take() {
            log.unflushed.push(file);
        }
        log.segment = log.segment.saturating_add(1);
        log.logged = 0;
        holds
    }

    /// Write `write`: the parts it names that are not on disk, then the workspace file, then
    /// remove the segments of the log it holds and the parts nothing names any more. The last
    /// segment it holds is marked with its hash first, so a start that finds both reads the
    /// file without them. A write named before one already written is skipped: that one holds
    /// what this held, and more. Whether it was written.
    pub(super) fn write(&self, write: &FileWrite) -> Result<bool, String> {
        let mut written = self.writing.lock_recover();
        if write.order <= *written {
            return Ok(false);
        }
        self.write_parts(&mut self.held(), &write.stored)?;
        // Held until the segments and parts this write replaces are gone: a part that opens the
        // workspace meanwhile waits for it, and one that opened it since has the file to itself.
        let opened = self.still_opened()?;
        let (dir, held): (PathBuf, Vec<u64>) = {
            let log = self.log();
            let held = log.segments.range(..=write.holds).copied().collect();
            (log.dir.clone(), held)
        };
        let marker = dir.join(format!("{}.done", write.holds));
        if !held.is_empty() {
            tilo_files::native::atomic::write(&marker, blake3::hash(&write.bytes).as_bytes())
                .map_err(|error| format!("{}: {error}", marker.display()))?;
        }
        tilo_files::native::atomic::write(&write.path, &write.bytes)
            .map_err(|error| error.to_string())?;
        *written = write.order;
        let mut named: BTreeSet<[u8; 32]> =
            write.stored.iter().map(|(digest, _)| *digest).collect();
        // What is named is read under the parts' hold, so a part written for a change appended
        // meanwhile is either pinned already or not on the list this removes from.
        let mut parts = self.held();
        {
            let mut log = self.log();
            log.whole = write.bytes.len() as u64;
            let mut kept = true;
            for segment in &held {
                match std::fs::remove_file(dir.join(segment.to_string())) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        kept = false;
                        tracing::debug!(%error, "a change log segment the workspace file holds was not removed");
                        continue;
                    }
                }
                log.segments.remove(segment);
            }
            // The mark stays while a segment it covers does: a start would read that one again.
            if kept && !held.is_empty() {
                let _ = std::fs::remove_file(&marker);
            }
            log.pinned = log.pinned.split_off(&write.holds.saturating_add(1));
            named.extend(log.pinned.values().flatten().copied());
        }
        self.collect(&mut parts, &named);
        drop(parts);
        drop(opened);
        drop(written);
        Ok(true)
    }

    /// The opened count's file, locked, while no part opened the workspace since this one did:
    /// a part that opened it later read the file and log this one left, and may have written the
    /// file whole and taken the log's segments, which an older copy must not replace.
    fn still_opened(&self) -> Result<advisory_lock::FileLock, String> {
        let (path, count) = self
            .opened
            .as_ref()
            .ok_or("a reader of the workspace writes nothing")?;
        let (file, now) = opened_count(path)?;
        if now != *count {
            return Err(format!(
                "{}: another part opened the workspace since this one did",
                path.display()
            ));
        }
        Ok(file)
    }

    /// Remove every part file `parts`, the workspace file as written, does not
    /// name.
    fn collect(&self, held: &mut OnDisk, named: &BTreeSet<[u8; 32]>) {
        let unnamed: Vec<[u8; 32]> = held.parts.difference(named).copied().collect();
        for digest in unnamed {
            match std::fs::remove_file(self.path(&digest)) {
                Ok(()) => {
                    held.parts.remove(&digest);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    held.parts.remove(&digest);
                }
                // Kept: the next collection tries again.
                Err(error) => tracing::debug!(%error, "workspace state part not removed"),
            }
        }
    }
}
