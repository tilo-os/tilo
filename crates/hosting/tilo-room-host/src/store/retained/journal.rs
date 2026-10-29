//! A generation's journal ([`JOURNAL`]): the records its group commits
//! made durable, in one file, with one barrier per group.
//!
//! Each entry names a record of the generation and holds its bytes. A
//! record is renamed into place only after the entry naming it is durable,
//! so after a crash every record the generation lists by name is whole or
//! named by the journal. Opening the store restores, from the journal of
//! each lineage's active generation, every record that is missing or not
//! what its entry holds ([`recover`]). Within the boot that wrote an entry
//! the page cache still holds what was written, so a record of the right
//! size is taken as whole; after a reboot its bytes are compared.
//!
//! The journal is collected with its generation. A replacement generation
//! hard-links survivors through [`tilo_relay_store::durable::link`], which
//! syncs a record durable only here before its file outlives the journal.
use super::{StoreError, current, sync_dir};
use std::{
    collections::HashMap,
    fs,
    io::{self, Read as _, Seek as _, Write as _},
    path::{Path, PathBuf},
};
pub(crate) use tilo_relay_store::durable::JOURNAL;

const MAGIC: [u8; 4] = *b"tgj1";
/// Magic, name length, body length, body digest, boot.
const FIXED: usize = 4 + 2 + 8 + 32 + 16;
/// The check over an entry's header and name.
const CHECK: usize = 8;

/// One record an entry names.
pub(super) struct Entry<'a> {
    pub(super) name: &'a str,
    pub(super) bytes: &'a [u8],
    pub(super) digest: &'a [u8; 32],
}

/// This boot, as the journal records it; zero where it is not known, which
/// makes recovery compare every record.
fn boot() -> [u8; 16] {
    static BOOT: std::sync::OnceLock<[u8; 16]> = std::sync::OnceLock::new();
    *BOOT.get_or_init(|| {
        #[cfg(target_os = "linux")]
        if let Ok(id) = fs::read_to_string("/proc/sys/kernel/random/boot_id") {
            let hash = blake3::hash(id.trim().as_bytes());
            if let Some(boot) = hash.as_bytes().first_chunk::<16>() {
                return *boot;
            }
        }
        [0; 16]
    })
}

fn check(header: &[u8]) -> [u8; CHECK] {
    let hash = blake3::hash(header);
    hash.as_bytes()
        .first_chunk::<CHECK>()
        .copied()
        .unwrap_or_default()
}

/// Encode `entries` as one append.
pub(super) fn encode(entries: &[Entry<'_>]) -> Result<Vec<u8>, StoreError> {
    let boot = boot();
    let mut out = Vec::new();
    for entry in entries {
        let name_len = u16::try_from(entry.name.len())
            .map_err(|_| io::Error::other("retained record name too long"))?;
        let start = out.len();
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(&(entry.bytes.len() as u64).to_le_bytes());
        out.extend_from_slice(entry.digest);
        out.extend_from_slice(&boot);
        out.extend_from_slice(entry.name.as_bytes());
        let header = check(out.get(start..).unwrap_or_default());
        out.extend_from_slice(&header);
        out.extend_from_slice(entry.bytes);
    }
    Ok(out)
}

/// Append `bytes` to `generation`'s journal and make them durable: the file,
/// and when this created it, its entry in the generation. On failure the
/// journal is cut back to what it held, as far as that is possible.
pub(super) fn append(generation: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let path = generation.join(JOURNAL);
    let created = !path.exists();
    let mut file = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)?;
    let before = file.metadata()?.len();
    let written = file
        .write_all(bytes)
        .map_err(StoreError::from)
        .and_then(|()| {
            #[cfg(test)]
            super::super::test_barrier::check(generation, "journal-sync")?;
            crate::store::fsync(&file)?;
            if created {
                sync_dir(generation)?;
            }
            Ok(())
        });
    if written.is_err() {
        let _ = file.set_len(before);
    }
    written
}

/// An entry as read back: where its body is.
struct Read {
    name: String,
    offset: u64,
    len: u64,
    digest: [u8; 32],
    boot: [u8; 16],
}

