//! Bounded, crash-safe generations for durable collaborative workspace files.
//!
//! This module is intentionally not used for identity keys, ACLs, or share
//! authority: rolling those stores backwards can revive an identity or grant.
//! It is for replicated or reconstructable workspace state. A damaged primary
//! is never discarded: recovery copies its exact bytes to an evidence file and
//! writes a receipt naming both hashes before returning the recovered bytes.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tilo_clock::unix_now_ms;

pub const RETAINED_GENERATIONS: usize = 3;
pub const MAX_DURABLE_STATE_BYTES: u64 = 256 * 1024 * 1024;
const BACKUP_SUFFIX: &str = ".tilo-backups";
const RECEIPT_VERSION: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryEvidence {
    pub backup_path: PathBuf,
    pub damaged_evidence_path: PathBuf,
    pub receipt_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadOutcome {
    pub bytes: Vec<u8>,
    pub recovery: Option<RecoveryEvidence>,
}

/// Read a primary under the same allocation bound used by recovery. Callers
/// that expose format-specific errors can validate this first, then invoke
/// [`read_or_recover`] only after validation fails.
///
/// # Errors
///
/// Returns an error when the file cannot be read or exceeds the durable-state
/// byte bound.
pub fn read_primary(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match read_bounded(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct RecoveryReceiptV1 {
    schema_version: u16,
    attempted_at_ms: u64,
    completed: bool,
    primary_path: String,
    damaged_blake3: String,
    recovered_blake3: String,
    backup_file: String,
    evidence_file: String,
}

/// Read and validate a primary file, recovering the newest valid retained
/// generation when (and only when) a present primary is invalid.
///
/// A missing primary remains missing. An invalid primary with no valid backup
/// fails closed and is left byte-for-byte untouched.
///
/// # Errors
///
/// Returns an error for I/O or serialization failure, an oversized primary,
/// or damaged state with no valid retained generation.
pub fn read_or_recover(
    path: &Path,
    validate: impl Fn(&[u8]) -> bool,
) -> io::Result<Option<ReadOutcome>> {
    let primary = match read_bounded(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if validate(&primary) {
        return Ok(Some(ReadOutcome {
            bytes: primary,
            recovery: None,
        }));
    }

    let backup_dir = backup_dir(path)?;
    let mut candidates = backup_files(&backup_dir)?;
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
    let recovered = candidates.into_iter().find_map(|(_, candidate)| {
        let bytes = read_bounded(&candidate).ok()?;
        (hash(&bytes) == hash_from_backup_name(&candidate)? && validate(&bytes))
            .then_some((candidate, bytes))
    });
    let Some((backup_path, recovered)) = recovered else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is damaged and no valid retained generation is available",
                path.display()
            ),
        ));
    };

    fs::create_dir_all(&backup_dir)?;
    let generation = next_artifact_id(&backup_dir)?;
    let damaged_hash = hash(&primary);
    let recovered_hash = hash(&recovered);
    let evidence_path =
        backup_dir.join(format!("evidence-{generation:020}-{damaged_hash}.corrupt"));
    tilo_files::native::atomic::write(&evidence_path, &primary)?;
    let receipt_path = backup_dir.join(format!("recovery-{generation:020}.json"));
    let mut receipt = RecoveryReceiptV1 {
        schema_version: RECEIPT_VERSION,
        attempted_at_ms: unix_now_ms(),
        completed: false,
        primary_path: path.display().to_string(),
        damaged_blake3: damaged_hash,
        recovered_blake3: recovered_hash,
        backup_file: file_name(&backup_path)?,
        evidence_file: file_name(&evidence_path)?,
    };
    let receipt_bytes = serde_json::to_vec_pretty(&receipt).map_err(io::Error::other)?;
    tilo_files::native::atomic::write(&receipt_path, &receipt_bytes)?;
    // Do not route this through `write`: that deliberately refuses to replace
    // an invalid primary. The exact bad bytes and recovery intent are durable.
    tilo_files::native::atomic::write(path, &recovered)?;
    receipt.completed = true;
    if let Ok(receipt_bytes) = serde_json::to_vec_pretty(&receipt) {
        // If this final annotation cannot be written (for example, the device
        // became full after the primary rename), the durable `completed:false`
        // receipt remains. That is conservative and still identifies every
        // byte involved; startup can compare the named recovered hash.
        if let Err(error) = tilo_files::native::atomic::write(&receipt_path, &receipt_bytes) {
            eprintln!(
                "workspace state recovered but completion receipt update failed at {}: {error}",
                receipt_path.display()
            );
        }
    }
    prune_recovery_artifacts(&backup_dir)?;

    Ok(Some(ReadOutcome {
        bytes: recovered,
        recovery: Some(RecoveryEvidence {
            backup_path,
            damaged_evidence_path: evidence_path,
            receipt_path,
        }),
    }))
}

