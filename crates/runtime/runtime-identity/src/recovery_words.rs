//! The 24-word recovery key of a single-device identity.
//!
//! A single-device identity's whole secret is the 32-byte Ed25519 seed its
//! record (or its secret provider) keeps: the public key, share index and
//! threshold all follow from it. The recovery key is that seed written as
//! 24 BIP39 words (256 bits and an 8-bit checksum), so the words alone
//! restore the identity and nothing else has to be written down.
//!
//! Like portable backups, this runs inside the crate that owns the secret
//! provider: the words leave it only as a [`Zeroizing`] string for the
//! person to copy, and restoring hands the seed straight to the provider.
//! Every error describes the phrase by word count or position, never by a
//! word, so none of it reaches a log or an error dialog.

use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};

use ed25519_dalek::SigningKey;
use threshold_identity::bip39::{self, MnemonicError};
use zeroize::Zeroizing;

use super::{IdentityLock, SecretProvider, identity_from_record, ondisk};

/// Words in a recovery key: 256 bits of seed and 8 of checksum, 11 a word.
pub const RECOVERY_WORD_COUNT: usize = 24;

/// The avatar colour an identity gets when nothing chose one. Minting uses
/// it, and so does restoring from words, which carry no presentation.
pub const DEFAULT_AVATAR_COLOR: [u8; 3] = [128, 128, 128];

/// Why words could not be produced or restored, phrased for the person.
///
/// Recovery-key operations report these inside an [`io::Error`] (kind
/// [`io::ErrorKind::InvalidInput`]); [`recovery_words_error`] recovers the
/// variant so a caller can react to it rather than parse a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryWordsError {
    /// Not [`RECOVERY_WORD_COUNT`] words.
    WordCount { found: usize },
    /// The word at this position (from 1) is not on the BIP39 list.
    UnknownWord { position: usize },
    /// Every word is on the list but they do not carry their checksum: one
    /// is mistyped as another listed word, or two are swapped.
    Checksum,
    /// The words restore a valid identity, but not the one this account
    /// already has.
    DifferentIdentity {
        /// This account's identity ([`recovery_expected_identity`]).
        expected: [u8; 32],
        /// The identity the words restore.
        found: [u8; 32],
    },
    /// This device keeps no secret of the identity (it joined from another
    /// device), so it has no recovery key to show.
    NoSecretOnThisDevice,
    /// A threshold identity has no single secret; its shares recover it.
    ThresholdIdentity,
}

impl std::fmt::Display for RecoveryWordsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WordCount { found } => write!(
                f,
                "A recovery key has {RECOVERY_WORD_COUNT} words; this has {found}."
            ),
            Self::UnknownWord { position } => write!(
                f,
                "Word {position} is not a recovery-key word. Check its spelling."
            ),
            Self::Checksum => f.write_str(
                "These words don't form a recovery key. One is probably mistyped or out of order.",
            ),
            Self::DifferentIdentity { .. } => {
                f.write_str("These words belong to a different identity than this computer's.")
            }
            Self::NoSecretOnThisDevice => f.write_str(
                "This computer doesn't keep this identity's secret. Show the recovery key on the device that does.",
            ),
            Self::ThresholdIdentity => {
                f.write_str("This identity is shared across devices and has no single recovery key.")
            }
        }
    }
}

impl std::error::Error for RecoveryWordsError {}

impl From<RecoveryWordsError> for io::Error {
    fn from(error: RecoveryWordsError) -> Self {
        Self::new(io::ErrorKind::InvalidInput, error)
    }
}

impl From<MnemonicError> for RecoveryWordsError {
    fn from(error: MnemonicError) -> Self {
        match error {
            MnemonicError::WordCount(found) => Self::WordCount { found },
            MnemonicError::UnknownWord(position) => Self::UnknownWord { position },
            MnemonicError::Checksum => Self::Checksum,
        }
    }
}