/// Every whole entry header of `path`, in order; a torn tail ends them.
fn entries(path: &Path) -> Result<Vec<Read>, StoreError> {
    let mut file = io::BufReader::new(fs::File::open(path)?);
    let total = file.get_ref().metadata()?.len();
    let mut offset = 0_u64;
    let mut out = Vec::new();
    loop {
        let mut fixed = [0_u8; FIXED];
        if file.read_exact(&mut fixed).is_err() || fixed.get(..4) != Some(&MAGIC[..]) {
            break;
        }
        let field = |range: std::ops::Range<usize>| fixed.get(range).unwrap_or_default();
        let name_len = u16::from_le_bytes(field(4..6).try_into().unwrap_or_default());
        let len = u64::from_le_bytes(field(6..14).try_into().unwrap_or_default());
        let digest: [u8; 32] = field(14..46).try_into().unwrap_or_default();
        let boot: [u8; 16] = field(46..62).try_into().unwrap_or_default();
        let mut name = vec![0_u8; usize::from(name_len)];
        let mut stored = [0_u8; CHECK];
        if file.read_exact(&mut name).is_err() || file.read_exact(&mut stored).is_err() {
            break;
        }
        let mut header = fixed.to_vec();
        header.extend_from_slice(&name);
        if check(&header) != stored {
            break;
        }
        let body = offset
            .saturating_add((FIXED + CHECK) as u64)
            .saturating_add(u64::from(name_len));
        let end = body.saturating_add(len);
        // optional-decode: the journal ends at its first entry that does not read (a torn tail)
        let Ok(name) = String::from_utf8(name) else {
            break;
        };
        // A record's name, never a path elsewhere.
        if end > total || name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
            break;
        }
        out.push(Read {
            name,
            offset: body,
            len,
            digest,
            boot,
        });
        file.seek(io::SeekFrom::Start(end))?;
        offset = end;
    }
    Ok(out)
}

/// Whether the record at `path` holds what `entry` names.
fn whole(path: &Path, entry: &Read) -> Result<bool, StoreError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if metadata.len() != entry.len {
        return Ok(false);
    }
    if entry.boot == boot() && entry.boot != [0; 16] {
        return Ok(true);
    }
    Ok(blake3::hash(&fs::read(path)?).as_bytes() == &entry.digest)
}

/// Restore what the journal of `generation` names and its directory lacks.
fn recover_generation(generation: &Path) -> Result<(), StoreError> {
    let path = generation.join(JOURNAL);
    let read = entries(&path)?;
    // A name the journal holds twice was appended again after a group whose
    // barrier failed: the later entry is the record.
    let mut last = HashMap::new();
    for (index, entry) in read.iter().enumerate() {
        last.insert(entry.name.as_str(), index);
    }
    let mut journal = None;
    let mut restored = false;
    for (index, entry) in read.iter().enumerate() {
        if last.get(entry.name.as_str()) != Some(&index) {
            continue;
        }
        let record = generation.join(&entry.name);
        if whole(&record, entry)? {
            // Read back after a reboot, it is on disk; otherwise it may be
            // in the page cache only.
            if entry.boot == boot() || boot() == [0; 16] {
                tilo_relay_store::durable::journaled(&record);
            }
            continue;
        }
        let file = match &mut journal {
            Some(file) => file,
            None => journal.insert(fs::File::open(&path)?),
        };
        file.seek(io::SeekFrom::Start(entry.offset))?;
        let mut bytes = vec![0_u8; usize::try_from(entry.len).map_err(io::Error::other)?];
        file.read_exact(&mut bytes)?;
        if blake3::hash(&bytes).as_bytes() != &entry.digest {
            // A torn append, whose group no commit was acknowledged for.
            break;
        }
        let staging = generation.join(format!(".{}.recover.{}", entry.name, std::process::id()));
        let mut out = fs::File::create(&staging)?;
        out.write_all(&bytes)?;
        crate::store::fsync(&out)?;
        fs::rename(&staging, &record)?;
        restored = true;
    }
    if restored {
        sync_dir(generation)?;
    }
    remove_stale_staging(generation);
    Ok(())
}

/// Group staging files another process left: each was journaled before its
/// rename or belongs to a group no commit was acknowledged for.
fn remove_stale_staging(generation: &Path) {
    let own = format!(".{}.group", std::process::id());
    let Ok(entries) = fs::read_dir(generation) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') && name.ends_with(".group") && !name.ends_with(&own) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Restore every record the journal of each lineage's active generation
/// under `v2` names: what a crash may have left missing or partly written.
///
/// # Errors
///
/// Returns an error when a journal or the records it names cannot be read
/// or restored.
pub(crate) fn recover(v2: &Path) -> Result<(), StoreError> {
    let lineages = match fs::read_dir(v2) {
        Ok(lineages) => lineages,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for lineage in lineages {
        let lineage: PathBuf = lineage?.path();
        if !lineage.is_dir() {
            continue;
        }
        // A lineage without a valid selection has no active generation to
        // restore; its readers report it.
        let Ok(Some(generation)) = current(&lineage) else {
            continue;
        };
        if generation.join(JOURNAL).exists() {
            recover_generation(&generation)?;
        }
    }
    Ok(())
}
