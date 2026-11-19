//! The split modules and shared chunks a split emitted, kept by what each was
//! emitted from, so the next split of a module that changed in one place
//! emits again only the modules that place reaches (`WASM_SPLIT_CACHE`).
//!
//! A module's key covers everything its emission reads: the start module's
//! interface, which is what it imports and the table's slots it fills (a
//! module is never paired with a start module it was not emitted beside, and
//! a start module's code or static data alone is no part of that), the bound
//! module's sections other than the ones a renumbering rewrites, the bodies
//! the module keeps (the parse it is emitted from leaves every other body
//! `unreachable`), what it defines, imports and deletes, the order of the
//! shared functions' table slots, its entries' slots and the static data it
//! writes. Every key is made by the splitter's own bytes too
//! ([`tool_version`]), so an emission a different one made is never read. A
//! module whose key is in the directory is that file, byte for byte what
//! emitting it again writes.
//!
//! What it reads, it reads by content: a function by its name, a body by what
//! it does rather than the indices it holds, a data symbol by its name
//! (content.rs). A function added or removed elsewhere renumbers the module
//! without changing any module's own code, so the modules it does not reach
//! keep their keys and are read back; only the modules holding changed or
//! renumbered code are emitted again.

use std::collections::{BTreeSet, HashSet};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};

use crate::data::DataPlan;
use crate::split::read_leb;

/// Raised whenever what the emission writes changes, so a directory a
/// different splitter filled is never read as this one's. The splitter's own
/// bytes are keyed too ([`tool_version`]), so a change to its code needs no
/// bump here.
const VERSION: &str = "wasm-split emit cache 2";

/// What the splitter itself is, by the bytes of the binary running it: a key
/// one splitter made is never read by another whose emission may differ,
/// whatever part of it changed. Read once per build; a binary this cannot read
/// answers what [`VERSION`] alone says.
fn tool_version() -> u64 {
    static TOOL: OnceLock<u64> = OnceLock::new();
    *TOOL.get_or_init(|| {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        match std::env::current_exe().map(std::fs::read) {
            Ok(Ok(bytes)) => bytes.hash(&mut hasher),
            _ => VERSION.hash(&mut hasher),
        }
        hasher.finish()
    })
}

pub struct EmitCache {
    dir: PathBuf,
    used: Mutex<HashSet<String>>,
}

impl EmitCache {
    /// The cache `WASM_SPLIT_CACHE` names, if it names one (empty names none).
    pub fn from_env() -> Result<Option<Self>> {
        std::env::var_os("WASM_SPLIT_CACHE")
            .filter(|dir| !dir.is_empty())
            .map(|dir| Self::at(Path::new(&dir)))
            .transpose()
    }

    /// The cache in directory `dir`, made if it is not there.
    pub fn at(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self {
            dir: dir.to_owned(),
            used: Mutex::default(),
        })
    }

    fn path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.wasm"))
    }

    /// The module emitted from `key` before, if this cache kept it whole.
    /// A file a stopped write or a copy left empty is no module: it is
    /// dropped here, and the caller emits that module again.
    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        let path = self.path(key);
        let bytes = std::fs::read(&path).ok()?;
        if bytes.is_empty() {
            remove(&path).ok();
            return None;
        }
        self.used.lock().expect("cache lock").insert(key.to_owned());
        Some(bytes)
    }

    /// Keep `bytes` as the module emitted from `key`. Written beside and
    /// renamed in, so a split that stops leaves no half module under a key,
    /// and made again when the directory went away under the split: what a
    /// stopped build removed costs the write, never the module.
    pub fn put(&self, key: &str, bytes: &[u8]) -> Result<()> {
        let path = self.path(key);
        let partial = self.dir.join(format!(".{key}.{}", std::process::id()));
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))?;
        std::fs::write(&partial, bytes)
            .with_context(|| format!("writing {}", partial.display()))?;
        std::fs::rename(&partial, &path).with_context(|| format!("writing {}", path.display()))?;
        self.used.lock().expect("cache lock").insert(key.to_owned());
        Ok(())
    }

    /// Drop every module this split neither read nor wrote: a module that
    /// changed is never asked for again under its old key.
    pub fn prune(&self) -> Result<()> {
        let used = self.used.lock().expect("cache lock");
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            // A directory a stopped build's cleanup removed under this split
            // holds nothing to prune: the split's modules are written.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", self.dir.display()));
            }
        };
        for entry in entries {
            let path = entry?.path();
            // Only this cache's finished modules: the directory holds the
            // analysis too (analysis.rs), which is not this cache's to prune,
            // and hidden partials that a split writing beside this one is
            // still filling.
            if !path.extension().is_some_and(|e| e == "wasm")
                || path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_none_or(|name| name.starts_with('.'))
            {
                continue;
            }
            let keep = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| used.contains(stem));
            if !keep {
                remove(&path)?;
            }
        }
        Ok(())
    }
}

