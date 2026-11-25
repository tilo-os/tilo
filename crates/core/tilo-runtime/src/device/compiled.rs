//! The device's one wasm compiler: the cache of modules compiled ahead of time, which every part
//! of the daemon compiles on and every runtime reads.
//!
//! A module a device runs is compiled once, here, and never by the program that runs it
//! (`tilo-wasm-aot`). The store is the one the launcher grants read-only to app sandboxes, under
//! the device's data home, so a program reads the machine code its daemon made and nothing else
//! writes it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use tilo_lock::MutexRecover as _;

/// The directory, under the device's data home, the compiled modules are kept in: the one the
/// launcher mounts read-only for the processes that run them.
pub(crate) const DIR: &str = "tilolang-compiled";

/// The compiler for the device whose data home is `data_home`, made once for the life of the
/// process: an install, a handover and a task's catalogue all compile on the same warm engine
/// and into the same store. `None` when this build has no compiler, or the device cannot make
/// the engine; every module then runs on the interpreter.
pub(crate) fn compiler_in(data_home: &Path) -> Option<Arc<tilo_wasm_aot::Cache>> {
    static COMPILERS: OnceLock<Mutex<BTreeMap<PathBuf, Option<Arc<tilo_wasm_aot::Cache>>>>> =
        OnceLock::new();
    let dir = data_home.join(DIR);
    let mut compilers = COMPILERS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock_recover();
    compilers
        .entry(dir.clone())
        .or_insert_with(|| match tilo_wasm_aot::Cache::at(&dir) {
            Ok(cache) => Some(Arc::new(cache)),
            Err(error) => {
                tracing::warn!(
                    %error,
                    dir = %dir.display(),
                    "this device compiles no tilolang module: every module runs on the interpreter"
                );
                None
            }
        })
        .clone()
}

/// The compiler of this device: the data home the launcher mounts the compiled modules from.
pub(crate) fn compiler() -> Option<Arc<tilo_wasm_aot::Cache>> {
    compiler_in(&tilo_paths::data_dir()?)
}

/// The modules an installed package's payload at `payload` runs: the tilolang module and the
/// modules it links (`app.wasm`, `imports/*.wasm`), and the painter modules a theme bundle
/// ships beside its document (`<set>.wasm` at the payload's root). Each is compiled by the
/// daemon before anything runs it.
pub(crate) fn payload_modules(payload: &Path) -> Vec<Vec<u8>> {
    let mut modules = Vec::new();
    if let Ok(wasm) = std::fs::read(payload.join("app.wasm")) {
        modules.push(wasm);
    }
    let mut imported: Vec<PathBuf> = match std::fs::read_dir(payload.join("imports")) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("wasm"))
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    imported.sort();
    modules.extend(imported.iter().filter_map(|path| std::fs::read(path).ok()));
    // A theme bundle's document and its sets share the payload's root: a set is a module the
    // toolkit loads to draw the theme, and it compiles like any other.
    if payload.join("theme.json").is_file()
        && let Ok(entries) = std::fs::read_dir(payload)
    {
        let mut sets: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("wasm"))
                    && path.file_name().is_none_or(|name| name != "app.wasm")
            })
            .collect();
        sets.sort();
        modules.extend(sets.iter().filter_map(|path| std::fs::read(path).ok()));
    }
    modules
}
