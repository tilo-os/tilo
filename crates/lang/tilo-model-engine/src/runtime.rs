//! The model runtime a split app module links against, and where a device
//! finds it.
//!
//! A runtime is a directory ([`RUNTIME_DIR`]) holding `runtime.json`, which
//! lists its modules in the order they are instantiated (the start module
//! first) and names the runtime by its hash, and those modules. The hash is
//! BLAKE3 over each module in order, as its length (eight bytes,
//! little-endian) and its bytes: what a split app module records for the
//! runtime it was split against, and what [`Runtime::read`] computes again
//! from the bytes it reads, so a runtime whose files do not match what it says
//! it is, is refused.
//!
//! [`runtime_dirs`] is where a device looks, in order:
//!
//! - every directory [`RUNTIME_VARIABLE`] lists (`:`-separated): the server's
//!   image names the runtime it carries this way;
//! - `$TILO_APP_MODELS/app-model-runtime`: a build output beside the apps'
//!   modules, when a caller names one;
//! - `lib/app-model-runtime` of every runtime bundle installed on the device:
//!   the person's (`$TILO_RUNTIME_BUNDLE_DIR`, else
//!   `$XDG_DATA_HOME/tilo-runtime`) and the system's (`/usr/lib/tilo-runtime`).
//!   The bundle an app's package names carries the runtime its module was
//!   split against, since both come from one release build.
//!
//! A device built from another release than an app's package lacks that
//! runtime; it fetches it from the registry that offers the bundle the
//! package names (`app_cli::model_runtime`), reads it with [`Runtime::read`]
//! and keeps it with [`Runtime::remember`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use crate::ModelError;

/// The custom section of a split app module: JSON naming the runtime it was
/// split against (`runtime`, its hash) and the export to call (`entry`).
pub const RUNTIME_SECTION: &str = "tilo_model_runtime";

/// The name of a runtime's directory, under a build output and under
/// a runtime bundle's `lib/`.
pub const RUNTIME_DIR: &str = "app-model-runtime";

/// The variable that names runtime directories for a device to look in first.
pub const RUNTIME_VARIABLE: &str = "TILO_APP_MODEL_RUNTIME";

/// The variable naming a build output (`<app>/model.wasm`, and the
/// runtime under [`RUNTIME_DIR`]).
const APP_MODELS_VARIABLE: &str = "TILO_APP_MODELS";

/// What a split module's [`RUNTIME_SECTION`] says.
#[derive(Debug, serde::Deserialize)]
pub(crate) struct Named {
    pub(crate) runtime: String,
    pub(crate) entry: String,
}

/// A model runtime, read and checked.
#[derive(Debug)]
pub struct Runtime {
    hash: String,
    dir: PathBuf,
    modules: Vec<Vec<u8>>,
}

#[derive(serde::Deserialize)]
struct Manifest {
    hash: String,
    modules: Vec<String>,
}

impl Runtime {
    /// Read the runtime in `dir`.
    ///
    /// # Errors
    /// [`ModelError::Runtime`] for a directory with no runtime, a module it
    /// lists that is not there, or modules whose hash is not the one it names.
    pub fn read(dir: &Path) -> Result<Self, ModelError> {
        let refused = |what: String| ModelError::Runtime(format!("{}: {what}", dir.display()));
        let manifest = std::fs::read(dir.join("runtime.json"))
            .map_err(|error| refused(format!("runtime.json: {error}")))?;
        let manifest: Manifest = serde_json::from_slice(&manifest)
            .map_err(|error| refused(format!("runtime.json: {error}")))?;
        if manifest.modules.is_empty() {
            return Err(refused("runtime.json lists no module".into()));
        }
        let mut modules = Vec::with_capacity(manifest.modules.len());
        for file in &manifest.modules {
            if file.contains('/') || file.starts_with('.') {
                return Err(refused(format!(
                    "runtime.json lists {file:?}, not a file beside it"
                )));
            }
            modules.push(
                std::fs::read(dir.join(file))
                    .map_err(|error| refused(format!("{file}: {error}")))?,
            );
        }
        let hash = hash(&modules);
        if hash != manifest.hash {
            return Err(refused(format!(
                "its modules are runtime {hash}, not the {} it names",
                manifest.hash
            )));
        }
        Ok(Self {
            hash,
            dir: dir.to_owned(),
            modules,
        })
    }