/// The [`RecoveryWordsError`] an operation of this module failed with, if
/// that is why it failed (as opposed to, say, an unreadable record).
#[must_use]
pub fn recovery_words_error(error: &io::Error) -> Option<RecoveryWordsError> {
    error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<RecoveryWordsError>())
        .copied()
}

/// The 24-word recovery key of the single-device identity at
/// `identity_path`, read from its record or its secret provider.
///
/// # Errors
///
/// [`RecoveryWordsError::NoSecretOnThisDevice`] for a joined device's public
/// record, [`RecoveryWordsError::ThresholdIdentity`] for a threshold share,
/// otherwise the record or provider failure (a locked Secret Service among
/// them, see [`super::is_secret_service_unavailable_io`]).
pub fn recovery_words(identity_path: impl AsRef<Path>) -> io::Result<Zeroizing<String>> {
    let seed = single_device_seed(identity_path.as_ref())?;
    bip39::encode(seed.as_slice())
        .ok_or_else(|| io::Error::other("a 32-byte seed did not encode as words"))
}

/// Check that `phrase` is a well-formed recovery key, without restoring it.
/// Input is read leniently (see [`recovery_words_public_key`]).
///
/// # Errors
///
/// The first problem: [`RecoveryWordsError::WordCount`],
/// [`RecoveryWordsError::UnknownWord`] or [`RecoveryWordsError::Checksum`].
pub fn check_recovery_words(phrase: &str) -> Result<(), RecoveryWordsError> {
    decode_seed(phrase).map(drop)
}

/// How far a person has got typing a recovery key, for feedback while they
/// type: a word still being typed is not flagged until it is finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoveryWordsProgress {
    /// Words entered so far, read as [`recovery_words_public_key`] reads them.
    pub words: usize,
    /// The 1-based position of the first finished word that is not a
    /// recovery-key word.
    pub unknown_word: Option<usize>,
}

/// Progress through typing `phrase`. The last word counts as finished only
/// once something other than a letter follows it.
#[must_use]
pub fn recovery_words_progress(phrase: &str) -> RecoveryWordsProgress {
    let normalized = normalize(phrase);
    let words: Vec<&str> = normalized.split_whitespace().collect();
    let finished = if phrase.chars().last().is_some_and(char::is_alphabetic) {
        words.len().saturating_sub(1)
    } else {
        words.len()
    };
    let unknown_word = words
        .iter()
        .take(finished)
        .position(|word| !bip39::is_word(word))
        .map(|at| at.saturating_add(1));
    RecoveryWordsProgress {
        words: words.len(),
        unknown_word,
    }
}

/// Where in `phrase` the finished words that are not recovery-key words
/// are, as byte ranges, to mark each one where it was typed. Words are read
/// as [`recovery_words_public_key`] reads them; the last word is left alone
/// until something other than a letter follows it.
#[must_use]
pub fn unknown_recovery_words(phrase: &str) -> Vec<Range<usize>> {
    let mut unknown = Vec::new();
    let mut start = None;
    for (at, character) in phrase.char_indices().chain([(phrase.len(), ' ')]) {
        match (character.is_alphabetic(), start) {
            (true, None) => start = Some(at),
            (false, Some(from)) => {
                start = None;
                let finished = at < phrase.len();
                let word = Zeroizing::new(phrase.get(from..at).unwrap_or_default().to_lowercase());
                if finished && !bip39::is_word(&word) {
                    unknown.push(from..at);
                }
            }
            _ => {}
        }
    }
    unknown
}

/// The recovery-key word a mistyped `typed` most likely meant, in any case,
/// to suggest it; `None` when no single word stands out.
#[must_use]
pub fn closest_recovery_word(typed: &str) -> Option<&'static str> {
    bip39::closest_word(&Zeroizing::new(typed.to_lowercase()))
}

