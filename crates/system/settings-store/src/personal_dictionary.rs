//! The person's personal vocabulary, shared by every input surface that checks or helps with
//! spelling: the word processor, the on-screen keyboard and dictation.
//!
//! The words are a section of the person's settings document ([`SECTION`]) beside their other
//! settings, so they follow them to their other devices. A consumer reads through a [`Reader`]
//! bound to one document; a reader is revoked when the session replaces that document.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use unicode_normalization::UnicodeNormalization;

use crate::{ConditionalUpdate, SettingsSource};

pub mod delivery;

/// The person's words, as a section of their settings document.
pub const SECTION: &str = "personal_dictionary";

/// The section's one leaf: the words and the revision an edit names. One value, so a device's
/// change reaches the person's other devices whole.
const WORDS: &str = "words";

const MAX_WORDS: usize = 1_024;

/// An explicit user addition. Display spelling is retained, including casing.
/// `None` means all languages; a tag such as `fr` restricts the hint to that
/// language and its regional variants. Additions never mean replacement rules.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Word {
    pub spelling: String,
    pub language: Option<String>,
}

impl Word {
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub revision: u64,
    pub words: Vec<Word>,
}

impl Snapshot {
    /// The words `edit` makes of these, with the revision after it.
    pub fn preview(&self, edit: Edit) -> io::Result<Self> {
        let mut next = self.clone();
        match edit {
            Edit::Add(word) => insert(&mut next.words, validate(&word)?)?,
            Edit::Replace { old, new } => {
                let new = validate(&new)?;
                let index = next
                    .words
                    .iter()
                    .position(|word| word.same_key(&old))
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
            Edit::Remove(old) => next.words.retain(|word| !word.same_key(&old)),
            Edit::Reset => next.words.clear(),
        }
        next.revision = next
            .revision
            .checked_add(1)
            .ok_or_else(|| invalid("dictionary revision exhausted"))?;
        Ok(next)
    }
    pub fn hints<'a>(&'a self, language: &'a str) -> impl Iterator<Item = &'a str> {
        self.words
            .iter()
            .filter(move |w| w.applies(language))
            .map(|w| w.spelling.as_str())
    }

    pub fn contains(&self, typed: &str, language: &str) -> bool {
        let key = folded(typed);
        self.hints(language).any(|hint| folded(hint) == key)
    }

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

pub trait Vocabulary: Send + Sync {
    fn snapshot(&self) -> io::Result<Snapshot>;
}

#[derive(Clone, Serialize, Deserialize)]
pub enum Edit {
    Add(Word),
    Replace { old: Word, new: Word },
    Remove(Word),
    Reset,
}

/// The person's dictionary as `source` holds it. A missing or unreadable section is empty: a
/// settings read never fails.
#[must_use]
pub fn snapshot_in(source: &dyn SettingsSource) -> Snapshot {
    snapshot_of(&source.load())
}

/// [`snapshot_in`] of a document already loaded.
#[must_use]
pub fn snapshot_of(document: &Map<String, Value>) -> Snapshot {
    document
        .get(SECTION)
        .and_then(Value::as_object)
        .and_then(|section| section.get(WORDS))
        .cloned()
        .and_then(|value| serde_json::from_value::<Snapshot>(value).ok())
        .filter(|snapshot| {
            snapshot.words.len() <= MAX_WORDS
                && snapshot.words.iter().all(|word| {
                    Word::new(&word.spelling, word.language.as_deref())
                        .is_ok_and(|valid| valid == *word)
                })
        })
        .unwrap_or_default()
}

/// Apply `edit` to the dictionary `revision` was read from.
///
/// # Errors
///
/// [`io::ErrorKind::WouldBlock`] when another edit landed since `revision`; [`io::ErrorKind::InvalidData`]
/// when the edit is not a valid change to the words; otherwise the source's own error.
pub fn edit_in(source: &dyn SettingsSource, revision: u64, edit: Edit) -> io::Result<Snapshot> {
    let document = source.load();
    let current = snapshot_of(&document);
    if current.revision != revision {
        return Err(changed());
    }
    let next = current.preview(edit)?;
    if serde_json::to_vec(&next.words)
        .map_err(io::Error::other)?
        .len()
        > crate::placement::MAX_PERSONAL_VALUE_BYTES
    {
        return Err(invalid("personal dictionary is full"));
    }
    let mut values = Map::new();
    values.insert(
        WORDS.to_owned(),
        serde_json::to_value(&next).map_err(io::Error::other)?,
    );
    let expected = match document.get(SECTION) {
        Some(Value::Object(section)) => section.clone(),
        _ => Map::new(),
    };
    match source.compare_update_section(SECTION, &expected, values)? {
        ConditionalUpdate::Stored => Ok(next),
        ConditionalUpdate::Superseded => Err(changed()),
    }
}

/// One document's view of the person's dictionary. Revoking it makes every clone fail, as the
/// session does when it replaces the document a recognizer was reading.
#[derive(Clone)]
pub struct Reader {
    source: Arc<dyn SettingsSource>,
    revoked: Arc<AtomicBool>,
}

impl Reader {
    #[must_use]
    pub fn bind(source: Arc<dyn SettingsSource>) -> Self {
        Self {
            source,
            revoked: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
    }

    /// Whether `other` reads the same document: two readers of it stay interchangeable while the
    /// words change.
    #[must_use]
    pub fn same_binding(&self, other: &Self) -> bool {
        self.reads(&other.source)
    }

    /// Whether this reader reads `source`.
    #[must_use]
    pub fn reads(&self, source: &Arc<dyn SettingsSource>) -> bool {
        Arc::ptr_eq(&self.source, source)
    }
}

impl Vocabulary for Reader {
    fn snapshot(&self) -> io::Result<Snapshot> {
        if self.revoked.load(Ordering::Acquire) {
            return Err(denied());
        }
        Ok(snapshot_in(self.source.as_ref()))
    }
}

fn validate(word: &Word) -> io::Result<Word> {
    Word::new(&word.spelling, word.language.as_deref())
}

fn insert(words: &mut Vec<Word>, word: Word) -> io::Result<()> {
    if words.iter().any(|held| held.same_key(&word)) {
        return Ok(());
    }
    if words.len() == MAX_WORDS {
        return Err(invalid("personal dictionary is full"));
    }
    words.push(word);
    Ok(())
}

fn changed() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "dictionary changed; reload before editing",
    )
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "personal dictionary reader was revoked",
    )
}

#[cfg(test)]
mod tests;
