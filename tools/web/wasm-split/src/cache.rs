//! The split modules and shared chunks a split emitted, kept by what each was
//! emitted from, so the next split of a module that changed in one place
//! emits again only the modules that place reaches (`WASM_SPLIT_CACHE`).
//!
//! A module's key covers everything its emission reads: the bound module's
//! sections other than its code, the function bodies the module keeps (the
//! parse it is emitted from leaves every other body `unreachable`), what it
//! defines, imports and deletes, the order of the shared functions' table
//! slots, its entries' slots and the static data it writes. A module whose
//! key is in the directory is that file, byte for byte what emitting it
//! again writes; any change outside its kept bodies (static data moved,
//! a function added, a table slot shifted) is a change of every key.

use std::collections::{BTreeSet, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};

/// Raised whenever what the emission writes changes, so a directory a
/// different splitter filled is never read as this one's.
const VERSION: &str = "wasm-split emit cache 1";

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

    /// The module emitted from `key` before, if this cache kept it.
    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        let bytes = std::fs::read(self.path(key)).ok()?;
        self.used.lock().expect("cache lock").insert(key.to_owned());
        Some(bytes)
    }

    /// Keep `bytes` as the module emitted from `key`. Written beside and
    /// renamed in, so a split that stops leaves no half module under a key.
    pub fn put(&self, key: &str, bytes: &[u8]) -> Result<()> {
        let path = self.path(key);
        let partial = self.dir.join(format!(".{key}.{}", std::process::id()));
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
        for entry in std::fs::read_dir(&self.dir)? {
            let path = entry?.path();
            // Only this cache's modules: the directory holds the analysis
            // too (analysis.rs), which is not this cache's to prune.
            if !path.extension().is_some_and(|e| e == "wasm") {
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

fn remove(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(error).with_context(|| format!("removing {}", path.display()))
        }
        _ => Ok(()),
    }
}

/// A 128-bit key built from what an emission reads: two SipHash-1-3 passes
/// with fixed, different keys.
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