/// The identity (its public key; hex of it is the fingerprint) `phrase`
/// restores, so a person can confirm it before restoring.
///
/// Words are read as people copy them: any case, separated by spaces,
/// newlines, commas or other punctuation, and numbering such as
/// `1. abandon 2. ability` is skipped.
///
/// # Errors
///
/// As [`check_recovery_words`].
pub fn recovery_words_public_key(phrase: &str) -> Result<[u8; 32], RecoveryWordsError> {
    let seed = decode_seed(phrase)?;
    Ok(SigningKey::from_bytes(&seed).verifying_key().to_bytes())
}

/// This account's identity: the active record's public key, if there is
/// one. Words for any other identity are refused by
/// [`import_recovery_words`], so a person restoring a joined device, or one
/// whose secret store lost its key, cannot end up with a second identity by
/// mistake. Reads public fields only; never contacts a secret provider.
///
/// # Errors
///
/// Returns an error if `identity_dir` is not a real directory owned by this
/// user or cannot be read.
pub fn recovery_expected_identity(identity_dir: &Path) -> io::Result<Option<[u8; 32]>> {
    Ok(super::list_identities(identity_dir)?
        .into_iter()
        .find(|summary| summary.active)
        .map(|summary| summary.public_key))
}

/// Restore the identity `phrase` carries into `identity_dir` as `id`, its
/// seed moved into the preferred secret provider.
///
/// When the directory already has a record of this identity, that record is
/// completed instead: a joined device's public record, or one whose secret
/// the provider lost, keeps its id and presentation and gains the secret
/// again. A record that already holds it is returned unchanged. A new record
/// gets `display_name` (trimmed; empty, like an identity minted before the
/// person named it) and [`DEFAULT_AVATAR_COLOR`].
///
/// # Errors
///
/// A [`RecoveryWordsError`] for malformed words or words of an identity
/// other than [`recovery_expected_identity`]; `InvalidInput` for a display
/// name over 1024 bytes or with control characters; otherwise the lifecycle
/// or provider failure.
pub fn import_recovery_words(
    identity_dir: &Path,
    id: &str,
    phrase: &str,
    display_name: &str,
) -> io::Result<PathBuf> {
    import_inner(identity_dir, id, phrase, display_name, None)
}

/// [`import_recovery_words`] through a provider chosen by trusted host code.
///
/// # Errors
///
/// As [`import_recovery_words`].
pub fn import_recovery_words_with_provider(
    identity_dir: &Path,
    id: &str,
    phrase: &str,
    display_name: &str,
    provider: &dyn SecretProvider,
) -> io::Result<PathBuf> {
    import_inner(identity_dir, id, phrase, display_name, Some(provider))
}

