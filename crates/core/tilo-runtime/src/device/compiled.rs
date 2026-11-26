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

/// The modules an installed package runs, as the host that runs it reads them: the package's own
/// ([`payload_modules`]) and the standard library of the runtime its code names, which the
/// runtime bundle under `runtimes` carries rather than the package. Each is compiled for this
/// device once, so no hand-over compiles any.
pub(crate) fn module_parts(manifest: &Path, payload: &Path, runtimes: &Path) -> Vec<Vec<u8>> {
    let mut modules = payload_modules(payload);
    let runtime = launcher::manifest::read_manifest(manifest)
        .ok()
        .and_then(|manifest| {
            launcher::manifest::code_runtime(&manifest, payload).map(str::to_owned)
        });
    if let Some(runtime) = runtime
        && let Some((_, version)) = runtime.split_once('/')
        && let Ok(std) = std::fs::read(
            runtimes
                .join(version)
                .join(runtime_portal::process_link::STD_FILE),
        )
        && !modules.contains(&std)
    {
        modules.push(std);
    }
    modules
}

/// Compile `package`'s modules into `cache`: the ones it does not hold yet, side by side. A
/// module that does not compile is said and left to run interpreted. Answers how many modules
/// the package carries and how many the cache lacked, so a caller can say what an install or an
/// open compiled.
pub(crate) fn compile_package(
    cache: &tilo_wasm_aot::Cache,
    app: &str,
    manifest: &Path,
    payload: &Path,
    runtimes: &Path,
) -> (usize, usize) {
    let modules = module_parts(manifest, payload, runtimes);
    if modules.is_empty() {
        return (0, 0);
    }
    let cold = modules
        .iter()
        .filter(|module| cache.kept(module).is_none())
        .count();
    let parts: Vec<&[u8]> = modules.iter().map(Vec::as_slice).collect();
    let began = std::time::Instant::now();
    let entries = cache.ensure_all(&parts);
    for (index, entry) in entries.iter().enumerate() {
        if let Err(error) = entry {
            tracing::warn!(
                app,
                module = index,
                %error,
                "an installed module is not compiled for this device: it runs interpreted"
            );
        }
    }
    let took_us = began.elapsed().as_micros();
    if cold == 0 {
        tracing::debug!(
            app,
            modules = parts.len(),
            took_us,
            "an installed package's modules are compiled for this device"
        );
    } else {
        tracing::info!(
            app,
            modules = parts.len(),
            cold,
            took_us,
            "an installed package's modules are compiled for this device"
        );
    }
    (parts.len(), cold)
}

/// The machine code `parts` run from, as a hand-over finds it: each part the cache holds now.
/// A part it does not hold is left empty — the host runs it on the interpreter this time — and
/// compiled on a thread of its own for the next hand-over, so no open waits for a compile.
/// Answers the entries, one per part, and how many came from the cache.
pub(crate) fn at_open(cache: &Arc<tilo_wasm_aot::Cache>, parts: &[&[u8]]) -> (Vec<Vec<u8>>, usize) {
    let entries: Vec<Vec<u8>> = parts
        .iter()
        .map(|part| cache.kept(part).unwrap_or_default())
        .collect();
    let cached = entries.iter().filter(|entry| !entry.is_empty()).count();
    if cached < parts.len() {
        let cold: Vec<Vec<u8>> = parts
            .iter()
            .zip(entries.iter())
            .filter(|(_, entry)| entry.is_empty())
            .map(|(part, _)| (*part).to_vec())
            .collect();
        let cache = Arc::clone(cache);
        let compiling = std::thread::Builder::new()
            .name("tilolang-compile".into())
            .spawn(move || {
                for part in &cold {
                    let _ = cache.ensure(part);
                }
            });
        if let Err(error) = compiling {
            tracing::warn!(%error, "no thread compiles the module this device lacks");
        }
    }
    (entries, cached)
}
