//! A namespace kept on disk document by document, every change of it kept the moment it is made.
//!
//! Each document of the tree (its own document, chunk 0, and each chunk) is kept in a file of its
//! own, `<chunk>.automerge`, saved whole now and then ([`KeptDocuments::keep`]): saving walks the
//! document, so it is not done on every change of a tree of thousands of files. Between two saves
//! each change is added to the document's journal, `<chunk>.changes`, as the namespace makes it
//! ([`KeptDocuments::journal`], what a mount hands every change to before it answers the request
//! that made it). Opening reads each document's save and then its journal, so a process killed at
//! any instant starts again holding every change it had answered for.
//!
//! A save replaces the journal it covers: before the document is saved the journal is set aside
//! as `<chunk>.changes.kept`, and changes made meanwhile go to a new journal; once the save is on
//! disk the journal set aside goes. A process killed between the two reads the old save, the
//! journal set aside and the new one, and a change read twice is one change.
//!
//! A journal holds frames, each the length of some changes and those changes: a frame the
//! process was killed while adding is cut off where it ends, and only that frame is lost, never
//! a change made before it.

use std::collections::BTreeMap;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use fs_protocol::TreeChunk;
use tilo_lock::MutexRecover as _;

use crate::namespace::Namespace;

/// A document saved whole.
const SAVED: &str = "automerge";
/// A document's changes since it was saved.
const JOURNAL: &str = "changes";
/// A document's journal set aside while the save that covers it is written.
const SET_ASIDE: &str = "changes.kept";
/// Where each document stood at the workspace when this device last merged it: the record's
/// heads. Beside the documents, so a restart asks each record only what changed since.
const RECORDS: &str = "records";

/// Documents of the tree as read from disk: each chunk's number and its document.
pub type Documents = Vec<(TreeChunk, Vec<u8>)>;

/// A namespace's documents kept under one directory.
#[derive(Debug)]
pub struct KeptDocuments {
    dir: PathBuf,
    heads: Mutex<Heads>,
    /// Held for a whole save: two saves that ran at once would each set a journal aside, and the
    /// older one landing last would drop the newer one's changes with the journal it removed.
    keep: Mutex<()>,
    /// Whether a frame was added since the last [`Self::sync`]: a sync with nothing new to sync
    /// touches no file, so a mount's window costs nothing while nothing is written.
    unsynced: AtomicBool,
}

/// One document's changes as they go to its journal, with the heads they leave it at.
struct Frame {
    chunk: TreeChunk,
    heads: Vec<automerge::ChangeHash>,
    changes: Vec<u8>,
}

/// Where each document stands on disk.
#[derive(Debug, Default)]
struct Heads {
    /// The heads each document's save and journal hold together.
    journaled: BTreeMap<TreeChunk, Vec<automerge::ChangeHash>>,
    /// The heads each document's save holds.
    saved: BTreeMap<TreeChunk, Vec<automerge::ChangeHash>>,
    /// Each journal a change was added to, open for appending: a copy of a photo library
    /// journals the tree and its chunk with every file, and opening and closing them each
    /// time would cost more than the changes. Set aside when the save that covers one is
    /// written, so nothing is added to a journal already on its way out.
    journals: BTreeMap<TreeChunk, std::sync::Arc<std::fs::File>>,
}