/// The start module's emission (its bytes and the static data it writes)
/// kept by what that emission reads: the plan, and the code of every body the
/// start module keeps.
const START_MAGIC: &[u8; 8] = b"WSSSTART";

/// How many start modules the directory keeps: a module alternates between a
/// few states (a change and its revert, two branches), and one is tens of
/// megabytes.
const START_KEEP: usize = 4;

impl EmitCache {
    fn start_path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("start-{key}.bin"))
    }

    /// The start module emitted from `key` before, with the static data it
    /// writes, if this cache kept them whole. A file a stopped write or a copy
    /// left empty is no module: it is dropped here, and the caller emits it
    /// again.
    pub fn get_start(&self, key: &str) -> Option<(Vec<u8>, DataPlan)> {
        let bytes = std::fs::read(self.start_path(key)).ok()?;
        let read = read_start(&bytes)?;
        // An empty module is no module: a stopped copy left it.
        if read.0.is_empty() {
            std::fs::remove_file(self.start_path(key)).ok();
            return None;
        }
        Some(read)
    }

    /// Keep `main` and `data` as the start module emitted from `key`.
    pub fn put_start(&self, key: &str, main: &[u8], data: &DataPlan) -> Result<()> {
        let mut out = Vec::with_capacity(main.len() + 4096);
        out.extend_from_slice(START_MAGIC);
        out.extend_from_slice(&(data.moved.len() as u64).to_le_bytes());
        for runs in &data.moved {
            out.extend_from_slice(&(runs.len() as u64).to_le_bytes());
            for (address, bytes) in runs {
                out.extend_from_slice(&address.to_le_bytes());
                out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
                out.extend_from_slice(bytes);
            }
        }
        out.extend_from_slice(&(data.removed().len() as u64).to_le_bytes());
        for range in data.removed() {
            out.extend_from_slice(&range.start.to_le_bytes());
            out.extend_from_slice(&range.end.to_le_bytes());
        }
        out.extend_from_slice(main);
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))?;
        let path = self.start_path(key);
        let partial = self
            .dir
            .join(format!(".start-{key}.{}", std::process::id()));
        std::fs::write(&partial, &out).with_context(|| format!("writing {}", partial.display()))?;
        std::fs::rename(&partial, &path).with_context(|| format!("writing {}", path.display()))?;
        self.prune_start(key);
        Ok(())
    }

    /// Drop the start modules this run did not use, past the last few.
    fn prune_start(&self, used: &str) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let mut kept = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(key) = name
                .strip_prefix("start-")
                .and_then(|rest| rest.strip_suffix(".bin"))
            else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let when = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            kept.push((when, key.to_owned()));
        }
        kept.sort_by_key(|(when, _)| std::cmp::Reverse(*when));
        for (_, key) in kept
            .into_iter()
            .filter(|(_, key)| key != used)
            .skip(START_KEEP - 1)
        {
            std::fs::remove_file(self.start_path(&key)).ok();
        }
    }
}

/// The start module a cache file holds, and the data it writes.
fn read_start(bytes: &[u8]) -> Option<(Vec<u8>, DataPlan)> {
    let mut at = 0usize;
    let take = |len: usize, at: &mut usize| -> Option<&[u8]> {
        let slice = bytes.get(*at..at.checked_add(len)?)?;
        *at += len;
        Some(slice)
    };
    if take(START_MAGIC.len(), &mut at)? != START_MAGIC {
        return None;
    }
    let count =
        |at: &mut usize| -> Option<u64> { Some(u64::from_le_bytes(take(8, at)?.try_into().ok()?)) };
    let mut moved = Vec::new();
    for _ in 0..count(&mut at)? {
        let mut runs = Vec::new();
        for _ in 0..count(&mut at)? {
            let address = u32::from_le_bytes(take(4, &mut at)?.try_into().ok()?);
            let len = count(&mut at)? as usize;
            runs.push((address, take(len, &mut at)?.to_vec()));
        }
        moved.push(runs);
    }
    let mut removed = Vec::new();
    for _ in 0..count(&mut at)? {
        let start = u32::from_le_bytes(take(4, &mut at)?.try_into().ok()?);
        let end = u32::from_le_bytes(take(4, &mut at)?.try_into().ok()?);
        removed.push(start..end);
    }
    let main = bytes.get(at..)?.to_vec();
    Some((main, DataPlan::from_ranges(moved, removed)))
}