/// Validate and atomically replace a primary, retaining its previous valid
/// contents as one bounded generation. A concurrently damaged primary is not
/// overwritten and is not promoted into the backup set.
///
/// # Errors
///
/// Returns an error when either candidate or existing state is invalid or too
/// large, or when directory, backup, atomic-write, or pruning I/O fails.
pub fn write(path: &Path, bytes: &[u8], validate: impl Fn(&[u8]) -> bool) -> io::Result<()> {
    if !validate(bytes) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to persist invalid workspace state",
        ));
    }
    if bytes.len() as u64 > MAX_DURABLE_STATE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "workspace state exceeds the durable backup byte bound",
        ));
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }

    match read_bounded(path) {
        Ok(previous) => {
            if !validate(&previous) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "refusing to overwrite damaged workspace state at {}",
                        path.display()
                    ),
                ));
            }
            if previous == bytes {
                return Ok(());
            }
            let backup_dir = backup_dir(path)?;
            fs::create_dir_all(&backup_dir)?;
            let generation = next_generation(&backup_dir)?;
            let backup = backup_dir.join(format!(
                "generation-{generation:020}-{}.bak",
                hash(&previous)
            ));
            tilo_files::native::atomic::write(&backup, &previous)?;
            tilo_files::native::atomic::write(path, bytes)?;
            prune_generations(&backup_dir)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            tilo_files::native::atomic::write(path, bytes)?;
        }
        Err(error) => return Err(error),
    }
    Ok(())
}

fn backup_dir(path: &Path) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid state filename"))?;
    Ok(path.with_file_name(format!(".{name}{BACKUP_SUFFIX}")))
}

fn backup_files(dir: &Path) -> io::Result<Vec<(u64, PathBuf)>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(rest) = name.strip_prefix("generation-") else {
            continue;
        };
        let Some((generation, _)) = rest.split_once('-') else {
            continue;
        };
        let Ok(generation) = generation.parse() else {
            continue;
        };
        if name
            .rsplit_once('.')
            .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("bak"))
        {
            files.push((generation, entry.path()));
        }
    }
    Ok(files)
}

fn next_generation(dir: &Path) -> io::Result<u64> {
    let generation = backup_files(dir)?
        .into_iter()
        .map(|(generation, _)| generation)
        .max()
        .unwrap_or(0);
    generation
        .checked_add(1)
        .ok_or_else(|| io::Error::other("workspace backup generation exhausted"))
}

fn next_artifact_id(dir: &Path) -> io::Result<u64> {
    let mut maximum = backup_files(dir)?
        .into_iter()
        .map(|(generation, _)| generation)
        .max()
        .unwrap_or(0);
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        for prefix in ["evidence-", "recovery-"] {
            let Some(rest) = name.strip_prefix(prefix) else {
                continue;
            };
            let digits = rest.split(['-', '.']).next().unwrap_or_default();
            if let Ok(value) = digits.parse::<u64>() {
                maximum = maximum.max(value);
            }
        }
    }
    maximum
        .checked_add(1)
        .ok_or_else(|| io::Error::other("workspace recovery generation exhausted"))
}