impl KeptDocuments {
    /// The documents kept in `dir`, made when it does not exist yet, and each document as kept
    /// there: its save, then its changes since, as one document (empty for none). A namespace
    /// opened from them is handed to [`Self::opened`] before anything is journaled.
    ///
    /// # Errors
    /// The directory or a document in it cannot be read.
    pub fn open(dir: &Path) -> io::Result<(Self, Documents)> {
        std::fs::create_dir_all(dir)?;
        let mut chunks = std::collections::BTreeSet::new();
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if let Some(chunk) = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.split_once('.'))
                .filter(|(_, kind)| [SAVED, JOURNAL, SET_ASIDE].contains(kind))
                .and_then(|(number, _)| number.parse::<TreeChunk>().ok())
            {
                chunks.insert(chunk);
            }
        }
        let mut documents = Vec::new();
        for chunk in chunks {
            let mut bytes = read_or_none(&file(dir, chunk, SAVED))?;
            for kind in [SET_ASIDE, JOURNAL] {
                bytes.extend(frames(&read_or_none(&file(dir, chunk, kind))?));
            }
            documents.push((chunk, bytes));
        }
        Ok((
            Self {
                dir: dir.to_path_buf(),
                heads: Mutex::default(),
                keep: Mutex::default(),
                unsynced: AtomicBool::new(true),
            },
            documents,
        ))
    }

    /// The documents [`Self::open`] answered, saves alone: what a namespace opens from when a
    /// journal does not read (damaged on disk), losing only the changes since each save.
    ///
    /// # Errors
    /// A save cannot be read.
    pub fn saved_only(&self) -> io::Result<Documents> {
        let mut documents = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let path = entry?.path();
            if let Some(chunk) = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".automerge"))
                .and_then(|number| number.parse::<TreeChunk>().ok())
            {
                documents.push((chunk, std::fs::read(&path)?));
            }
        }
        Ok(documents)
    }

    /// Where each document of the tree stood at the workspace when this device last merged it:
    /// its record's heads, as [`Self::keep_records`] wrote them. A restarted process asks each
    /// record only what changed since those heads, instead of reading what the workspace holds
    /// of it whole again.
    ///
    /// # Errors
    /// The file cannot be read, or a line in it is not what [`Self::keep_records`] writes.
    pub fn records(&self) -> io::Result<BTreeMap<TreeChunk, Vec<[u8; 32]>>> {
        let mut records = BTreeMap::new();
        let Ok(text) = std::fs::read_to_string(self.dir.join(RECORDS)) else {
            return Ok(records);
        };
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let mut words = line.split_whitespace();
            let chunk = words
                .next()
                .and_then(|chunk| chunk.parse::<TreeChunk>().ok())
                .ok_or_else(|| io::Error::other(format!("not a record line: {line}")))?;
            let mut heads = Vec::new();
            for word in words {
                let head = hex::decode(word)
                    .ok()
                    .and_then(|head| <[u8; 32]>::try_from(head).ok())
                    .ok_or_else(|| io::Error::other(format!("not a head: {word}")))?;
                heads.push(head);
            }
            records.insert(chunk, heads);
        }
        Ok(records)
    }

    /// Keep where each document stood at the workspace, as [`Self::records`] reads it back.
    ///
    /// Written after the documents are on disk, so a record's heads never say this device merged
    /// a change its own disk lost.
    ///
    /// # Errors
    /// The file cannot be written.
    pub fn keep_records(&self, records: &BTreeMap<TreeChunk, Vec<[u8; 32]>>) -> io::Result<()> {
        let mut text = String::new();
        for (chunk, heads) in records {
            text.push_str(&chunk.to_string());
            for head in heads {
                text.push(' ');
                text.push_str(&hex::encode(head));
            }
            text.push('\n');
        }
        tilo_files::native::atomic::write_with_mode(&self.dir.join(RECORDS), text.as_bytes(), 0o600)
    }

    /// `namespace` was opened from what [`Self::open`] answered, which held each document at
    /// `read` ([`Namespace::from_documents_read`]; none for a namespace made new): what opening
    /// changed since is journaled now.
    ///
    /// # Errors
    /// A journal could not be added to.
    pub fn opened(
        &self,
        namespace: &mut Namespace,
        read: BTreeMap<TreeChunk, Vec<automerge::ChangeHash>>,
    ) -> io::Result<()> {
        {
            let mut kept = self.heads.lock_recover();
            kept.saved.clone_from(&read);
            kept.journaled = read;
        }
        self.journal_locked(namespace)
    }

    /// Add what each document of `namespace` changed since it was last kept to its journal, so a
    /// change is on disk before anything answers for it. What changed is read under the
    /// namespace's lock; the frames are written once it is let go, so a write to the disk is the
    /// writer's wait, never a reader's.
    ///
    /// The tree's own document goes last. It lists the chunks, and a process that ends between
    /// two documents' frames must not come back with a tree that lists a chunk whose nodes it
    /// never journaled: that tree would go out naming a chunk nobody holds. A chunk journaled
    /// before the tree that lists it is only a chunk not listed yet.
    ///
    /// # Errors
    /// A journal could not be added to; the documents journaled before it are kept.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the heads stay held across each frame's write, so a save's set-aside never races one"
    )]
    pub fn journal(&self, namespace: &Mutex<Namespace>) -> io::Result<()> {
        let mut kept;
        let frames = {
            let mut namespace = namespace.lock_recover();
            kept = self.heads.lock_recover();
            Self::pending(&mut namespace, &kept)
        };
        // The namespace is let go; the documents' heads are not, so a save's set-aside never
        // races a frame's write.
        self.append(&mut kept, frames)
    }

    /// [`Self::journal`] for a caller already holding the namespace's lock: a changeset that
    /// writes its own step and journals it under the same hold.
    ///
    /// # Errors
    /// A journal could not be added to; the documents journaled before it are kept.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the heads stay held across each frame's write, so a save's set-aside never races one"
    )]
    pub fn journal_locked(&self, namespace: &mut Namespace) -> io::Result<()> {
        let mut kept = self.heads.lock_recover();
        let frames = Self::pending(namespace, &kept);
        self.append(&mut kept, frames)
    }

    /// The frames that carry what `namespace` changed since each document was last kept, in the
    /// order they must reach the disk.
    fn pending(namespace: &mut Namespace, kept: &Heads) -> Vec<Frame> {
        let mut documents: Vec<_> = namespace.document_heads().into_iter().collect();
        documents.sort_by_key(|(chunk, _)| (*chunk == 0, *chunk));
        let mut frames = Vec::new();
        for (chunk, heads) in documents {
            let known = kept.journaled.get(&chunk).cloned().unwrap_or_default();
            if known == heads {
                continue;
            }
            let changes = namespace
                .document_write(chunk, &known, usize::MAX)
                .map_or_else(Vec::new, |write| write.changes);
            frames.push(Frame {
                chunk,
                heads,
                changes,
            });
        }
        frames
    }

    /// Write `frames` to their journals, in order, recording each document's heads as its frame
    /// lands. A frame that fails leaves its heads unrecorded, so the next pass writes it again.
    fn append(&self, kept: &mut Heads, frames: Vec<Frame>) -> io::Result<()> {
        for frame in frames {
            if !frame.changes.is_empty() {
                append_frame(&self.dir, kept, frame.chunk, &frame.changes)?;
                self.unsynced.store(true, Ordering::Release);
            }
            kept.journaled.insert(frame.chunk, frame.heads);
        }
        Ok(())
    }

    /// Make every journal frame this has appended so far durable, then the directory that names
    /// them: what a mount calls from its flusher and from `fsync`, after the content those frames
    /// describe is durable. One sync per file here, not per change.
    ///
    /// # Errors
    /// A journal or the directory could not be synced.
    pub fn sync(&self) -> io::Result<()> {
        if !self.unsynced.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        self.sync_all()
            .inspect_err(|_| self.unsynced.store(true, Ordering::Release))
    }

    fn sync_all(&self) -> io::Result<()> {
        // The journals this process added to are synced through their open files: a copy
        // journals every few milliseconds, and opening each journal again to sync it would
        // cost what appending it did. A journal nothing added to here (one a killed process
        // left frames in) is still read from disk and synced.
        let held: Vec<(TreeChunk, std::sync::Arc<std::fs::File>)> = {
            let kept = self.heads.lock_recover();
            kept.journals
                .iter()
                .map(|(chunk, journal)| (*chunk, std::sync::Arc::clone(journal)))
                .collect()
        };
        // The syncs run outside the heads lock: a change waits for it to journal, and a copy
        // makes one every few milliseconds.
        for (chunk, journal) in &held {
            journal
                .sync_all()
                .map_err(|error| naming(&file(&self.dir, *chunk, JOURNAL), &error))?;
        }
        let held: Vec<TreeChunk> = held.into_iter().map(|(chunk, _)| chunk).collect();
        self.sync_listed(journals(&self.dir)?, &held)?;
        std::fs::File::open(&self.dir)
            .and_then(|dir| dir.sync_all())
            .map_err(|error| naming(&self.dir, &error))
    }

    /// Sync the journals the directory listed, save for those in `held` (synced through the
    /// files this process keeps open for them).
    ///
    /// A save running beside this sync sets a journal aside between the listing and here (every
    /// start saves what an earlier process journaled): its frames are synced where they went,
    /// and nothing is left to sync once that save let go of them too.
    fn sync_listed(
        &self,
        listed: Vec<(Option<TreeChunk>, PathBuf)>,
        held: &[TreeChunk],
    ) -> io::Result<()> {
        for (chunk, path) in listed {
            if chunk.is_some_and(|chunk| held.contains(&chunk)) {
                continue;
            }
            let aside = chunk.map(|chunk| file(&self.dir, chunk, SET_ASIDE));
            sync_if_present(&path).and_then(|synced| match (synced, aside) {
                (false, Some(aside)) => sync_if_present(&aside).map(drop),
                _ => Ok(()),
            })?;
        }
        Ok(())
    }

    /// Save each document of `namespace` that changed since its last save, whole, and let go of
    /// the journal that save covers. The namespace is locked while the documents are saved to
    /// memory, not while they are written.
    ///
    /// # Errors
    /// A document could not be written; it is saved again next time, and its journal stays.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "a journal is set aside and its document saved under one lock of the namespace"
    )]
    pub fn keep(&self, namespace: &Mutex<Namespace>) -> io::Result<()> {
        let _keeping = self.keep.lock_recover();
        let mut saved = Vec::new();
        {
            let mut namespace = namespace.lock_recover();
            // What the namespace changed since the last journal goes into it first: the journal
            // set aside below then holds every change up to the save.
            let mut kept = self.heads.lock_recover();
            let frames = Self::pending(&mut namespace, &kept);
            self.append(&mut kept, frames)?;
            for (chunk, heads) in namespace.document_heads() {
                if kept.saved.get(&chunk) == Some(&heads) {
                    continue;
                }
                let Some(bytes) = namespace.save_document(chunk) else {
                    continue;
                };
                // The journal is renamed, so nothing may add to it through a file opened
                // before the rename: the next change opens the new one.
                kept.journals.remove(&chunk);
                set_aside(&self.dir, chunk)?;
                kept.journaled.insert(chunk, heads.clone());
                saved.push((chunk, heads, bytes));
            }
        }
        let mut failed = None;
        for (chunk, heads, bytes) in saved {
            match tilo_files::native::atomic::write_with_mode(
                &file(&self.dir, chunk, SAVED),
                &bytes,
                0o600,
            ) {
                Ok(()) => {
                    self.heads.lock_recover().saved.insert(chunk, heads);
                    remove_or_none(&file(&self.dir, chunk, SET_ASIDE))?;
                }
                Err(error) => failed = Some(error),
            }
        }
        // What was written is read back and compared with the tree, for isolating a save that
        // claims a generation it did not write.
        if std::env::var_os("TILO_FS_KEEP_VERIFY").is_some() {
            let live = namespace.lock_recover().document_heads();
            match KeptDocuments::open(&self.dir)
                .and_then(|(_, documents)| {
                    Namespace::from_documents_read("verify", documents).map_err(io::Error::other)
                })
                .map(|(mut reopened, _)| reopened.document_heads())
            {
                Ok(disk) if disk == live => {
                    eprintln!("kept tree matches: {} documents", live.len());
                }
                Ok(disk) => {
                    let differs: Vec<_> = live
                        .iter()
                        .filter(|(chunk, heads)| disk.get(chunk) != Some(heads))
                        .map(|(chunk, _)| *chunk)
                        .collect();
                    eprintln!(
                        "kept tree differs in {differs:?}: live {} documents, disk {}",
                        live.len(),
                        disk.len()
                    );
                }
                Err(error) => eprintln!("kept tree does not read back: {error}"),
            }
        }
        failed.map_or(Ok(()), Err)
    }
}

