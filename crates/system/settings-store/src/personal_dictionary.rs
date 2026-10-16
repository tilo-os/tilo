//! Local personal vocabulary shared by input consumers, outside room records.
//!
//! The session host owns [`Service`]. It supplies a private local root and binds
//! the authenticated person's root key; neither is discovered from an app's
//! environment. Consumers receive only [`Reader`], never a filesystem path or
//! the authority to switch people. Anonymous/demo sessions have no reader.
//! Nothing in this module exports records, changes settings grants, or syncs.
//! Hosts must bind again on account changes and issue fresh readers. Old
//! readers fail closed, including after switching back to the same person.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;
use unicode_normalization::UnicodeNormalization;

pub mod host;

const MAX_WORDS: usize = 1_024;
const MAX_FILE_BYTES: usize = 512 * 1_024;
const VERSION: u32 = 1;

/// An explicit user addition. Display spelling is retained, including casing.
/// `None` means all languages; a tag such as `fr` restricts the hint to that
/// language and its regional variants. Additions never mean replacement rules.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Word {
    pub spelling: String,
    pub language: Option<String>,
}

impl Word {
    /// Validate and canonicalise an addition, retaining its display casing.
    ///
    /// # Errors
    /// Empty, oversized or non-word text, or an invalid language tag.
    pub fn new(spelling: &str, language: Option<&str>) -> io::Result<Self> {
        let spelling: String = spelling.trim().nfc().collect();
        if spelling.is_empty()
            || spelling.chars().count() > 128
            || !spelling.chars().any(char::is_alphabetic)
            || spelling
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(invalid("enter one word of at most 128 characters"));
        }
        let language = language.map(str::to_ascii_lowercase);
        if language.as_deref().is_some_and(|tag| {
            tag.len() > 35
                || tag.split('-').any(|part| {
                    part.is_empty()
                        || part.len() > 8
                        || !part.bytes().all(|c| c.is_ascii_alphanumeric())
                })
                || !tag.split('-').next().is_some_and(|part| {
                    (2..=8).contains(&part.len()) && part.bytes().all(|c| c.is_ascii_alphabetic())
                })
        }) {
            return Err(invalid("invalid language tag"));
        }
        Ok(Self { spelling, language })
    }

    fn same_key(&self, other: &Self) -> bool {
        self.language == other.language && folded(&self.spelling) == folded(&other.spelling)
    }

    fn applies(&self, language: &str) -> bool {
        self.language.as_deref().is_none_or(|tag| {
            language.eq_ignore_ascii_case(tag)
                || language
                    .to_ascii_lowercase()
                    .strip_prefix(tag)
                    .is_some_and(|tail| tail.starts_with('-'))
        })
    }
}

fn folded(word: &str) -> String {
    word.nfc().flat_map(char::to_lowercase).nfc().collect()
}

/// A current, bounded set of hints. Consumers retain user-entered text; they
/// use these spellings only for visible suggestions or recognition biasing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub revision: u64,
    pub words: Vec<Word>,
}

impl Snapshot {
    /// Preview a queued edit without writing a second vocabulary store.
    /// Imports wait for the host's ownership/migration decision before words
    /// are shown. All other changes use the same canonical word rules.
    ///
    /// # Errors
    /// An invalid entry, missing edit target, duplicate target or full set.
    pub fn preview(&self, edit: &Edit) -> io::Result<Self> {
        let mut next = self.clone();
        match edit {
            Edit::Add(word) => insert(&mut next.words, validate(word)?)?,
            Edit::Replace { old, new } => {
                let new = validate(new)?;
                let index = next
                    .words
                    .iter()
                    .position(|word| word.same_key(old))
                    .ok_or_else(|| invalid("the word no longer exists"))?;
                if next
                    .words
                    .iter()
                    .enumerate()
                    .any(|(i, word)| i != index && word.same_key(&new))
                {
                    return Err(invalid("the edited word already exists"));
                }
                if let Some(word) = next.words.get_mut(index) {
                    *word = new;
                }
            }
            Edit::Remove(old) => next.words.retain(|word| !word.same_key(old)),
            Edit::Reset => next.words.clear(),
        }
        next.revision = next
            .revision
            .checked_add(1)
            .ok_or_else(|| invalid("dictionary revision exhausted"))?;
        Ok(next)
    }
    /// Hints with their exact display spellings, applicable to this language.
    pub fn hints<'a>(&'a self, language: &'a str) -> impl Iterator<Item = &'a str> {
        self.words
            .iter()
            .filter(move |w| w.applies(language))
            .map(|w| w.spelling.as_str())
    }

    /// Protect an explicit addition from autocorrection, without changing its
    /// typed casing. Unicode canonical equivalence is accepted.
    pub fn contains(&self, typed: &str, language: &str) -> bool {
        let key = folded(typed);
        self.hints(language).any(|hint| folded(hint) == key)
    }

