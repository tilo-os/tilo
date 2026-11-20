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

use fs_protocol::TreeChunk;
use tilo_lock::MutexRecover as _;

use crate::namespace::Namespace;

/// A document saved whole.
const SAVED: &str = "automerge";
/// A document's changes since it was saved.
const JOURNAL: &str = "changes";
/// A document's journal set aside while the save that covers it is written.
const SET_ASIDE: &str = "changes.kept";

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
}

/// Where each document stands on disk.
#[derive(Debug, Default)]
struct Heads {
    /// The heads each document's save and journal hold together.
    journaled: BTreeMap<TreeChunk, Vec<automerge::ChangeHash>>,
    /// The heads each document's save holds.
    saved: BTreeMap<TreeChunk, Vec<automerge::ChangeHash>>,
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
        self.journal(namespace)
    }

    /// Add what each document of `namespace` changed since it was last kept to its journal: called
    /// under the namespace's lock, so a change is on disk before anything answers for it.
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
        reason = "the heads change with each journal written; both happen under the namespace's lock"
    )]
    pub fn journal(&self, namespace: &mut Namespace) -> io::Result<()> {
        let mut kept = self.heads.lock_recover();
        let mut documents: Vec<_> = namespace.document_heads().into_iter().collect();
        documents.sort_by_key(|(chunk, _)| (*chunk == 0, *chunk));
        for (chunk, heads) in documents {
            let known = kept.journaled.get(&chunk).cloned().unwrap_or_default();
            if known == heads {
                continue;
            }
            if let Some(write) = namespace.document_write(chunk, &known, usize::MAX)
                && !write.changes.is_empty()
            {
                append_frame(&file(&self.dir, chunk, JOURNAL), &write.changes)?;
            }
            kept.journaled.insert(chunk, heads);
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
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension() == Some(std::ffi::OsStr::new(JOURNAL)) {
                std::fs::File::open(&path)?.sync_all()?;
            }
        }
        std::fs::File::open(&self.dir)?.sync_all()
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
            self.journal(&mut namespace)?;
            let mut kept = self.heads.lock_recover();
            for (chunk, heads) in namespace.document_heads() {
                if kept.saved.get(&chunk) == Some(&heads) {
                    continue;
                }
                let Some(bytes) = namespace.save_document(chunk) else {
                    continue;
                };
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
                    eprintln!("kept tree matches: {} documents", live.len())
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

/// Add `changes` to the journal at `path` as one frame.
fn append_frame(path: &Path, changes: &[u8]) -> io::Result<()> {
    let length = u32::try_from(changes.len())
        .map_err(|_| io::Error::other("changes too long for one frame"))?;
    let mut frame = Vec::with_capacity(changes.len().saturating_add(4));
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(changes);
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    out.write_all(&frame)
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
            kept.journal(&mut namespace).unwrap();
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
        let (kept, mut namespace) = open(dir.path());
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
        assert!(kept.journal(&mut namespace).is_err());
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
        kept.journal(&mut namespace).unwrap();
        let shared = Mutex::new(namespace);
        kept.keep(&shared).unwrap();
        assert!(!file(dir.path(), 0, JOURNAL).exists());
        assert!(!file(dir.path(), 0, SET_ASIDE).exists());
        let mut namespace = shared.into_inner().unwrap();
        namespace
            .create_file(ROOT_INO, "after.txt", None, 1, 1)
            .unwrap();
        kept.journal(&mut namespace).unwrap();

        // Killed with a journal set aside and its save never written.
        for chunk in namespace.document_heads().into_keys() {
            set_aside(dir.path(), chunk).unwrap();
        }
        namespace
            .create_file(ROOT_INO, "during.txt", None, 1, 1)
            .unwrap();
        kept.journal(&mut namespace).unwrap();
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
            kept.journal(&mut namespace).unwrap();
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

    /// A frame cut off as the process was killed while adding it costs that frame alone.
    #[test]
    fn a_frame_cut_off_costs_only_itself() {
        let dir = tempfile::tempdir().unwrap();
        let (kept, mut namespace) = open(dir.path());
        namespace
            .create_file(ROOT_INO, "whole.txt", None, 1, 1)
            .unwrap();
        kept.journal(&mut namespace).unwrap();
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