    /// The runtime whose hash is `hash`, from the first of [`runtime_dirs`]
    /// that holds it. Each is read once per process.
    ///
    /// # Errors
    /// [`ModelError::Runtime`] naming `hash` and what each directory looked in
    /// holds, when none is it.
    pub fn find(hash: &str) -> Result<Arc<Self>, ModelError> {
        let found = found();
        if let Some(runtime) = found
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(hash)
        {
            return Ok(Arc::clone(runtime));
        }
        let mut seen = Vec::new();
        for dir in runtime_dirs() {
            if !dir.join("runtime.json").is_file() {
                continue;
            }
            match Self::read(&dir) {
                Ok(runtime) if runtime.hash == hash => {
                    let runtime = Arc::new(runtime);
                    found
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(hash.to_owned(), Arc::clone(&runtime));
                    return Ok(runtime);
                }
                Ok(runtime) => seen.push(format!("{} holds {}", dir.display(), runtime.hash)),
                Err(error) => seen.push(error.to_string()),
            }
        }
        Err(ModelError::Runtime(if seen.is_empty() {
            format!(
                "the module links against model runtime {hash}, and there is none here \
                 (looked in {}; {RUNTIME_VARIABLE} names one)",
                runtime_dirs()
                    .iter()
                    .map(|dir| dir.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else {
            format!(
                "the module links against model runtime {hash}, and no runtime here is it: {}",
                seen.join("; ")
            )
        }))
    }

    /// Keep this runtime for the rest of the process: [`Self::find`] answers
    /// its hash with it from now on, wherever it was read from (a runtime a
    /// device fetched because none of [`runtime_dirs`] holds it).
    #[must_use]
    pub fn remember(self) -> Arc<Self> {
        let runtime = Arc::new(self);
        found()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(runtime.hash.clone())
            .or_insert_with(|| Arc::clone(&runtime))
            .clone()
    }

    /// The runtime's hash.
    #[must_use]
    pub fn hash(&self) -> &str {
        &self.hash
    }

    /// Where it was read from.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Its modules, in the order they are instantiated.
    pub fn modules(&self) -> impl Iterator<Item = &[u8]> {
        self.modules.iter().map(Vec::as_slice)
    }
}

/// The runtimes this process has read, by hash.
fn found() -> &'static Mutex<HashMap<String, Arc<Runtime>>> {
    static FOUND: OnceLock<Mutex<HashMap<String, Arc<Runtime>>>> = OnceLock::new();
    FOUND.get_or_init(Mutex::default)
}

/// The runtime's hash of `modules` (the module docs say how).
fn hash(modules: &[Vec<u8>]) -> String {
    let mut hasher = blake3::Hasher::new();
    for module in modules {
        hasher.update(&(module.len() as u64).to_le_bytes());
        hasher.update(module);
    }
    hasher.finalize().to_hex().to_string()
}

/// Where a device looks for a runtime, in order (the module docs say why each).
#[must_use]
pub fn runtime_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os(RUNTIME_VARIABLE)
        .map(|paths| std::env::split_paths(&paths).collect())
        .unwrap_or_default();
    if let Some(models) = std::env::var_os(APP_MODELS_VARIABLE).filter(|dir| !dir.is_empty()) {
        dirs.push(PathBuf::from(models).join(RUNTIME_DIR));
    }
    let person = std::env::var_os("TILO_RUNTIME_BUNDLE_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_DATA_HOME")
                .filter(|dir| !dir.is_empty())
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/share"))
                })
                .map(|data| data.join("tilo-runtime"))
        });
    for root in person
        .into_iter()
        .chain([PathBuf::from("/usr/lib/tilo-runtime")])
    {
        let Ok(versions) = std::fs::read_dir(&root) else {
            continue;
        };
        let mut found: Vec<PathBuf> = versions
            .filter_map(Result::ok)
            .map(|version| version.path().join("lib").join(RUNTIME_DIR))
            .collect();
        // The newest version's name sorts last; the newest is likeliest to be asked for.
        found.sort();
        dirs.extend(found.into_iter().rev());
    }
    dirs
}

