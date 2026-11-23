//! A read of the files the host kept its changes in before the device store,
//! as the move needs them: nothing here writes.
//!
//! - `retained/v2/<route>/CURRENT` names the route's generation: a directory
//!   of `*.delta` files, `checkpoint.delta` first, then the rest by name.
//!   A generation's `.journal` holds every change a group commit made durable,
//!   so a change whose file a crash left missing or torn is read from there.
//! - `pending/<device>/*.delta` are the changes that waited for a device that
//!   was offline, oldest first.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::Path;

const JOURNAL: &str = ".journal";
const MAGIC: [u8; 4] = *b"tgj1";
/// Magic, name length, body length, body digest, boot.
const FIXED: usize = 4 + 2 + 8 + 32 + 16;
/// The check over an entry's header and name.
const CHECK: usize = 8;

/// Every change `v2` keeps, route by route, each once.
pub(super) fn retained(v2: &Path) -> io::Result<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for route in sorted(v2)? {
        if !route.is_dir() {
            continue;
        }
        let Some(generation) = current(&route)? else {
            continue;
        };
        for change in generation_changes(&generation)? {
            if seen.insert(blake3::hash(&change)) {
                out.push(change);
            }
        }
    }
    Ok(out)
}

/// Every change waiting in `pending`, device by device, oldest first.
pub(super) fn pending(pending: &Path) -> io::Result<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    for device in sorted(pending)? {
        if !device.is_dir() {
            continue;
        }
        for path in sorted(&device)? {
            if path.extension().is_some_and(|ext| ext == "delta") {
                out.push(fs::read(path)?);
            }
        }
    }
    Ok(out)
}

/// The entries of `dir` by name; none when it does not exist.
fn sorted(dir: &Path) -> io::Result<Vec<std::path::PathBuf>> {
    let mut paths = match fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<Vec<_>>>()?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    paths.sort();
    Ok(paths)
}

/// The generation `route`'s `CURRENT` names, if it names one.
fn current(route: &Path) -> io::Result<Option<std::path::PathBuf>> {
    let name = match fs::read_to_string(route.join("CURRENT")) {
        Ok(name) => name,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if name.len() != 32 || !name.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::other(format!(
            "{}: CURRENT names no generation",
            route.display()
        )));
    }
    let generation = route.join(name);
    if !generation.is_dir() {
        return Err(io::Error::other(format!(
            "{}: the generation CURRENT names is missing",
            generation.display()
        )));
    }
    Ok(Some(generation))
}

/// The changes of `generation` in their order, each as its file holds it or,
/// where the file is missing or not what the journal names, as the journal
/// holds it.
fn generation_changes(generation: &Path) -> io::Result<Vec<Vec<u8>>> {
    let journal = journal(&generation.join(JOURNAL))?;
    let mut names: Vec<String> = journal.keys().cloned().collect();
    for path in sorted(generation)? {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with('.')
            && path.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("delta"))
        {
            names.push(name.to_owned());
        }
    }
    names.sort();
    names.dedup();
    if let Some(at) = names.iter().position(|name| name == "checkpoint.delta") {
        let checkpoint = names.remove(at);
        names.insert(0, checkpoint);
    }
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let file = match fs::read(generation.join(&name)) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let change = match (file, journal.get(&name)) {
            (Some(bytes), Some(kept)) if blake3::hash(&bytes) != blake3::hash(kept) => kept.clone(),
            (Some(bytes), _) => bytes,
            (None, Some(kept)) => kept.clone(),
            (None, None) => continue,
        };
        out.push(change);
    }
    Ok(out)
}

/// What the journal at `path` holds, by record name: the later of two entries
/// for one name is the record. The journal ends at its first entry that does
/// not read whole, a torn append.
fn journal(path: &Path) -> io::Result<HashMap<String, Vec<u8>>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => return Err(error),
    };
    let mut out = HashMap::new();
    let mut rest = bytes.as_slice();
    while let Some((entry, next)) = journal_entry(rest) {
        out.insert(entry.0, entry.1);
        rest = next;
    }
    Ok(out)
}

/// One whole journal entry: its device's name, its bytes, and what follows it.
type JournalEntry<'a> = ((String, Vec<u8>), &'a [u8]);

/// One whole journal entry at the start of `bytes`, and what follows it.
fn journal_entry(bytes: &[u8]) -> Option<JournalEntry<'_>> {
    let (fixed, rest) = bytes.split_at_checked(FIXED)?;
    if fixed.get(..4)? != MAGIC {
        return None;
    }
    let name_len = usize::from(u16::from_le_bytes(fixed.get(4..6)?.try_into().ok()?));
    let len = usize::try_from(u64::from_le_bytes(fixed.get(6..14)?.try_into().ok()?)).ok()?;
    let digest = fixed.get(14..46)?;
    let (name, rest) = rest.split_at_checked(name_len)?;
    let (check, rest) = rest.split_at_checked(CHECK)?;
    let mut header = fixed.to_vec();
    header.extend_from_slice(name);
    if blake3::hash(&header).as_bytes().get(..CHECK)? != check {
        return None;
    }
    let (body, rest) = rest.split_at_checked(len)?;
    if blake3::hash(body).as_bytes().as_slice() != digest {
        return None;
    }
    let name = String::from_utf8(name.to_vec()).ok()?;
    if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
        return None;
    }
    Some(((name, body.to_vec()), rest))
}
