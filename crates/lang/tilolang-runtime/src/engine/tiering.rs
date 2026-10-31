//! Tiered execution ([`super::EngineKind::Tiered`]): a module runs on wasmi from the moment it
//! is loaded while Cranelift compiles it on a thread of its own; calls move to the compiled
//! module once it is ready.
//!
//! * The swap happens between calls. Guests keep no state between calls and instances are
//!   interchangeable ([`super`]), so the next call simply checks out an instance of the compiled
//!   module instead of the interpreted one. A call that is running when the compile finishes
//!   ends on wasmi; nothing waits for anything.
//! * Compiled modules are kept per process by content hash ([`Compiler`]), and on disk in the
//!   directory [`super::keep_compiled_in`] names, so the next process to load a module starts on
//!   compiled code. An entry is the serialized module behind a digest of it; one that does not
//!   match its digest, or that this wasmtime cannot load (another version, configuration or
//!   CPU), is not loaded and the module is compiled again.
//! * One compile thread, started with the first module, takes modules in the order they were
//!   loaded. A module that fails to compile stays on wasmi; why is logged and kept
//!   ([`Tiering::failure`]).

use std::collections::{HashMap, HashSet};
use std::hash::{Hash as _, Hasher as _};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock, PoisonError};

use super::wasmtime_backend::Wasmtime;
use super::{Backend as _, Compiler, EngineKind};
use crate::ids::ModuleHash;