    /// Explicit completion chips, preserving casing. No edit-distance or
    /// substring replacements: a personal word is a hint, not a rewrite rule.
    pub fn completions(&self, prefix: &str, language: &str, limit: usize) -> Vec<String> {
        if prefix.is_empty() || limit == 0 {
            return Vec::new();
        }
        let key = folded(prefix);
        let mut result = Vec::new();
        for word in self.hints(language).filter(|w| folded(w).starts_with(&key)) {
            if !result.iter().any(|w: &String| folded(w) == folded(word)) {
                result.push(word.to_owned());
            }
            if result.len() == limit {
                break;
            }
        }
        result
    }
}

/// Small read/biasing interface for OSK, spellcheck and on-device dictation.
/// Read before each input operation; an error must clear cached personal hints.
pub trait Vocabulary: Send + Sync {
    /// # Errors
    /// Revoked account binding, unavailable storage, or invalid persisted data.
    fn snapshot(&self) -> io::Result<Snapshot>;
}

/// An explicit edit. Replace/remove name the old entry; optimistic revisions
/// keep a stale settings screen from deleting or overwriting newer additions.
#[derive(Clone, Serialize, Deserialize)]
pub enum Edit {
    Add(Word),
    Replace { old: Word, new: Word },
    Remove(Word),
    Reset,
}

#[derive(Serialize, Deserialize)]
struct Document {
    version: u32,
    owner: [u8; 32],
    revision: u64,
    words: Vec<Word>,
}

impl Document {
    fn empty(owner: [u8; 32]) -> Self {
        Self {
            version: VERSION,
            owner,
            revision: 0,
            words: Vec::new(),
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            revision: self.revision,
            words: self.words.clone(),
        }
    }
}

struct Binding {
    owner: Option<[u8; 32]>,
    epoch: u64,
}

struct Inner {
    root: PathBuf,
    binding: Mutex<Binding>,
    changes: crate::ChangeSignal,
}

/// Host-owned local dictionary service. The root must be a trusted private
/// directory outside app/room stores, controlled by the session's OS account.
/// Do not hand this authority or its root to untrusted apps or collaborators.
pub struct Service(Arc<Inner>);

impl Drop for Service {
    fn drop(&mut self) {
        // Consumers must not keep account authority alive after its host exits.
        self.0.binding.lock_recover().owner = None;
        self.0.changes.ring();
    }
}

/// Revocable read capability for one authenticated binding. Clones share the
/// binding; account switching invalidates every old clone.
#[derive(Clone)]
pub struct Reader {
    inner: Arc<Inner>,
    epoch: u64,
}

impl Service {
    /// Create an unbound service. A host must establish identity explicitly.
    ///
    /// # Errors
    /// The root is relative. Directory creation waits until the first write.
    pub fn new(root: PathBuf) -> io::Result<Self> {
        if !root.is_absolute() {
            return Err(invalid(
                "dictionary root must be an absolute host-owned path",
            ));
        }
        Ok(Self(Arc::new(Inner {
            root,
            binding: Mutex::new(Binding {
                owner: None,
                epoch: 0,
            }),
            changes: crate::ChangeSignal::default(),
        })))
    }

    /// Called by the host after authenticating/switching the local account.
    /// `None` revokes access (logout, unverified identity, public demo).
    pub fn bind(&self, owner: Option<[u8; 32]>) -> Option<Reader> {
        let mut binding = self.0.binding.lock_recover();
        let Some(epoch) = binding.epoch.checked_add(1) else {
            binding.owner = None;
            return None;
        };
        binding.epoch = epoch;
        binding.owner = owner;
        let reader = owner.map(|_| Reader {
            inner: Arc::clone(&self.0),
            epoch: binding.epoch,
        });
        drop(binding);
        self.0.changes.ring();
        reader
    }

    /// The currently authenticated host binding; never inferred from an app.
    pub fn owner(&self) -> Option<[u8; 32]> {
        self.0.binding.lock_recover().owner
    }

    /// Issue a reader for the current binding without changing its epoch.
    pub fn reader(&self) -> Option<Reader> {
        let binding = self.0.binding.lock_recover();
        let reader = binding.owner.map(|_| Reader {
            inner: Arc::clone(&self.0),
            epoch: binding.epoch,
        });
        drop(binding);
        reader
    }

    /// Issue a reader only when the expected account is the live binding.
    pub(crate) fn reader_for(&self, owner: [u8; 32]) -> Option<Reader> {
        let binding = self.0.binding.lock_recover();
        (binding.owner == Some(owner)).then(|| Reader {
            inner: Arc::clone(&self.0),
            epoch: binding.epoch,
        })
    }

    /// Watch other host instances' atomic edits without retaining authority.
    #[cfg(feature = "watch")]
    pub(crate) fn watch(&self) -> io::Result<crate::watch::SettingsWatch> {
        create_private_directory(&self.0.root)?;
        let inner = Arc::downgrade(&self.0);
        crate::watch::watch_dictionary_at(&self.0.root, move || {
            if let Some(inner) = inner.upgrade() {
                inner.changes.ring();
            }
        })
    }

    pub fn changes(&self) -> &crate::ChangeSignal {
        &self.0.changes
    }

