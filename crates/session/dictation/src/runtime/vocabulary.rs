//! Private recognition hints from the shared, revocable personal service.
//! No store, account discovery, rewrite rules or diagnostic text lives here.
use std::io;
use std::sync::Arc;

use settings_store::personal_dictionary::{Snapshot, Vocabulary, Word};
use zeroize::{Zeroize as _, Zeroizing};

const MAX_HINTS: usize = 256;
const MAX_HINT_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseError {
    Changed,
    Unavailable,
}

/// One actor's vocabulary binding. Read only on the actor thread, never while
/// drawing or handling compositor input. Errors retire all cached hints.
/// The host must also cancel the active dictation when replacing its reader.
pub struct VocabularyLease {
    reader: Arc<dyn Vocabulary>,
    language: Zeroizing<String>,
    fingerprint: Zeroizing<[u8; 32]>,
    revision: u64,
    hints: Zeroizing<Vec<String>>,
    live: bool,
}

impl VocabularyLease {
    /// Capture accepted casing and the owner's exact language applicability.
    /// Automatic language uses only entries without a language restriction.
    /// # Errors
    /// Revoked/unavailable binding or malformed/oversized hint data.
    pub fn capture(reader: Arc<dyn Vocabulary>, language: &str) -> io::Result<Self> {
        let mut snapshot = reader.snapshot()?;
        let result = (|| {
            let fingerprint = fingerprint(&snapshot)?;
            let applicable: Vec<&str> = if language == "auto" {
                snapshot
                    .words
                    .iter()
                    .filter(|word| word.language.is_none())
                    .map(|word| word.spelling.as_str())
                    .collect()
            } else {
                snapshot.hints(language).collect()
            };
            let mut hints = Zeroizing::new(Vec::new());
            let mut bytes = 0usize;
            for spelling in applicable {
                let Some(next) = bytes
                    .checked_add(spelling.len())
                    .and_then(|value| value.checked_add(1))
                else {
                    return Err(invalid());
                };
                if hints.len() == MAX_HINTS || next > MAX_HINT_BYTES {
                    break;
                }
                hints.push(spelling.to_owned());
                bytes = next;
            }
            Ok(Self {
                reader,
                language: Zeroizing::new(language.to_owned()),
                fingerprint: Zeroizing::new(fingerprint),
                revision: snapshot.revision,
                hints,
                live: true,
            })
        })();
        wipe(&mut snapshot);
        result
    }

    /// Decoder vocabulary data only; callers must not interpret it as commands
    /// or replace substrings in a transcript. Native token budgeting is separate.
    pub fn hints(&self) -> &[String] {
        &self.hints
    }

    /// Recheck before each semantic input operation and before delivering words.
    /// Any edit, binding failure or revocation permanently retires this lease.
    /// # Errors
    /// A retired, changed, unavailable or revoked dictionary.
    pub fn check(&mut self) -> Result<(), LeaseError> {
        let result = if self.live {
            self.reader
                .snapshot()
                .map_err(|_| LeaseError::Unavailable)
                .and_then(|mut snapshot| {
                    let matches = fingerprint(&snapshot)
                        .map(|got| snapshot.revision == self.revision && got == *self.fingerprint);
                    wipe(&mut snapshot);
                    if matches.map_err(|_| LeaseError::Unavailable)? {
                        Ok(())
                    } else {
                        Err(LeaseError::Changed)
                    }
                })
        } else {
            Err(LeaseError::Unavailable)
        };
        if result.is_err() {
            self.live = false;
            self.hints.zeroize();
            self.fingerprint.zeroize();
            self.language.zeroize();
        }
        result
    }
}

