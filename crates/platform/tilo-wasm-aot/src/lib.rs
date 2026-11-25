//! Wasmtime modules compiled ahead of time for one device.
//!
//! A module is compiled once, by the device daemon, into machine code for this device, and
//! every runtime that executes it deserializes that compiled module instead of compiling it
//! again. The daemon links Cranelift (`compile`); the tilolang process, the window it draws
//! and any other runtime link wasmtime's runtime alone and only deserialize.
//!
//! Everything a compiled module depends on is part of its key, so an entry never outlives what
//! it was made by: the module's own digest, this wasmtime's version, the engine's configuration
//! and the device's target all reach the entry's name ([`Cache::name`]), and wasmtime checks
//! them again when the module is deserialized.
//!
//! An entry is sealed: the module's digest and a digest of the serialized module, then the
//! module ([`seal`]). A runtime that is handed one checks the first digest against the module
//! bytes it holds, so an artifact is only ever loaded for the module the daemon made it for,
//! and the second against its own body. The artifact travels over the daemon's own channel (a
//! sealed memfd beside the module) or is read from the cache directory the daemon owns, never
//! from a file an app can write.

#![cfg(not(target_arch = "wasm32"))]

use std::path::Path;
#[cfg(feature = "compile")]
use std::path::PathBuf;
#[cfg(feature = "compile")]
use std::sync::{Mutex, PoisonError};

/// The one engine configuration every runtime compiles or deserializes with.
///
/// Fuel is metered, calls are bounded by the engine's epoch, and no guest stack is walked for a
/// trap, as the interpreter reports traps. The strategy is left at wasmtime's default: the
/// compiler build makes that Cranelift, and the runtime build has no compiler at all. The two
/// agree on everything deserialization checks (the target, the tunables and the features), so a
/// module the daemon compiled is accepted by a runtime configured here.
#[must_use]
pub fn config() -> wasmtime::Config {
    let mut config = wasmtime::Config::new();
    config.consume_fuel(true);
    config.epoch_interruption(true);
    config.wasm_backtrace_max_frames(None);
    config
}

/// The engine the daemon compiles with.
///
/// # Errors
///
/// This build has no compiler (`compile` is off), or the engine cannot be made on this device.
#[cfg(feature = "compile")]
pub fn engine() -> Result<wasmtime::Engine, String> {
    wasmtime::Engine::new(&config()).map_err(|error| error.to_string())
}

/// The engine a runtime deserializes with: the same configuration, with no compiler linked.
///
/// # Errors
///
/// The engine cannot be made on this device.
pub fn runtime_engine() -> Result<wasmtime::Engine, String> {
    wasmtime::Engine::new(&config()).map_err(|error| error.to_string())
}

/// How many modules may compile at once, to the extent the device's cores allow.
#[cfg(feature = "compile")]
const CONCURRENT: usize = 32;

/// A valid module with nothing in it, which the cache compiles to warm the compiler.
#[cfg(feature = "compile")]
const EMPTY_MODULE: &[u8] = b"\0asm\x01\0\0\0";

/// What starts every sealed entry, before the two digests.
pub const MAGIC: &[u8; 8] = b"tlaot001";

/// The length of one digest in an entry.
pub const DIGEST: usize = 32;

/// The bytes before the serialized module: the magic and the two digests.
const HEADER: usize = MAGIC.len() + 2 * DIGEST;

/// An entry the daemon wrote: the module's digest, a digest of the serialized module, and the
/// serialized module.
#[must_use]
pub fn seal(module: &[u8], serialized: &[u8]) -> Vec<u8> {
    let mut entry = Vec::with_capacity(HEADER.saturating_add(serialized.len()));
    entry.extend_from_slice(MAGIC);
    entry.extend_from_slice(blake3::hash(module).as_bytes());
    entry.extend_from_slice(blake3::hash(serialized).as_bytes());
    entry.extend_from_slice(serialized);
    entry
}