fn import_inner(
    dir: &Path,
    id: &str,
    phrase: &str,
    display_name: &str,
    provider: Option<&dyn SecretProvider>,
) -> io::Result<PathBuf> {
    let seed = decode_seed(phrase)?;
    let public = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
    let display_name = display_name.trim();
    if display_name.len() > 1024 || display_name.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "An identity needs a bounded display name without control characters.",
        ));
    }

    let _lock = IdentityLock::acquire(dir)?;
    let present = super::list_identities_unlocked(dir)?;
    if let Some(active) = present.iter().find(|summary| summary.active)
        && active.public_key != public
    {
        return Err(RecoveryWordsError::DifferentIdentity {
            expected: active.public_key,
            found: public,
        }
        .into());
    }

    let Some(existing) = present.iter().find(|summary| summary.public_key == public) else {
        let record = full_record(
            ondisk::IdentityRecord {
                profile_photo: None,
                display_name: display_name.to_owned(),
                avatar_color: DEFAULT_AVATAR_COLOR,
                threshold_identity: single_device_identity(public, Zeroizing::default()),
                created_at: String::new(),
                schema_version: 1,
                secret_ref: None,
            },
            &seed,
        )?;
        let imported = super::load_from_bytes(&record)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        return super::import_identity_unlocked(dir, id, &record, &imported, provider);
    };

    let path = super::identity_path(dir, &existing.id)?;
    let current = super::read_record(&path)?;
    if current.schema_version == 1 && !current.threshold_identity.secret_bytes.is_empty() {
        // It already holds its key inline; securing it is all that is left.
        secure(dir, &path, provider)?;
        return Ok(path);
    }
    if provider_holds_secret(dir, &current) {
        // Nothing was lost; rewriting would only put the key on disk again.
        return Ok(path);
    }
    let previous = Zeroizing::new(super::read_record_bytes(&path)?);
    let display_name = if super::is_chosen_display_name(&current.display_name) {
        current.display_name.clone()
    } else {
        display_name.to_owned()
    };
    let record = full_record(
        ondisk::IdentityRecord {
            profile_photo: current.profile_photo,
            display_name,
            avatar_color: current.avatar_color,
            threshold_identity: single_device_identity(public, Zeroizing::default()),
            created_at: current.created_at,
            schema_version: 1,
            secret_ref: None,
        },
        &seed,
    )?;
    super::write_private_atomic(&path, &record)?;
    if let Err(error) = secure(dir, &path, provider) {
        // Securing fails before publishing new metadata unless it says
        // otherwise; then the file still holds the complete inline record,
        // so put back the one the person had rather than leave a plaintext
        // key behind.
        if matches!(
            tilo_files::native::atomic::publication_state(&error),
            None | Some(tilo_files::native::atomic::PublicationState::NotPublished)
        ) {
            let _ = super::write_private_atomic(&path, &previous);
        }
        return Err(error);
    }
    Ok(path)
}

fn secure(dir: &Path, path: &Path, provider: Option<&dyn SecretProvider>) -> io::Result<()> {
    match provider {
        Some(provider) => super::secure_record_with_provider_unlocked(dir, path, provider),
        None => super::secure_record_unlocked(dir, path),
    }
    .map(drop)
}

/// Whether `record` is secured and its provider still returns a secret that
/// matches it.
fn provider_holds_secret(dir: &Path, record: &ondisk::IdentityRecord) -> bool {
    let Some(reference) = record.secret_ref.as_ref() else {
        return false;
    };
    super::secure_store::provider_for(dir, reference)
        .and_then(|provider| provider.load(&reference.locator))
        .is_ok_and(|secret| {
            identity_from_record(
                super::backup::record_for_validation(record),
                Some(secret.as_slice()),
            )
            .is_ok()
        })
}

fn single_device_identity(
    group_public_key: [u8; 32],
    secret_bytes: Zeroizing<Vec<u8>>,
) -> ondisk::SerializableIdentity {
    ondisk::SerializableIdentity {
        single_device: true,
        group_public_key,
        threshold: 1,
        total_shares: 1,
        share_index: 1,
        secret_bytes,
    }
}

/// `record` with `seed` inline, encoded as a schema-1 record: the form
/// [`super::import_identity_unlocked`] validates and then secures.
fn full_record(
    mut record: ondisk::IdentityRecord,
    seed: &Zeroizing<[u8; 32]>,
) -> io::Result<Zeroizing<Vec<u8>>> {
    record.threshold_identity.secret_bytes = Zeroizing::new(seed.to_vec());
    // Sized for the record so encoding never reallocates and leaves an
    // unwiped partial copy of the key behind; a photo is the only large part.
    let photo = record
        .profile_photo
        .as_ref()
        .map_or(0, |photo| photo.rgba().len().saturating_mul(4));
    let mut out = Zeroizing::new(Vec::with_capacity(photo.saturating_add(4096)));
    serde_json::to_writer(&mut *out, &record)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(out)
}

