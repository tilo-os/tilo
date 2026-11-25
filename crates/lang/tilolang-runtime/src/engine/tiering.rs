//! The compiled tier ([`super::EngineKind::Tiered`]): modules the device daemon compiled ahead
//! of time, deserialized here and run between calls.
//!
//! * A runtime links wasmtime's runtime alone: it has no compiler. The daemon compiles each
//!   module for this device, and the process is handed the artifact beside the module
//!   (`tilo-wasm-aot`). Which module an artifact was made for, and whether wasmtime accepts it,
//!   are checked before it is used ([`super::adopt_precompiled`]); an artifact that is not is
//!   never loaded.
//! * A module with an artifact starts compiled. A module without one runs on wasmi, the
//!   interpreter, and stays there: nothing is compiled in this process. That is what an
//!   embedded module (the standard library, a block written at run time) runs as on a device
//!   whose daemon has not compiled it, and what a test that hands no artifact sees.
//! * Compiled modules are kept per process by content hash ([`Compiler`]), so a module shared
//!   by several runtimes is deserialized once.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::Compiler;
use super::wasmtime_backend::Wasmtime;
use crate::ids::ModuleHash;

/// The process's compiled tier: the compiled modules the daemon produced, and why one that was
/// handed here could not be used.
pub(super) struct Tiering {
    pub(super) compiler: Compiler<Wasmtime>,
    /// Modules whose artifact was refused, and why; they stay on wasmi.
    failed: Mutex<HashMap<ModuleHash, String>>,
}

impl Tiering {
    pub(super) fn new(backend: Wasmtime) -> Self {
        Self {
            compiler: Compiler::new(backend),
            failed: Mutex::new(HashMap::new()),
        }
    }

    fn failed(&self) -> MutexGuard<'_, HashMap<ModuleHash, String>> {
        self.failed.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The compiled module for `bytes`, when the daemon made one: kept here already, handed to
    /// the process (`super::adopt_precompiled`), or in the device's cache `dir`.
    ///
    /// Every entry is bound to these bytes, and wasmtime checks the version, configuration and
    /// target as it deserializes. Nothing here compiles: with no entry the module runs on the
    /// interpreter.
    pub(super) fn adopt(
        &self,
        hash: &ModuleHash,
        bytes: &[u8],
        dir: Option<&std::path::Path>,
    ) -> Option<wasmtime::Module> {
        if let Some(module) = self.compiler.known(hash) {
            return Some(module);
        }
        if let Some(serialized) = super::precompiled_for(hash) {
            match self.compiler.backend.deserialize(&serialized) {
                Ok(module) => {
                    self.compiler.keep(hash.clone(), module.clone());
                    return Some(module);
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "a compiled module handed to this process is for another build: the \
                         module runs interpreted"
                    );
                    self.failed().insert(hash.clone(), error);
                    return None;
                }
            }
        }
        dir.and_then(|dir| self.kept(hash, bytes, dir))
    }

    /// The artifact the daemon compiled for `bytes` in `dir`, which devices and app sandboxes are
    /// granted read-only: `<module hash>-<engine>.cwasm`, sealed by `tilo-wasm-aot`.
    ///
    /// The entry is checked to be the one made for these bytes, and wasmtime checks the version,
    /// configuration and target as it deserializes. Nothing here compiles: an entry that is not
    /// there, not for this module, or not for this build is passed over and the module runs on
    /// the interpreter.
    pub(super) fn kept(
        &self,
        hash: &ModuleHash,
        bytes: &[u8],
        dir: &std::path::Path,
    ) -> Option<wasmtime::Module> {
        let prefix = format!("{hash}-");
        for entry in std::fs::read_dir(dir).ok()? {
            let path = entry.ok()?.path();
            let name = path.file_name()?.to_str()?;
            if !name.starts_with(&prefix)
                || path
                    .extension()
                    .is_none_or(|extension| extension != "cwasm")
            {
                continue;
            }
            let Ok(entry) = std::fs::read(&path) else {
                continue;
            };
            let Ok(serialized) = tilo_wasm_aot::open_for(&entry, bytes) else {
                tracing::warn!(
                    path = %path.display(),
                    "a compiled module is not the one this module's daemon made: it runs \
                     interpreted"
                );
                continue;
            };
            match self.compiler.backend.deserialize(serialized) {
                Ok(module) => {
                    self.compiler.keep(hash.clone(), module.clone());
                    return Some(module);
                }
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "a compiled module is for another build: it runs interpreted"
                    );
                }
            }
        }
        None
    }

    /// The compiled modules could not be instantiated together: the module runs on the
    /// interpreter, and why is kept.
    pub(super) fn reject(&self, hash: &ModuleHash, why: String) {
        self.failed().insert(hash.clone(), why);
    }

    /// Why the module's artifact could not be used, if it could not.
    pub(super) fn failure(&self, hash: &ModuleHash) -> Option<String> {
        self.failed().get(hash).cloned()
    }
}