fn fingerprint(snapshot: &Snapshot) -> io::Result<[u8; 32]> {
    if snapshot.words.len() > 1024 {
        return Err(invalid());
    }
    let mut hash = blake3::Hasher::new();
    hash.update(&snapshot.revision.to_le_bytes());
    for word in &snapshot.words {
        let mut validated = Word::new(&word.spelling, word.language.as_deref())?;
        let valid =
            validated == *word && !word.spelling.contains("<|") && !word.spelling.contains("|>");
        validated.spelling.zeroize();
        if let Some(language) = validated.language.as_mut() {
            language.zeroize();
        }
        if !valid {
            return Err(invalid());
        }
        hash.update(&(word.spelling.len() as u64).to_le_bytes());
        hash.update(word.spelling.as_bytes());
        match word.language.as_deref() {
            Some(language) => {
                hash.update(&[1]);
                hash.update(&(language.len() as u64).to_le_bytes());
                hash.update(language.as_bytes());
            }
            None => {
                hash.update(&[0]);
            }
        }
    }
    Ok(*hash.finalize().as_bytes())
}

fn wipe(snapshot: &mut Snapshot) {
    for word in &mut snapshot.words {
        word.spelling.zeroize();
        if let Some(language) = word.language.as_mut() {
            language.zeroize();
        }
    }
    snapshot.words.clear();
}

fn invalid() -> io::Error {
    io::Error::other("personal vocabulary unavailable or changed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use settings_store::personal_dictionary::{Edit, Service};

    #[test]
    fn interrupted_reader_failure_is_not_an_ordinary_dictionary_edit() {
        struct Reader {
            inner: settings_store::personal_dictionary::Reader,
            failed: std::sync::atomic::AtomicBool,
        }
        impl Vocabulary for Reader {
            fn snapshot(&self) -> io::Result<Snapshot> {
                if self.failed.load(std::sync::atomic::Ordering::Acquire) {
                    Err(io::ErrorKind::Interrupted.into())
                } else {
                    self.inner.snapshot()
                }
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let service = Service::new(directory.path().join("dictionary")).unwrap();
        let inner = service.bind(Some([1; 32])).unwrap();
        service
            .edit(&inner, 0, Edit::Add(Word::new("TiLo", None).unwrap()))
            .unwrap();
        let reader = Arc::new(Reader {
            inner,
            failed: false.into(),
        });
        let mut lease = VocabularyLease::capture(reader.clone(), "en").unwrap();
        assert_eq!(lease.hints(), ["TiLo"]);
        reader
            .failed
            .store(true, std::sync::atomic::Ordering::Release);
        assert_eq!(lease.check(), Err(LeaseError::Unavailable));
        assert!(lease.hints().is_empty());
    }

    #[test]
    fn shared_service_casing_language_removal_and_binding_revocation_retire_hints() {
        let directory = tempfile::tempdir().unwrap();
        let service = Service::new(directory.path().join("dictionary")).unwrap();
        let reader = service.bind(Some([1; 32])).unwrap();
        let mut snapshot = reader.snapshot().unwrap();
        for word in [
            Word::new("TiLo", None).unwrap(),
            Word::new("Élodie", Some("fr")).unwrap(),
            Word::new("Québec", Some("fr-CA")).unwrap(),
        ] {
            snapshot = service
                .edit(&reader, snapshot.revision, Edit::Add(word))
                .unwrap();
        }
        let mut lease = VocabularyLease::capture(Arc::new(reader.clone()), "fr").unwrap();
        assert_eq!(lease.hints(), ["TiLo", "Élodie"]);
        assert_eq!(
            VocabularyLease::capture(Arc::new(reader.clone()), "auto")
                .unwrap()
                .hints(),
            ["TiLo"]
        );
        lease.check().unwrap();
        service
            .edit(
                &reader,
                snapshot.revision,
                Edit::Remove(Word::new("TiLo", None).unwrap()),
            )
            .unwrap();
        assert!(lease.check().is_err());
        assert!(lease.hints().is_empty());
        assert!(lease.check().is_err(), "a changed lease cannot revive");
        let mut replacement = VocabularyLease::capture(Arc::new(reader), "fr").unwrap();
        service.bind(None);
        service.bind(Some([1; 32]));
        assert!(
            replacement.check().is_err(),
            "switching back cannot revive the old binding"
        );
        assert!(replacement.hints().is_empty());
    }
}