/// The seed of the single-device identity at `path`. Holds the lifecycle
/// lock so removal or a provider move cannot race the provider lookup.
fn single_device_seed(path: &Path) -> io::Result<Zeroizing<[u8; 32]>> {
    let dir = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "identity path has no parent directory",
        )
    })?;
    let _lock = IdentityLock::acquire(dir)?;
    let record = super::read_record(path)?;
    if super::is_public_member_projection(&record) {
        return Err(RecoveryWordsError::NoSecretOnThisDevice.into());
    }
    if !record.threshold_identity.single_device {
        return Err(RecoveryWordsError::ThresholdIdentity.into());
    }
    let secret = match record.schema_version {
        1 => Zeroizing::new(record.threshold_identity.secret_bytes.to_vec()),
        2 => {
            let reference = record.secret_ref.as_ref().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "secured identity has no secret reference",
                )
            })?;
            super::secure_store::provider_for(dir, reference)?.load(&reference.locator)?
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "identity has an unsupported schema version",
            ));
        }
    };
    if secret.is_empty() {
        return Err(RecoveryWordsError::NoSecretOnThisDevice.into());
    }
    // The record's public key must follow from this secret, or the words
    // would restore an identity nobody verifies against.
    let identity = identity_from_record(
        super::backup::record_for_validation(&record),
        Some(secret.as_slice()),
    )
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    if !identity.is_single_device() {
        return Err(RecoveryWordsError::ThresholdIdentity.into());
    }
    let mut seed = Zeroizing::new([0_u8; 32]);
    if secret.len() != seed.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "single-device secret is not 32 bytes",
        ));
    }
    seed.copy_from_slice(&secret);
    Ok(seed)
}

/// The seed `phrase` encodes, read leniently: letters are lowercased and
/// every run of anything else (digits, punctuation, whitespace) separates
/// words, which drops list numbering and commas.
fn decode_seed(phrase: &str) -> Result<Zeroizing<[u8; 32]>, RecoveryWordsError> {
    let normalized = normalize(phrase);
    let found = normalized.split_whitespace().count();
    if found != RECOVERY_WORD_COUNT {
        return Err(RecoveryWordsError::WordCount { found });
    }
    let entropy = bip39::decode(&normalized)?;
    let mut seed = Zeroizing::new([0_u8; 32]);
    if entropy.len() != seed.len() {
        return Err(RecoveryWordsError::WordCount { found });
    }
    seed.copy_from_slice(&entropy);
    Ok(seed)
}