    /// Apply a user edit to the current person, under the local file lock.
    ///
    /// # Errors
    /// Revoked reader, stale revision, invalid entry, or persistence failure.
    pub fn edit(&self, reader: &Reader, revision: u64, edit: Edit) -> io::Result<Snapshot> {
        let result = self.change(reader, revision, |doc| {
            match edit {
                Edit::Add(word) => insert(&mut doc.words, validate(&word)?)?,
                Edit::Replace { old, new } => {
                    let new = validate(&new)?;
                    let index = doc
                        .words
                        .iter()
                        .position(|w| w.same_key(&old))
                        .ok_or_else(|| invalid("the word no longer exists"))?;
                    if doc
                        .words
                        .iter()
                        .enumerate()
                        .any(|(i, w)| i != index && w.same_key(&new))
                    {
                        return Err(invalid("the edited word already exists"));
                    }
                    if let Some(word) = doc.words.get_mut(index) {
                        *word = new;
                    }
                }
                Edit::Remove(old) => doc.words.retain(|w| !w.same_key(&old)),
                Edit::Reset => doc.words.clear(),
            }
            Ok(())
        });
        if result.is_ok() {
            self.0.changes.ring();
        }
        result
    }

    fn change(
        &self,
        reader: &Reader,
        revision: u64,
        change: impl FnOnce(&mut Document) -> io::Result<()>,
    ) -> io::Result<Snapshot> {
        if !Arc::ptr_eq(&self.0, &reader.inner) {
            return Err(denied());
        }
        let binding = reader.inner.binding.lock_recover();
        let owner = reader.owner(&binding)?;
        let path = self.0.path(owner);
        create_private_directory(&self.0.root)?;
        let _lock = advisory_lock::FileLock::open(
            &path.with_extension("lock"),
            advisory_lock::LockMode::Exclusive,
        )?;
        let mut doc = read(&path, owner)?;
        if doc.revision != revision {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "dictionary changed; reload before editing",
            ));
        }
        change(&mut doc)?;
        doc.revision = doc
            .revision
            .checked_add(1)
            .ok_or_else(|| invalid("dictionary revision exhausted"))?;
        let bytes = serde_json::to_vec(&doc).map_err(io::Error::other)?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(invalid("dictionary is too large"));
        }
        tilo_files::native::atomic::write_private(&path, &bytes)?;
        let snapshot = doc.snapshot();
        // Keep the binding locked through publication: logout must not race a
        // checked edit and allow it to write after account authority is revoked.
        drop(binding);
        Ok(snapshot)
    }
}

impl Inner {
    fn path(&self, owner: [u8; 32]) -> PathBuf {
        self.root.join(format!("{}.json", hex::encode(owner)))
    }
}

impl Reader {
    /// Compare opaque binding identity without reading dictionary contents.
    /// A same-person logout/login still issues a different binding epoch.
    pub fn same_binding(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner) && self.epoch == other.epoch
    }

    fn owner(&self, binding: &Binding) -> io::Result<[u8; 32]> {
        if binding.epoch != self.epoch {
            return Err(denied());
        }
        binding.owner.ok_or_else(denied)
    }
}

impl Vocabulary for Reader {
    fn snapshot(&self) -> io::Result<Snapshot> {
        let binding = self.inner.binding.lock_recover();
        let owner = self.owner(&binding)?;
        // Atomic replacement means readers see one whole committed document.
        // Always reload; a mtime-only cache misses rapid edits and removals.
        let snapshot = read(&self.inner.path(owner), owner)?.snapshot();
        drop(binding);
        Ok(snapshot)
    }
}

fn validate(word: &Word) -> io::Result<Word> {
    Word::new(&word.spelling, word.language.as_deref())
}

fn insert(words: &mut Vec<Word>, word: Word) -> io::Result<()> {
    if words.iter().any(|w| w.same_key(&word)) {
        return Ok(());
    }
    if words.len() == MAX_WORDS {
        return Err(invalid("personal dictionary is full"));
    }
    words.push(word);
    Ok(())
}

fn read(path: &Path, owner: [u8; 32]) -> io::Result<Document> {
    let Some(bytes) = tilo_files::native::bounded::read(path, MAX_FILE_BYTES)? else {
        return Ok(Document::empty(owner));
    };
    let doc: Document = serde_json::from_slice(&bytes).map_err(|e| invalid(&e.to_string()))?;
    if doc.version != VERSION || doc.owner != owner || doc.words.len() > MAX_WORDS {
        return Err(invalid("invalid dictionary version, owner or size"));
    }
    let mut validated = Vec::new();
    for word in &doc.words {
        let valid = validate(word)?;
        if &valid != word || validated.iter().any(|w: &Word| w.same_key(word)) {
            return Err(invalid(
                "dictionary contains noncanonical or duplicate entries",
            ));
        }
        validated.push(valid);
    }
    Ok(doc)
}

fn create_private_directory(root: &Path) -> io::Result<()> {
    tilo_files::native::atomic::create_private_directory(root)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "personal dictionary binding was revoked",
    )
}

#[cfg(test)]
mod tests;