/// What a sealed entry holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opened<'a> {
    /// The digest of the module bytes the entry was made for.
    pub module: [u8; DIGEST],
    /// The serialized module, whole and matching its digest.
    pub serialized: &'a [u8],
}

/// The entry `bytes` hold, its body checked against its digest.
///
/// # Errors
///
/// Bytes that are not a sealed entry, or whose body does not match the digest written for it.
pub fn open(bytes: &[u8]) -> Result<Opened<'_>, &'static str> {
    let rest = bytes
        .strip_prefix(MAGIC.as_slice())
        .ok_or("not a compiled module")?;
    if rest.len() < 2 * DIGEST {
        return Err("truncated");
    }
    let (module, rest) = rest.split_at(DIGEST);
    let (digest, serialized) = rest.split_at(DIGEST);
    if blake3::hash(serialized).as_bytes() != digest {
        return Err("its digest does not match");
    }
    let mut module_digest = [0; DIGEST];
    module_digest.copy_from_slice(module);
    Ok(Opened {
        module: module_digest,
        serialized,
    })
}

/// The entry `bytes` hold, checked to be the one made for `module`: the daemon wrote it for
/// these bytes and no other, and its body is whole.
///
/// # Errors
///
/// As [`open`], or an artifact the daemon made for a different module.
pub fn open_for<'a>(bytes: &'a [u8], module: &[u8]) -> Result<&'a [u8], String> {
    let opened = open(bytes).map_err(str::to_owned)?;
    if opened.module != *blake3::hash(module).as_bytes() {
        return Err("the compiled module was made for another module".to_owned());
    }
    Ok(opened.serialized)
}

/// The entry `dir` holds for `module`, without compiling: what a runtime reads from the store the
/// daemon owns, which it is granted read-only. `None` when no entry there is the one made for
/// these bytes.
///
/// This is a lookup, never a compile: a process that runs a module reads the machine code the
/// device's daemon made for it, and a module the daemon has not compiled runs on the interpreter
/// where it is used.
#[must_use]
pub fn kept_in(dir: &Path, module: &[u8]) -> Option<Vec<u8>> {
    let prefix = format!("{}-", blake3::hash(module).to_hex());
    for entry in std::fs::read_dir(dir).ok()? {
        let path = entry.ok()?.path();
        let name = path.file_name()?.to_str()?;
        if !name.starts_with(&prefix)
            || !path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("cwasm"))
        {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if open_for(&bytes, module).is_ok() {
            return Some(bytes);
        }
    }
    None
}

/// A device's compiled modules: `<dir>/<module hash>-<engine>.cwasm`, where `<engine>` stands
/// for this wasmtime's version, configuration and target, as wasmtime's own compatibility hash
/// writes them.
///
/// One engine serves every compile, so a module is laid out and optimized on one warmed
/// compiler rather than one made per module.
#[cfg(feature = "compile")]
pub struct Cache {
    dir: PathBuf,
    engine: wasmtime::Engine,
    engine_name: String,
    /// One compile at a time per module, in stripes: two callers asking for the same new module
    /// wait for the first rather than compile it twice, and different modules compile side by
    /// side.
    compiling: Vec<Mutex<()>>,
}

#[cfg(feature = "compile")]
impl Cache {
    /// The cache in `dir`, on one engine for the life of it.
    ///
    /// The compiler is warmed here with an empty module, so the first module a person changes
    /// pays for its own compile and not for the compiler's first use.
    ///
    /// # Errors
    ///
    /// The engine cannot be made on this device.
    pub fn at(dir: impl Into<PathBuf>) -> Result<Self, String> {
        let engine = engine()?;
        // Cranelift lays out its instruction selector and allocator on first use; a daemon
        // starts once and then compiles the modules people open, so that one-time cost is
        // paid here, where no frame waits on it.
        let _ = wasmtime::Module::new(&engine, EMPTY_MODULE);
        let engine_name = engine_name(&engine);
        Ok(Self {
            dir: dir.into(),
            engine,
            engine_name,
            compiling: (0..CONCURRENT).map(|_| Mutex::new(())).collect(),
        })
    }