/// What a compile thread is asked to do.
struct Job {
    hash: ModuleHash,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct State {
    /// Modules waiting for or in their compile.
    pending: HashSet<ModuleHash>,
    /// Modules whose compile failed, and why.
    failed: HashMap<ModuleHash, String>,
    /// Compiles wait while this is set ([`Tiering::pause`]).
    paused: bool,
}

/// The process's compiled tier: Cranelift, its compiled modules, and the thread compiling them.
pub(super) struct Tiering {
    pub(super) compiler: Compiler<Wasmtime>,
    state: Mutex<State>,
    /// Signalled whenever [`State`] changes.
    changed: Condvar,
    /// The compile thread's queue, made with the first job.
    jobs: OnceLock<Mutex<Sender<Job>>>,
    /// The on-disk cache, read when first needed from [`super::keep_compiled_in`].
    disk: OnceLock<Option<DiskCache>>,
    /// Where the disk cache is, before it is first read.
    disk_dir: Mutex<Option<PathBuf>>,
}

impl Tiering {
    pub(super) fn new() -> Result<Self, String> {
        Ok(Self {
            compiler: Compiler::new(Wasmtime::new(EngineKind::Cranelift)?),
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            jobs: OnceLock::new(),
            disk: OnceLock::new(),
            disk_dir: Mutex::new(None),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keep compiled modules in `dir`, from now on. Fails once the cache was read.
    pub(super) fn keep_in(&self, dir: PathBuf) -> Result<(), String> {
        if let Some(disk) = self.disk.get() {
            return match disk {
                Some(cache) if cache.dir == dir => Ok(()),
                _ => Err(format!(
                    "compiled modules are already kept {}; too late to keep them in {}",
                    disk.as_ref().map_or_else(
                        || "in memory only".to_owned(),
                        |c| c.dir.display().to_string()
                    ),
                    dir.display()
                )),
            };
        }
        *self.disk_dir.lock().unwrap_or_else(PoisonError::into_inner) = Some(dir);
        Ok(())
    }

    fn disk(&self) -> Option<&DiskCache> {
        self.disk
            .get_or_init(|| {
                let dir = self
                    .disk_dir
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()?;
                Some(DiskCache::new(dir, &self.compiler.backend))
            })
            .as_ref()
    }

    /// The compiled module, if this process has it or the disk cache holds it.
    pub(super) fn ready(&self, hash: &ModuleHash) -> Option<wasmtime::Module> {
        if let Some(module) = self.compiler.known(hash) {
            return Some(module);
        }
        let module = self.disk()?.load(&self.compiler.backend, hash)?;
        self.compiler.keep(hash.clone(), module.clone());
        Some(module)
    }

    /// The module this process has compiled, without looking on disk.
    pub(super) fn compiled(&self, hash: &ModuleHash) -> Option<wasmtime::Module> {
        self.compiler.known(hash)
    }

    /// Compile the module in the background, unless it is compiled, being compiled, or failed.
    pub(super) fn request(&'static self, hash: &ModuleHash, bytes: &[u8]) {
        {
            let mut state = self.state();
            if state.pending.contains(hash)
                || state.failed.contains_key(hash)
                || self.compiler.known(hash).is_some()
            {
                return;
            }
            state.pending.insert(hash.clone());
        }
        let job = Job {
            hash: hash.clone(),
            bytes: bytes.to_vec(),
        };
        let jobs = self.jobs.get_or_init(|| Mutex::new(self.start()));
        let sent = jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .send(job);
        if let Err(unsent) = sent {
            // The thread could not be started or ended: the module stays on wasmi, and why is
            // kept.
            self.finish(
                &unsent.0.hash,
                Err("the compile thread is not running".into()),
            );
        }
    }

    /// The compile thread, and its queue.
    fn start(&'static self) -> Sender<Job> {
        let (send, receive) = channel();
        let started = std::thread::Builder::new()
            .name("tilolang-compile".into())
            .spawn(move || self.work(&receive));
        if let Err(error) = started {
            tracing::warn!(%error, "no thread to compile tilolang modules: they stay on wasmi");
            // The receiver is dropped with the closure, so every send fails and says so.
        }
        send
    }

    fn work(&self, jobs: &Receiver<Job>) {
        while let Ok(job) = jobs.recv() {
            drop(
                self.changed
                    .wait_while(self.state(), |state| state.paused)
                    .unwrap_or_else(PoisonError::into_inner),
            );
            let outcome = self.compiler.backend.compile(&job.bytes);
            if let Ok(module) = &outcome {
                self.compiler.keep(job.hash.clone(), module.clone());
                if let Some(disk) = self.disk()
                    && let Err(error) = disk.store(&job.hash, module)
                {
                    tracing::warn!(
                        module = %job.hash,
                        %error,
                        "a compiled tilolang module was not kept on disk"
                    );
                }
            }
            self.finish(&job.hash, outcome.map(drop));
        }
    }

    fn finish(&self, hash: &ModuleHash, outcome: Result<(), String>) {
        let mut state = self.state();
        state.pending.remove(hash);
        if let Err(why) = outcome {
            tracing::warn!(module = %hash, %why, "a tilolang module stays on wasmi");
            state.failed.insert(hash.clone(), why);
        }
        drop(state);
        self.changed.notify_all();
    }

    /// The compiled module could not be instantiated: it is dropped, and its module stays on
    /// wasmi.
    pub(super) fn reject(&self, hash: &ModuleHash, why: String) {
        self.compiler.drop_module(hash);
        self.finish(hash, Err(why));
    }

    /// Forget the modules compiled in this process, and why any failed.
    pub(super) fn forget(&self) {
        self.compiler.forget();
        self.state().failed.clear();
    }

    /// Why the module's compile failed, if it did.
    pub(super) fn failure(&self, hash: &ModuleHash) -> Option<String> {
        self.state().failed.get(hash).cloned()
    }

    /// Hold compiles from starting (`true`) or let them run again: how tests catch a module
    /// before its compile.
    #[cfg(test)]
    pub(super) fn pause(&self, paused: bool) {
        self.state().paused = paused;
        self.changed.notify_all();
    }

    /// Wait until the module is no longer waiting for its compile: ended by the compile thread
    /// finishing it, either way. Returns at once for a module never requested.
    pub(super) fn settle(&self, hash: &ModuleHash) {
        drop(
            self.changed
                .wait_while(self.state(), |state| state.pending.contains(hash))
                .unwrap_or_else(PoisonError::into_inner),
        );
    }
}

/// Compiled modules on disk: `<dir>/<module hash>-<engine>.cwasm`, where `<engine>` stands
/// for this wasmtime's version, configuration and CPU features. A file is a digest of the
/// serialized module, then the module.
struct DiskCache {
    dir: PathBuf,
    engine: String,
}

/// What starts every entry, before its digest.
const MAGIC: &[u8; 8] = b"tlcwasm1";
const DIGEST: usize = 32;

impl DiskCache {
    fn new(dir: PathBuf, backend: &Wasmtime) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        backend
            .engine()
            .precompile_compatibility_hash()
            .hash(&mut hasher);
        Self {
            dir,
            engine: format!("{:016x}", hasher.finish()),
        }
    }

    fn path(&self, hash: &ModuleHash) -> PathBuf {
        self.dir.join(format!("{hash}-{}.cwasm", self.engine))
    }

    /// The module kept for `hash`, or nothing: no entry, or one that is damaged or for another
    /// engine, which is said on the log and then compiled again.
    fn load(&self, backend: &Wasmtime, hash: &ModuleHash) -> Option<wasmtime::Module> {
        let path = self.path(hash);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "a compiled module could not be read");
                return None;
            }
        };
        match verified(&bytes) {
            Ok(serialized) => match backend.deserialize(serialized) {
                Ok(module) => Some(module),
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "a compiled module is for another engine; compiling again");
                    None
                }
            },
            Err(why) => {
                tracing::warn!(path = %path.display(), why, "a compiled module is damaged; compiling again");
                None
            }
        }
    }

    /// Keep `module` for `hash`: written beside its place and renamed into it, so a reader
    /// sees a whole entry or none, and two processes keeping the same module both succeed.
    fn store(&self, hash: &ModuleHash, module: &wasmtime::Module) -> Result<(), String> {
        let serialized = module.serialize().map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let mut entry = Vec::new();
        entry.extend_from_slice(MAGIC);
        entry.extend_from_slice(blake3::hash(&serialized).as_bytes());
        entry.extend_from_slice(&serialized);
        let path = self.path(hash);
        let mut partial = tempfile::Builder::new()
            .prefix(".partial-")
            .tempfile_in(&self.dir)
            .map_err(|e| format!("{}: {e}", self.dir.display()))?;
        std::io::Write::write_all(&mut partial, &entry)
            .and_then(|()| partial.as_file().sync_all())
            .map_err(|e| format!("{}: {e}", partial.path().display()))?;
        partial
            .persist(&path)
            .map_err(|e| format!("{}: {}", path.display(), e.error))?;
        Ok(())
    }
}

/// The serialized module of an entry whose digest matches it.
fn verified(entry: &[u8]) -> Result<&[u8], &'static str> {
    let rest = entry.strip_prefix(MAGIC.as_slice()).ok_or("not an entry")?;
    if rest.len() < DIGEST {
        return Err("truncated");
    }
    let (digest, serialized) = rest.split_at(DIGEST);
    if blake3::hash(serialized).as_bytes() != digest {
        return Err("its digest does not match");
    }
    Ok(serialized)
}

/// The disk cache's entry for `hash` in `dir`, if there is one.
#[cfg(test)]
pub(super) fn entry(dir: &std::path::Path, hash: &ModuleHash) -> Option<PathBuf> {
    let prefix = format!("{hash}-");
    std::fs::read_dir(dir).ok()?.find_map(|e| {
        let path = e.ok()?.path();
        let name = path.file_name()?.to_str()?;
        (name.starts_with(&prefix) && path.extension().is_some_and(|e| e == "cwasm"))
            .then(|| path.clone())
    })
}