fn prune_generations(dir: &Path) -> io::Result<()> {
    let mut files = backup_files(dir)?;
    files.sort_by_key(|(generation, _)| *generation);
    let remove = files.len().saturating_sub(RETAINED_GENERATIONS);
    for (_, path) in files.into_iter().take(remove) {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn prune_recovery_artifacts(dir: &Path) -> io::Result<()> {
    for prefix in ["evidence-", "recovery-"] {
        let mut paths = fs::read_dir(dir)?
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_type().is_ok_and(|kind| kind.is_file())
                    && entry.file_name().to_string_lossy().starts_with(prefix)
            })
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        paths.sort();
        let remove = paths.len().saturating_sub(RETAINED_GENERATIONS);
        for path in paths.into_iter().take(remove) {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
    let too_large = io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{} exceeds the durable state byte bound", path.display()),
    );
    tilo_files::native::bounded::read_or(path, MAX_DURABLE_STATE_BYTES, too_large)?
        .ok_or_else(|| io::ErrorKind::NotFound.into())
}

fn hash_from_backup_name(path: &Path) -> Option<String> {
    path.file_name()?
        .to_str()?
        .strip_suffix(".bak")?
        .rsplit_once('-')
        .map(|(_, hash)| hash.to_owned())
}

fn file_name(path: &Path) -> io::Result<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid backup filename"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid(bytes: &[u8]) -> bool {
        bytes.starts_with(b"valid:")
    }

    #[test]
    fn backups_retain_generations_and_recover_from_the_newest_valid_one() {
        // Phase `retains_three_generations_and_recovers_with_evidence`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("namespace.bin");
            for value in 0..6 {
                write(&path, format!("valid:{value}").as_bytes(), valid).unwrap();
            }
            let backups = backup_files(&backup_dir(&path).unwrap()).unwrap();
            assert_eq!(backups.len(), RETAINED_GENERATIONS);

            fs::write(&path, b"damaged-current").unwrap();
            let outcome = read_or_recover(&path, valid).unwrap().unwrap();
            assert_eq!(outcome.bytes, b"valid:4");
            let evidence = outcome.recovery.unwrap();
            assert_eq!(
                fs::read(&evidence.damaged_evidence_path).unwrap(),
                b"damaged-current"
            );
            assert_eq!(fs::read(&path).unwrap(), b"valid:4");
            let receipt: RecoveryReceiptV1 =
                serde_json::from_slice(&fs::read(evidence.receipt_path).unwrap()).unwrap();
            assert_eq!(receipt.damaged_blake3, hash(b"damaged-current"));
            assert_eq!(receipt.recovered_blake3, hash(b"valid:4"));
            assert!(receipt.completed);
        }
        // Phase `a_corrupt_newest_backup_falls_back_to_an_older_valid_generation`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("state.json");
            write(&path, b"valid:one", valid).unwrap();
            write(&path, b"valid:two", valid).unwrap();
            write(&path, b"valid:three", valid).unwrap();
            let backup_dir = backup_dir(&path).unwrap();
            let newest = backup_files(&backup_dir)
                .unwrap()
                .into_iter()
                .max_by_key(|(generation, _)| *generation)
                .unwrap()
                .1;
            fs::write(newest, b"valid:tampered").unwrap();
            fs::write(&path, b"damaged").unwrap();
            let outcome = read_or_recover(&path, valid).unwrap().unwrap();
            assert_eq!(outcome.bytes, b"valid:one");
        }
        // Phase `repeated_recovery_retains_distinct_evidence_and_receipts`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("state.json");
            write(&path, b"valid:one", valid).unwrap();
            write(&path, b"valid:two", valid).unwrap();
            for damaged in [b"damaged-one".as_slice(), b"damaged-two".as_slice()] {
                fs::write(&path, damaged).unwrap();
                assert!(
                    read_or_recover(&path, valid)
                        .unwrap()
                        .unwrap()
                        .recovery
                        .is_some()
                );
            }
            let backup_dir = backup_dir(&path).unwrap();
            let names = fs::read_dir(backup_dir)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            assert_eq!(
                names
                    .iter()
                    .filter(|name| name.starts_with("evidence-"))
                    .count(),
                2
            );
            assert_eq!(
                names
                    .iter()
                    .filter(|name| name.starts_with("recovery-"))
                    .count(),
                2
            );
        }
    }

    #[test]
    fn damaged_or_oversized_primaries_are_preserved_untouched() {
        // Phase `invalid_primary_without_valid_backup_is_preserved`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("state.json");
            fs::write(&path, b"damaged").unwrap();
            assert!(read_or_recover(&path, valid).is_err());
            assert_eq!(fs::read(&path).unwrap(), b"damaged");
            assert!(!backup_dir(&path).unwrap().exists());
        }
        // Phase `write_refuses_to_overwrite_concurrently_damaged_primary`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("state.json");
            write(&path, b"valid:one", valid).unwrap();
            write(&path, b"valid:two", valid).unwrap();
            fs::write(&path, b"damaged").unwrap();
            assert!(write(&path, b"valid:three", valid).is_err());
            assert_eq!(fs::read(&path).unwrap(), b"damaged");
        }
        // Phase `oversized_primary_fails_before_allocation_and_remains_untouched`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("state.json");
            let file = fs::File::create(&path).unwrap();
            file.set_len(MAX_DURABLE_STATE_BYTES + 1).unwrap();
            drop(file);
            let error = read_or_recover(&path, |_| true).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(
                fs::metadata(&path).unwrap().len(),
                MAX_DURABLE_STATE_BYTES + 1
            );
        }
    }
}