fn file(dir: &Path, chunk: TreeChunk, kind: &str) -> PathBuf {
    dir.join(format!("{chunk}.{kind}"))
}

fn read_or_none(path: &Path) -> io::Result<Vec<u8>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

/// Sync the file at `path` to the disk; false when there is no such file.
fn sync_if_present(path: &Path) -> io::Result<bool> {
    match std::fs::File::open(path) {
        Ok(file) => file
            .sync_all()
            .map(|()| true)
            .map_err(|error| naming(path, &error)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(naming(path, &error)),
    }
}

/// The journals in `dir` as the directory lists them, each with its chunk when its name names
/// one: what [`KeptDocuments::sync_listed`] syncs.
fn journals(dir: &Path) -> io::Result<Vec<(Option<TreeChunk>, PathBuf)>> {
    let mut listed = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension() != Some(std::ffi::OsStr::new(JOURNAL)) {
            continue;
        }
        let chunk = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(JOURNAL))
            .and_then(|number| number.strip_suffix('.'))
            .and_then(|number| number.parse::<TreeChunk>().ok());
        listed.push((chunk, path));
    }
    Ok(listed)
}

/// `error`, saying the file it happened to.
fn naming(path: &Path, error: &io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}

fn remove_or_none(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Set document `chunk`'s journal aside for the save about to be written. A journal already set
/// aside (a save that failed) keeps it, with this one's frames after its own.
fn set_aside(dir: &Path, chunk: TreeChunk) -> io::Result<()> {
    let journal = file(dir, chunk, JOURNAL);
    let aside = file(dir, chunk, SET_ASIDE);
    if !aside.exists() {
        return match std::fs::rename(&journal, &aside) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
    }
    let bytes = read_or_none(&journal)?;
    if !bytes.is_empty() {
        let mut out = std::fs::OpenOptions::new().append(true).open(&aside)?;
        out.write_all(&bytes)?;
    }
    remove_or_none(&journal)
}

/// Add `changes` to `chunk`'s journal as one frame, through the file kept open for it. A write
/// that fails drops that file, so the next append opens the journal again rather than adding to
/// one whose frame may be cut off.
fn append_frame(dir: &Path, kept: &mut Heads, chunk: TreeChunk, changes: &[u8]) -> io::Result<()> {
    let length = u32::try_from(changes.len())
        .map_err(|_| io::Error::other("changes too long for one frame"))?;
    let mut frame = Vec::with_capacity(changes.len().saturating_add(4));
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(changes);
    let journal = match kept.journals.entry(chunk) {
        std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
        std::collections::btree_map::Entry::Vacant(entry) => entry.insert(std::sync::Arc::new(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(file(dir, chunk, JOURNAL))?,
        )),
    };
    if let Err(error) = (&**journal).write_all(&frame) {
        kept.journals.remove(&chunk);
        return Err(error);
    }
    Ok(())
}

/// The changes a journal's frames hold, one after another; a frame cut off ends them.
fn frames(journal: &[u8]) -> Vec<u8> {
    let mut changes = Vec::new();
    let mut rest = journal;
    while let Some((length, after)) = rest.split_first_chunk::<4>() {
        let length = u32::from_le_bytes(*length) as usize;
        let Some((frame, after)) = after.split_at_checked(length) else {
            break;
        };
        changes.extend_from_slice(frame);
        rest = after;
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::ROOT_INO;

    fn open(dir: &Path) -> (KeptDocuments, Namespace) {
        let (kept, documents) = KeptDocuments::open(dir).unwrap();
        let (mut namespace, read) = if documents.is_empty() {
            (Namespace::new("laptop").unwrap(), BTreeMap::new())
        } else {
            Namespace::from_documents_read("laptop", documents).unwrap()
        };
        kept.opened(&mut namespace, read).unwrap();
        (kept, namespace)
    }

    fn holds(namespace: &mut Namespace, name: &str) -> bool {
        namespace.lookup(ROOT_INO, name).is_some()
    }

    /// A save that sets a journal aside between the sync listing it and opening it costs the
    /// sync nothing: the listed file is gone, and its frames went where the save put them. It
    /// failed with "No such file or directory" a few times every time a folder's sync started,
    /// which saves what it merged at once.
    #[test]
    fn a_journal_a_save_set_aside_between_the_listing_and_the_sync_is_not_missed() {
        let dir = tempfile::tempdir().unwrap();
        let (kept, mut namespace) = open(dir.path());
        namespace
            .create_file(ROOT_INO, "written.txt", None, 1, 1)
            .unwrap();
        kept.journal_locked(&mut namespace).unwrap();
        // What the sync lists, and then what a save running beside it leaves of it.
        let listed = journals(dir.path()).unwrap();
        assert!(!listed.is_empty(), "a journal to sync");
        let mut set_aside_count = 0;
        for chunk in namespace.document_heads().into_keys() {
            if !file(dir.path(), chunk, JOURNAL).exists() {
                continue;
            }
            set_aside(dir.path(), chunk).unwrap();
            assert!(
                file(dir.path(), chunk, SET_ASIDE).exists(),
                "chunk {chunk} is set aside"
            );
            set_aside_count += 1;
        }
        assert!(set_aside_count > 0, "a journal set aside");
        kept.sync_listed(listed, &[]).expect(
            "a sync whose journals a save set aside names no missing file: the frames are in the \
             set-aside copy",
        );
    }

    /// Syncing while saves set journals aside never fails: a save renames a journal between
    /// the sync listing it and opening it, which failed with "No such file or directory" a few
    /// times every time a folder's sync started (it saves what it merged at once).
    #[test]
    fn a_sync_beside_saves_never_misses_a_journal() {
        let dir = tempfile::tempdir().unwrap();
        let (kept, namespace) = open(dir.path());
        let namespace = Mutex::new(namespace);
        let done = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let syncing = scope.spawn(|| {
                while !done.load(Ordering::Acquire) {
                    kept.unsynced.store(true, Ordering::Release);
                    kept.sync()?;
                }
                io::Result::Ok(())
            });
            for n in 0..6_000 {
                let mut locked = namespace.lock_recover();
                locked
                    .create_file(ROOT_INO, format!("file-{n}.txt"), None, 1, 1)
                    .unwrap();
                kept.journal_locked(&mut locked).unwrap();
                drop(locked);
                if n % 25 == 0 {
                    kept.keep(&namespace).unwrap();
                }
            }
            done.store(true, Ordering::Release);
            syncing
                .join()
                .unwrap()
                .expect("a sync beside the saves failed");
        });
    }

    /// Thousands of files written, the process killed without a save since the first: every
    /// file journaled is there when it starts again.
    #[test]
    fn every_change_journaled_outlives_a_kill_without_a_save() {
        let dir = tempfile::tempdir().unwrap();
        let (kept, mut namespace) = open(dir.path());
        for n in 0..2_500 {
            namespace
                .create_file(ROOT_INO, format!("file-{n}.txt"), None, 1, 1)
                .unwrap();
            kept.journal_locked(&mut namespace).unwrap();
        }
        let heads = namespace.document_heads();
        assert!(heads.len() > 2, "the files fill more than one chunk");
        drop((kept, namespace));

        let (_, mut again) = open(dir.path());
        assert_eq!(again.document_heads(), heads);
        for n in [0, 1_023, 1_024, 2_499] {
            assert!(holds(&mut again, &format!("file-{n}.txt")), "file-{n}.txt");
        }
    }

    /// The tree's own document is journaled after the chunks it lists: a process that ends
    /// between the two comes back with a chunk not listed yet, never with a tree listing a chunk
    /// whose nodes it lost.
    #[test]
    fn a_new_chunk_is_journaled_before_the_tree_that_lists_it() {
        let dir = tempfile::tempdir().unwrap();
        let (kept, namespace) = open(dir.path());
        // A save covers what opening journaled and lets go of the journals opened for it, so
        // the tree's journal is opened fresh below, where the test has made it unwritable.
        let shared = Mutex::new(namespace);
        kept.keep(&shared).unwrap();
        let mut namespace = shared.into_inner().unwrap();
        let before = namespace.document_heads().len();
        let mut n = 0;
        while namespace.document_heads().len() == before {
            namespace
                .create_file(ROOT_INO, format!("file-{n}.txt"), None, 1, 1)
                .unwrap();
            n += 1;
        }
        let new: Vec<TreeChunk> = namespace
            .document_heads()
            .into_keys()
            .filter(|chunk| *chunk != 0)
            .collect();
        // The tree's own journal cannot be written: the process ends there.
        let tree = file(dir.path(), 0, JOURNAL);
        let _ = std::fs::remove_file(&tree);
        std::fs::create_dir(&tree).unwrap();
        assert!(kept.journal_locked(&mut namespace).is_err());
        for chunk in new {
            let journal = std::fs::metadata(file(dir.path(), chunk, JOURNAL));
            assert!(
                journal.is_ok_and(|journal| journal.len() > 0),
                "chunk {chunk} is journaled before the tree's own document"
            );
        }
    }

    /// A save replaces the journal it covers; changes after it journal again, and a kill
    /// between setting the journal aside and the save landing loses nothing.
    #[test]
    fn a_save_takes_the_journals_place_and_a_kill_mid_save_loses_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (kept, mut namespace) = open(dir.path());
        namespace
            .create_file(ROOT_INO, "before.txt", None, 1, 1)
            .unwrap();
        kept.journal_locked(&mut namespace).unwrap();
        let shared = Mutex::new(namespace);
        kept.keep(&shared).unwrap();
        assert!(!file(dir.path(), 0, JOURNAL).exists());
        assert!(!file(dir.path(), 0, SET_ASIDE).exists());
        let mut namespace = shared.into_inner().unwrap();
        namespace
            .create_file(ROOT_INO, "after.txt", None, 1, 1)
            .unwrap();
        kept.journal_locked(&mut namespace).unwrap();

        // Killed with a journal set aside and its save never written.
        for chunk in namespace.document_heads().into_keys() {
            set_aside(dir.path(), chunk).unwrap();
        }
        namespace
            .create_file(ROOT_INO, "during.txt", None, 1, 1)
            .unwrap();
        kept.journal_locked(&mut namespace).unwrap();
        let heads = namespace.document_heads();
        drop((kept, namespace));

        let (_, mut again) = open(dir.path());
        for name in ["before.txt", "after.txt", "during.txt"] {
            assert!(holds(&mut again, name), "{name}");
        }
        assert_eq!(again.document_heads(), heads);
    }

    /// A directory full of files, journaled and saved, opens again as the same tree: what a
    /// stopped daemon must come back to.
    #[test]
    fn a_folder_of_files_saved_and_reopened_holds_every_one() {
        let dir = tempfile::tempdir().unwrap();
        let (kept, mut namespace) = open(dir.path());
        let folder = namespace
            .create_dir(ROOT_INO, "in", 1)
            .expect("make the folder");
        for n in 0..300 {
            namespace
                .create_file(folder, format!("f{n:07}"), None, 1, 1)
                .unwrap();
            kept.journal_locked(&mut namespace).unwrap();
        }
        kept.keep(&Mutex::new(namespace)).unwrap();
        drop(kept);

        let (_, documents) = KeptDocuments::open(dir.path()).unwrap();
        let (mut again, _) = Namespace::from_documents_read("reader", documents).unwrap();
        let folder = again
            .lookup(ROOT_INO, "in")
            .expect("the folder is back")
            .ino
            .0;
        assert_eq!(
            again.readdir(folder).map(|entries| entries.len()),
            Some(302)
        );
    }

    /// Where each record stood at the workspace is kept beside the documents and reads back as
    /// it was written: what a restarted holder asks each record's changes since, instead of
    /// reading everything the workspace holds of it again.
    #[test]
    fn where_a_record_stood_reads_back_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let (kept, mut namespace) = open(dir.path());
        namespace
            .create_file(ROOT_INO, "kept.txt", None, 1, 1)
            .unwrap();
        kept.journal_locked(&mut namespace).unwrap();
        let stood: BTreeMap<TreeChunk, Vec<[u8; 32]>> = namespace
            .document_heads()
            .into_iter()
            .map(|(chunk, heads)| (chunk, heads.into_iter().map(|head| head.0).collect()))
            .collect();
        assert!(stood.values().any(|heads| !heads.is_empty()), "{stood:?}");
        kept.keep_records(&stood).unwrap();
        drop(kept);

        let (again, _) = KeptDocuments::open(dir.path()).unwrap();
        assert_eq!(again.records().unwrap(), stood);
        // And what is not a record line is no place at all rather than a start refused.
        std::fs::write(dir.path().join(RECORDS), "not a number\n").unwrap();
        assert!(again.records().is_err());
    }

    /// A frame cut off as the process was killed while adding it costs that frame alone.
    #[test]
    fn a_frame_cut_off_costs_only_itself() {
        let dir = tempfile::tempdir().unwrap();
        let (kept, mut namespace) = open(dir.path());
        namespace
            .create_file(ROOT_INO, "whole.txt", None, 1, 1)
            .unwrap();
        kept.journal_locked(&mut namespace).unwrap();
        for chunk in namespace.document_heads().into_keys() {
            let journal = file(dir.path(), chunk, JOURNAL);
            if journal.exists() {
                let mut out = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&journal)
                    .unwrap();
                out.write_all(&[200, 0, 0, 0, 1, 2, 3]).unwrap();
            }
        }
        drop((kept, namespace));

        let (_, mut again) = open(dir.path());
        assert!(holds(&mut again, "whole.txt"));
    }
}