fn remove(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(error).with_context(|| format!("removing {}", path.display()))
        }
        _ => Ok(()),
    }
}

/// A 128-bit key built from what an emission reads: two SipHash-1-3 passes
/// with fixed, different keys, each starting from the cache's format and the
/// splitter's own bytes ([`tool_version`]), so no key outlives the splitter
/// that made it.
#[derive(Clone)]
pub struct Key {
    hashers: [std::hash::DefaultHasher; 2],
}

impl Key {
    pub fn new() -> Self {
        let mut hashers = [
            std::hash::DefaultHasher::new(),
            std::hash::DefaultHasher::new(),
        ];
        for (salt, hasher) in hashers.iter_mut().enumerate() {
            salt.hash(hasher);
            VERSION.hash(hasher);
            tool_version().hash(hasher);
        }
        Self { hashers }
    }

    pub fn add<T: Hash + ?Sized>(&mut self, value: &T) -> &mut Self {
        for hasher in &mut self.hashers {
            value.hash(hasher);
        }
        self
    }

    /// A set, in its sorted order: what the emission reads of it is its
    /// members, not the order a hash set holds them in.
    pub fn add_set<T: Hash + Ord>(&mut self, values: impl IntoIterator<Item = T>) -> &mut Self {
        let sorted: BTreeSet<T> = values.into_iter().collect();
        self.add(&sorted.len());
        for value in &sorted {
            self.add(value);
        }
        self
    }

    pub fn finish(&self) -> String {
        format!(
            "{:016x}{:016x}",
            self.hashers[0].finish(),
            self.hashers[1].finish()
        )
    }
}

/// The module's data section, as bytes, when it has one.
pub(crate) fn data_section(bytes: &[u8]) -> Option<&[u8]> {
    section_range(bytes, 11).map(|range| &bytes[range])
}

/// A section's contents by id, the first with that id.
fn section_range(bytes: &[u8], wanted: u8) -> Option<Range<usize>> {
    let mut at = 8;
    while at < bytes.len() {
        let id = bytes[at];
        let mut body = at + 1;
        let size = read_leb(bytes, &mut body).ok()?;
        if id == wanted {
            return Some(body..(body + size).min(bytes.len()));
        }
        at = body + size;
    }
    None
}

/// The sections an emission does not read as bytes because a function added
/// or removed elsewhere rewrites them: the function section (3, whose entries
/// follow the functions a module keeps, each keyed by content), the export
/// section (7) and the element section (9), which the emission rebuilds from
/// the split points and the table it keeps, the code (10), the data (11) and
/// the data count (12), and every custom section but `target_features`, which
/// the emission drops (`remove_custom_sections`).
pub(crate) fn sections_not_keyed(bytes: &[u8]) -> Result<Vec<Range<usize>>> {
    let mut skipped = Vec::new();
    let mut at = 8;
    while at < bytes.len() {
        let start = at;
        let id = bytes[at];
        at += 1;
        let size = read_leb(bytes, &mut at)?;
        let contents = at;
        at = (at + size).min(bytes.len());
        let custom_kept =
            id == 0 && custom_section_name(bytes, contents) == Some("target_features");
        // The types are left out too: a module's emission writes the types
        // its own functions and bodies name, in the bind's canonical order,
        // so a type added elsewhere (a function given a signature the module
        // does not use) moves no module's bytes.
        if id == 1 || (3..=12).contains(&id) || (id == 0 && !custom_kept) {
            skipped.push(start..at);
        }
    }
    Ok(skipped)
}

/// A custom section's name, which its contents hold first.
pub(crate) fn custom_section_name(bytes: &[u8], contents: usize) -> Option<&str> {
    let mut at = contents;
    let len = read_leb(bytes, &mut at).ok()?;
    std::str::from_utf8(bytes.get(at..at + len)?).ok()
}

/// `key` over `bytes` without `skipped`, which must be in order, as it is.
pub(crate) fn key_sections(key: &mut Key, bytes: &[u8], skipped: &[Range<usize>]) -> Result<()> {
    let mut at = 0;
    for range in skipped {
        if range.start > at {
            key.add(&bytes[at..range.start]);
        }
        at = at.max(range.end);
    }
    if at < bytes.len() {
        key.add(&bytes[at..]);
    }
    Ok(())
}