/// What `module`'s [`RUNTIME_SECTION`] names, or `None` for a whole module.
pub(crate) fn named(module: &[u8]) -> Result<Option<Named>, ModelError> {
    let refused = |why: &str| ModelError::Load(format!("not a WebAssembly module: {why}"));
    let mut at = module
        .strip_prefix(b"\0asm")
        .and_then(|rest| rest.get(4..))
        .ok_or_else(|| refused("no header"))?;
    while let Some((&id, rest)) = at.split_first() {
        let (size, rest) = leb(rest).ok_or_else(|| refused("a section's size"))?;
        let (contents, after) = (rest.get(..size), rest.get(size..));
        let (Some(contents), Some(after)) = (contents, after) else {
            return Err(refused("a section goes past the end"));
        };
        if id == 0 {
            let (length, rest) = leb(contents).ok_or_else(|| refused("a custom section's name"))?;
            if rest.get(..length) == Some(RUNTIME_SECTION.as_bytes()) {
                let named = serde_json::from_slice(rest.get(length..).unwrap_or_default())
                    .map_err(|error| ModelError::Load(format!("{RUNTIME_SECTION}: {error}")))?;
                return Ok(Some(named));
            }
        }
        at = after;
    }
    Ok(None)
}

/// An unsigned LEB128 number of at most 32 bits, and what follows it.
fn leb(bytes: &[u8]) -> Option<(usize, &[u8])> {
    let mut value: u64 = 0;
    let mut shift = 0_u32;
    for (index, byte) in bytes.iter().take(5).enumerate() {
        value |= u64::from(byte & 0x7f) << shift;
        shift = shift.saturating_add(7);
        if byte & 0x80 == 0 {
            return Some((
                usize::try_from(value).ok()?,
                bytes.get(index.checked_add(1)?..)?,
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A module with one custom section `name` holding `data`.
    fn with_section(name: &str, data: &[u8]) -> Vec<u8> {
        let mut module = b"\0asm\x01\0\0\0".to_vec();
        let mut contents = vec![u8::try_from(name.len()).unwrap()];
        contents.extend_from_slice(name.as_bytes());
        contents.extend_from_slice(data);
        module.push(0);
        module.push(u8::try_from(contents.len()).unwrap());
        module.extend_from_slice(&contents);
        module
    }

    #[test]
    fn a_split_module_names_its_runtime_and_a_whole_one_none() {
        let split = with_section(
            RUNTIME_SECTION,
            br#"{"runtime":"abc","entry":"tilo_model_apply_x"}"#,
        );
        let found = named(&split).unwrap().unwrap();
        assert_eq!(found.runtime, "abc");
        assert_eq!(found.entry, "tilo_model_apply_x");
        assert!(named(&with_section("name", b"whatever")).unwrap().is_none());
        assert!(named(b"not wasm").is_err());
    }

    #[test]
    fn a_runtime_is_read_only_when_its_modules_are_the_ones_it_names() {
        let dir = tempfile::tempdir().unwrap();
        let modules = vec![b"\0asm\x01\0\0\0".to_vec(), b"\0asm\x01\0\0\0\0\0".to_vec()];
        std::fs::write(dir.path().join("main.wasm"), &modules[0]).unwrap();
        std::fs::write(dir.path().join("shared0.wasm"), &modules[1]).unwrap();
        let manifest = |hash: &str| {
            serde_json::json!({"schema": 1, "hash": hash, "modules": ["main.wasm", "shared0.wasm"]})
                .to_string()
        };
        std::fs::write(dir.path().join("runtime.json"), manifest(&hash(&modules))).unwrap();
        let runtime = Runtime::read(dir.path()).unwrap();
        assert_eq!(runtime.hash(), hash(&modules));
        assert_eq!(runtime.modules().count(), 2);

        std::fs::write(dir.path().join("runtime.json"), manifest("0000")).unwrap();
        let refused = Runtime::read(dir.path()).unwrap_err().to_string();
        assert!(
            refused.contains(&hash(&modules)) && refused.contains("0000"),
            "{refused}"
        );
    }
}