fn normalize(phrase: &str) -> Zeroizing<String> {
    let mut normalized = Zeroizing::new(String::with_capacity(phrase.len().saturating_mul(2)));
    for character in phrase.chars() {
        if character.is_alphabetic() {
            normalized.extend(character.to_lowercase());
        } else {
            normalized.push(' ');
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn secured_identity(dir: &Path, id: &str, seed: u8) -> PathBuf {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let bytes = serde_json::to_vec(&serde_json::json!({
            "display_name": "Ada",
            "avatar_color": [10, 20, 30],
            "threshold_identity": {
                "single_device": true,
                "group_public_key": key.verifying_key().to_bytes(),
                "threshold": 1_u16,
                "total_shares": 1_u16,
                "share_index": 1_u16,
                "secret_bytes": vec![seed; 32],
            },
            "created_at": "2026-09-01T00:00:00Z",
            "schema_version": 1,
        }))
        .unwrap();
        let provider = super::super::BrokerFileProvider::new(dir);
        super::super::import_identity_with_provider(dir, id, &bytes, &provider).unwrap();
        dir.join(format!("{id}.json"))
    }

    fn words_of(seed: u8) -> Zeroizing<String> {
        bip39::encode(&[seed; 32]).unwrap()
    }

    #[test]
    fn unknown_words_are_found_where_they_were_typed() {
        assert_eq!(
            unknown_recovery_words("1. Abandon 2. abilty\nabout abov"),
            vec![14..20],
            "a word still being typed is left alone"
        );
        assert_eq!(unknown_recovery_words("zebra, ZEBRAA. "), vec![7..13]);
        assert!(unknown_recovery_words("").is_empty());
    }

    #[test]
    fn progress_flags_only_finished_unknown_words() {
        assert_eq!(
            recovery_words_progress("1. abandon 2. abil"),
            RecoveryWordsProgress {
                words: 2,
                unknown_word: None
            },
            "a word still being typed is not flagged"
        );
        assert_eq!(
            recovery_words_progress("abandon abil "),
            RecoveryWordsProgress {
                words: 2,
                unknown_word: Some(2)
            }
        );
        assert_eq!(
            recovery_words_progress(&words_of(7)),
            RecoveryWordsProgress {
                words: RECOVERY_WORD_COUNT,
                unknown_word: None
            }
        );
    }

    #[test]
    fn words_round_trip_into_an_identity_that_signs_as_the_original() {
        let source = tempfile::tempdir().unwrap();
        let path = secured_identity(source.path(), "personal", 41);
        let original = super::super::load_from_path(path.clone()).unwrap();
        let words = recovery_words(&path).unwrap();
        assert_eq!(words.split(' ').count(), RECOVERY_WORD_COUNT);
        assert_eq!(
            recovery_words_public_key(&words).unwrap(),
            original.group_public_key()
        );

        let target = tempfile::tempdir().unwrap();
        let provider = super::super::BrokerFileProvider::new(target.path());
        let restored = import_recovery_words_with_provider(
            target.path(),
            "restored",
            &words,
            " Ada ",
            &provider,
        )
        .unwrap();
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(&restored).unwrap()).unwrap();
        assert_eq!(
            record["schema_version"], 2,
            "the seed moved into the provider"
        );
        assert_eq!(record["display_name"], "Ada");
        assert!(record["threshold_identity"].get("secret_bytes").is_none());
        let identity = super::super::load_from_path(restored.clone()).unwrap();
        assert_eq!(identity.group_public_key(), original.group_public_key());
        assert_eq!(identity.avatar_color(), DEFAULT_AVATAR_COLOR);
        assert_eq!(
            identity.sign(b"recovered").unwrap(),
            original.sign(b"recovered").unwrap()
        );
        // Restoring again is a no-op on the record that now holds the key.
        assert_eq!(
            import_recovery_words_with_provider(target.path(), "again", &words, "", &provider)
                .unwrap(),
            restored
        );
        assert_eq!(recovery_words(&restored).unwrap(), words);
    }

    #[test]
    fn words_are_read_as_people_copy_them_and_errors_name_positions_only() {
        let words = words_of(42);
        let list: Vec<&str> = words.split(' ').collect();
        let numbered: String = list
            .iter()
            .enumerate()
            .map(|(at, word)| format!("{}. {}\n", at + 1, word.to_uppercase()))
            .collect::<Vec<_>>()
            .concat();
        let expected = recovery_words_public_key(&words).unwrap();
        for written in [
            numbered,
            list.join(", "),
            format!("  {}\t\n", list.join("   ")),
            list.iter()
                .enumerate()
                .map(|(at, word)| format!("{})\u{a0}{word}", at + 1))
                .collect::<Vec<_>>()
                .join(" "),
        ] {
            assert_eq!(
                recovery_words_public_key(&written),
                Ok(expected),
                "{written}"
            );
        }

        assert_eq!(
            check_recovery_words(&list[..23].join(" ")),
            Err(RecoveryWordsError::WordCount { found: 23 })
        );
        assert_eq!(
            check_recovery_words(&list[..12].join(" ")),
            Err(RecoveryWordsError::WordCount { found: 12 }),
            "twelve words, as a short BIP39 phrase has, are not a recovery key"
        );
        let mut unknown = list.clone();
        unknown[6] = "tilo";
        assert_eq!(
            check_recovery_words(&unknown.join(" ")),
            Err(RecoveryWordsError::UnknownWord { position: 7 })
        );
        let mut mistyped = list.clone();
        mistyped[23] = if list[23] == "abandon" {
            "ability"
        } else {
            "abandon"
        };
        let checksum = check_recovery_words(&mistyped.join(" ")).unwrap_err();
        assert_eq!(checksum, RecoveryWordsError::Checksum);

        // No error, in any rendering, repeats a word of the phrase.
        let errors = [
            RecoveryWordsError::WordCount { found: 23 },
            RecoveryWordsError::UnknownWord { position: 7 },
            checksum,
        ];
        for error in errors {
            let io_error = io::Error::from(error);
            assert_eq!(recovery_words_error(&io_error), Some(error));
            for rendered in [
                error.to_string(),
                format!("{error:?}"),
                io_error.to_string(),
                format!("{io_error:?}"),
            ] {
                for word in &list {
                    assert!(
                        !rendered
                            .split(|c: char| !c.is_alphabetic())
                            .any(|part| part.eq_ignore_ascii_case(word)),
                        "{rendered} names {word}"
                    );
                }
            }
        }
    }

    #[test]
    fn words_complete_this_accounts_identity_and_refuse_another() {
        let dir = tempfile::tempdir().unwrap();
        let key = SigningKey::from_bytes(&[43; 32]).verifying_key().to_bytes();
        let public = super::super::public_member_record("Ada", [1, 2, 3], key).unwrap();
        let provider = super::super::BrokerFileProvider::new(dir.path());
        super::super::import_identity_with_provider(dir.path(), "joined", &public, &provider)
            .unwrap();
        super::super::set_active_identity(dir.path(), "joined").unwrap();
        assert_eq!(recovery_expected_identity(dir.path()).unwrap(), Some(key));
        let joined = dir.path().join("joined.json");
        assert_eq!(
            recovery_words_error(&recovery_words(&joined).unwrap_err()),
            Some(RecoveryWordsError::NoSecretOnThisDevice)
        );

        let other = words_of(44);
        let refused =
            import_recovery_words_with_provider(dir.path(), "other", &other, "Eve", &provider)
                .unwrap_err();
        assert_eq!(
            recovery_words_error(&refused),
            Some(RecoveryWordsError::DifferentIdentity {
                expected: key,
                found: recovery_words_public_key(&other).unwrap(),
            })
        );
        assert!(!dir.path().join("other.json").exists());

        // The right words give the joined record its secret back, keeping
        // its id and the name the person chose.
        let restored = import_recovery_words_with_provider(
            dir.path(),
            "ignored",
            &words_of(43),
            "Someone",
            &provider,
        )
        .unwrap();
        assert_eq!(restored, joined);
        assert!(!dir.path().join("ignored.json").exists());
        let identity = super::super::load_from_path(restored).unwrap();
        assert!(identity.is_single_device());
        assert_eq!(identity.display_name(), "Ada");
        assert_eq!(identity.avatar_color(), [1, 2, 3]);
        assert_eq!(identity.group_public_key(), key);
        identity.sign(b"restored root").unwrap();
    }

    #[test]
    fn a_lost_provider_secret_is_restored_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = secured_identity(dir.path(), "primary", 45);
        let provider = super::super::BrokerFileProvider::new(dir.path());
        let locator = hex::encode(SigningKey::from_bytes(&[45; 32]).verifying_key().to_bytes());
        provider.remove(&locator).unwrap();
        assert!(super::super::load_from_path(path.clone()).is_err());
        assert!(recovery_words(&path).is_err());

        let restored =
            import_recovery_words_with_provider(dir.path(), "x", &words_of(45), "", &provider)
                .unwrap();
        assert_eq!(restored, path);
        let identity = super::super::load_from_path(path).unwrap();
        assert_eq!(identity.display_name(), "Ada");
        identity.sign(b"back").unwrap();
    }
}