    /// Where this cache is.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// An entry's place, named by the module's digest and this engine.
    #[must_use]
    pub fn name(&self, module: &[u8]) -> String {
        format!(
            "{}-{}.cwasm",
            blake3::hash(module).to_hex(),
            self.engine_name
        )
    }

    /// The stripe a module's compile takes: modules that share it compile one at a time.
    fn stripe(&self, module: &[u8]) -> Option<&Mutex<()>> {
        let first = blake3::hash(module)
            .as_bytes()
            .first()
            .copied()
            .unwrap_or(0);
        let at = usize::from(first)
            .checked_rem(self.compiling.len())
            .unwrap_or(0);
        self.compiling.get(at)
    }

    /// The compiled entry for `module`: from the cache when it holds one, else compiled now.
    ///
    /// # Errors
    ///
    /// The module does not compile, or the cache cannot be written. A module that fails to
    /// compile is reported, never run here: the compiler is the daemon's.
    pub fn ensure(&self, module: &[u8]) -> Result<Vec<u8>, String> {
        let path = self.dir.join(self.name(module));
        if let Ok(bytes) = std::fs::read(&path)
            && open_for(&bytes, module).is_ok()
        {
            return Ok(bytes);
        }
        // One module compiles once: a caller that arrives while it compiles waits, then reads
        // the entry the first wrote.
        let _turn = self
            .stripe(module)
            .map(|stripe| stripe.lock().unwrap_or_else(PoisonError::into_inner));
        if let Ok(bytes) = std::fs::read(&path)
            && open_for(&bytes, module).is_ok()
        {
            return Ok(bytes);
        }
        let compiled = wasmtime::Module::new(&self.engine, module)
            .map_err(|error| format!("the module does not compile: {error}"))?;
        let serialized = compiled
            .serialize()
            .map_err(|error| format!("the compiled module is not writable: {error}"))?;
        let entry = seal(module, &serialized);
        std::fs::create_dir_all(&self.dir)
            .map_err(|error| format!("{}: {error}", self.dir.display()))?;
        let mut written = tempfile::NamedTempFile::new_in(&self.dir)
            .map_err(|error| format!("{}: {error}", self.dir.display()))?;
        std::io::Write::write_all(&mut written, &entry)
            .and_then(|()| written.as_file().sync_all())
            .map_err(|error| format!("{}: {error}", path.display()))?;
        written
            .persist(&path)
            .map_err(|error| format!("{}: {}", path.display(), error.error))?;
        Ok(entry)
    }

    /// The compiled entry for each of `parts`, compiled in parallel: a package's module and the
    /// modules it uses are independent, and a device has the cores.
    #[must_use]
    pub fn ensure_all(&self, parts: &[&[u8]]) -> Vec<Result<Vec<u8>, String>> {
        if parts.len() <= 1 {
            return parts.iter().map(|part| self.ensure(part)).collect();
        }
        std::thread::scope(|scope| {
            // Every compile starts before the first is joined, so they run at once.
            let mut waiting = Vec::with_capacity(parts.len());
            for part in parts {
                waiting.push(scope.spawn(move || self.ensure(part)));
            }
            waiting
                .into_iter()
                .map(|spawned| {
                    spawned
                        .join()
                        .unwrap_or_else(|_| Err("a compile thread ended".to_owned()))
                })
                .collect()
        })
    }

    /// The entry for `module` in the cache, without compiling it.
    #[must_use]
    pub fn kept(&self, module: &[u8]) -> Option<Vec<u8>> {
        let bytes = std::fs::read(self.dir.join(self.name(module))).ok()?;
        open_for(&bytes, module).ok()?;
        Some(bytes)
    }
}

/// The name of `engine` among cache entries: what a serialized module was made by, as wasmtime
/// reads it back.
#[cfg(feature = "compile")]
fn engine_name(engine: &wasmtime::Engine) -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    engine.precompile_compatibility_hash().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests;
