//! Provider runner. Loads one core WebAssembly module per registered
//! provider, checks it against the `format-provider-abi` contract once, and
//! runs detect / import / export in a fresh wasmi instance per call.
//!
//! Resource caps, applied per call:
//! * **Memory** — the store's limiter refuses linear-memory growth past
//!   [`Limits::max_memory_bytes`] (default 256 MiB) and traps, so the guest
//!   cannot mask the refusal.
//! * **CPU** — the store starts with [`Limits::fuel`] units of wasmi fuel.
//!   Fuel is deterministic, so a runaway guest stops after the same amount of
//!   work on every machine instead of after a wall-clock deadline.
//!
//! Providers get no WASI and may import nothing except
//! `tilo_fp.fill_random`.

use std::path::Path;
use std::sync::Arc;

use format_provider_abi::{
    ABI_VERSION, CodecError, Detected, decode_detected, decode_result, exports, imports,
    unpack_region,
};
use wasmi::{
    Caller, Config, Engine, Extern, ExternType, FuncType, Instance, Linker, Module, Store,
    StoreLimits, StoreLimitsBuilder, TrapCode, ValType,
};

/// Default linear-memory ceiling per call.
pub const MAX_MEMORY_BYTES: usize = 256 * 1024 * 1024;

/// Default fuel per call. Importing 1 MB of Markdown with the builtin provider
/// consumes about 220 billion units, so it fits with roughly 2x headroom;
/// wasmi burns on the order of a billion units per second.
pub const DEFAULT_FUEL: u64 = 500_000_000_000;

/// Largest provider module the host will read.
const MAX_MODULE_BYTES: u64 = 64 * 1024 * 1024;

/// Per-call resource budget.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_memory_bytes: usize,
    pub fuel: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_memory_bytes: MAX_MEMORY_BYTES,
            fuel: DEFAULT_FUEL,
        }
    }
}

struct HostState {
    limits: StoreLimits,
}

/// A validated provider module, cheap to clone.
#[derive(Clone)]
pub struct LoadedProvider {
    module: Module,
    linker: Arc<Linker<HostState>>,
    limits: Limits,
}

/// Build an engine with fuel metering, shared by every provider.
pub fn engine() -> Engine {
    let mut cfg = Config::default();
    cfg.consume_fuel(true);
    Engine::new(&cfg)
}

impl LoadedProvider {
    /// Load a provider module from a `.wasm` file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read, is too large, is not a
    /// valid WebAssembly module, or does not implement the provider ABI.
    pub fn from_path(engine: &Engine, path: &Path, limits: Limits) -> anyhow::Result<Self> {
        let size = std::fs::metadata(path)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?
            .len();
        anyhow::ensure!(
            size <= MAX_MODULE_BYTES,
            "{} is {size} bytes; providers are limited to {MAX_MODULE_BYTES}",
            path.display()
        );
        let bytes =
            std::fs::read(path).map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
        Self::from_bytes(engine, &bytes, limits)
            .map_err(|e| anyhow::anyhow!("loading {}: {e:#}", path.display()))
    }

    /// Load a provider module from in-memory bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the bytes are not a valid WebAssembly module or do
    /// not implement the provider ABI.
    pub fn from_bytes(engine: &Engine, bytes: &[u8], limits: Limits) -> anyhow::Result<Self> {
        let module =
            Module::new(engine, bytes).map_err(|e| anyhow::anyhow!("invalid module: {e}"))?;
        check_interface(&module)?;
        let mut linker = Linker::<HostState>::new(engine);
        linker
            .func_wrap(imports::MODULE, imports::FILL_RANDOM, fill_random)
            .map_err(|e| anyhow::anyhow!("defining {}: {e}", imports::FILL_RANDOM))?;
        let provider = Self {
            module,
            linker: Arc::new(linker),
            limits,
        };
        let mut store = provider.new_store();
        let instance = provider
            .instantiate(&mut store)
            .map_err(|e| anyhow::anyhow!("instantiating: {e}"))?;
        let version = instance
            .get_typed_func::<(), i32>(&store, exports::ABI_VERSION)
            .and_then(|f| f.call(&mut store, ()))
            .map_err(|e| anyhow::anyhow!("calling {}: {e}", exports::ABI_VERSION))?;
        anyhow::ensure!(
            version == ABI_VERSION,
            "provider speaks ABI version {version}; this host supports {ABI_VERSION}"
        );
        Ok(provider)
    }

    /// The per-call budget this provider runs under.
    pub fn limits(&self) -> Limits {
        self.limits
    }

    fn new_store(&self) -> Store<HostState> {
        let limits = StoreLimitsBuilder::new()
            .memory_size(self.limits.max_memory_bytes)
            .instances(1)
            .memories(1)
            .tables(1)
            .trap_on_grow_failure(true)
            .build();
        let mut store = Store::new(self.module.engine(), HostState { limits });
        store.limiter(|s| &mut s.limits);
        // Fuel metering is always enabled by `engine()`; a store from any
        // other engine is rejected when the module is instantiated.
        let _ = store.set_fuel(self.limits.fuel);
        store
    }

    fn instantiate(&self, store: &mut Store<HostState>) -> Result<Instance, wasmi::Error> {
        self.linker.instantiate_and_start(store, &self.module)
    }

    /// Run one entry point in a fresh instance and return its response bytes.
    fn call(&self, entry: &str, input: &[u8]) -> Result<Vec<u8>, CodecError> {
        let mut store = self.new_store();
        let instance = self.instantiate(&mut store).map_err(|e| classify(&e))?;
        let memory = instance
            .get_memory(&store, exports::MEMORY)
            .ok_or_else(|| CodecError::trap("provider exports no memory"))?;
        let len = u32::try_from(input.len())
            .map_err(|_| CodecError::resource_limit("input exceeds 4 GiB"))?;
        let ptr = instance
            .get_typed_func::<u32, u32>(&store, exports::ALLOC)
            .and_then(|f| f.call(&mut store, len))
            .map_err(|e| classify(&e))?;
        region(memory.data_mut(&mut store), ptr, len)
            .ok_or_else(|| CodecError::trap("provider allocated an out-of-bounds input region"))?
            .copy_from_slice(input);
        let packed = instance
            .get_typed_func::<(u32, u32), u64>(&store, entry)
            .and_then(|f| f.call(&mut store, (ptr, len)))
            .map_err(|e| classify(&e))?;
        let (out_ptr, out_len) = unpack_region(packed);
        region(memory.data_mut(&mut store), out_ptr, out_len)
            .map(|r| r.to_vec())
            .ok_or_else(|| CodecError::trap("provider returned an out-of-bounds response"))
    }

    /// Run `detect` on a file head.
    ///
    /// # Errors
    ///
    /// Returns a `CodecError` if the guest traps, exceeds its limits, or
    /// returns a malformed response.
    pub fn detect(&self, head: &[u8]) -> Result<Detected, CodecError> {
        let response = self.call(exports::DETECT, head)?;
        decode_detected(&response).map_err(|e| CodecError::trap(e.to_string()))
    }

    /// Run `import`. Returns the canonical CRDT bytes.
    ///
    /// # Errors
    ///
    /// Returns the guest's `CodecError`, or one describing a trap, exhausted
    /// limit, or malformed response.
    pub fn import_doc(&self, bytes: &[u8]) -> Result<Vec<u8>, CodecError> {
        let response = self.call(exports::IMPORT, bytes)?;
        decode_result(response).map_err(|e| CodecError::trap(e.to_string()))?
    }

    /// Run `export`. Returns the file-format bytes.
    ///
    /// # Errors
    ///
    /// Returns the guest's `CodecError`, or one describing a trap, exhausted
    /// limit, or malformed response.
    pub fn export_doc(&self, canonical: &[u8]) -> Result<Vec<u8>, CodecError> {
        let response = self.call(exports::EXPORT, canonical)?;
        decode_result(response).map_err(|e| CodecError::trap(e.to_string()))?
    }
}

/// Host side of `tilo_fp.fill_random`: write OS entropy into guest memory.
fn fill_random(mut caller: Caller<'_, HostState>, ptr: u32, len: u32) -> Result<(), wasmi::Error> {
    let Some(Extern::Memory(memory)) = caller.get_export(exports::MEMORY) else {
        return Err(wasmi::Error::new("fill_random: provider exports no memory"));
    };
    let dest = region(memory.data_mut(&mut caller), ptr, len)
        .ok_or_else(|| wasmi::Error::from(TrapCode::MemoryOutOfBounds))?;
    getrandom::fill(dest).map_err(|e| wasmi::Error::new(format!("fill_random: {e}")))
}

/// The guest memory region `ptr..ptr + len`, if it is in bounds.
fn region(memory: &mut [u8], ptr: u32, len: u32) -> Option<&mut [u8]> {
    let start = usize::try_from(ptr).ok()?;
    let end = start.checked_add(usize::try_from(len).ok()?)?;
    memory.get_mut(start..end)
}

/// Reject modules that do not match the ABI before running any guest code.
fn check_interface(module: &Module) -> anyhow::Result<()> {
    use ValType::{I32, I64};
    let required: [(&str, &[ValType], &[ValType]); 5] = [
        (exports::ABI_VERSION, &[], &[I32]),
        (exports::ALLOC, &[I32], &[I32]),
        (exports::DETECT, &[I32, I32], &[I64]),
        (exports::IMPORT, &[I32, I32], &[I64]),
        (exports::EXPORT, &[I32, I32], &[I64]),
    ];
    for (name, params, results) in required {
        match module.get_export(name) {
            Some(ExternType::Func(ty))
                if ty == FuncType::new(params.iter().copied(), results.iter().copied()) => {}
            Some(ExternType::Func(ty)) => {
                anyhow::bail!(
                    "export {name} has signature {ty:?}, expected {params:?} -> {results:?}"
                )
            }
            Some(_) => anyhow::bail!("export {name} is not a function"),
            None => anyhow::bail!("missing export {name}"),
        }
    }
    anyhow::ensure!(
        matches!(
            module.get_export(exports::MEMORY),
            Some(ExternType::Memory(_))
        ),
        "missing memory export `{}`",
        exports::MEMORY
    );
    for import in module.imports() {
        anyhow::ensure!(
            import.module() == imports::MODULE && import.name() == imports::FILL_RANDOM,
            "provider imports {}::{}; only {}::{} is available",
            import.module(),
            import.name(),
            imports::MODULE,
            imports::FILL_RANDOM
        );
    }
    Ok(())
}

/// Map a wasmi error to a structured `CodecError`. Exhausted fuel and refused
/// memory growth are resource limits; everything else is a guest trap. Either
/// way, the host stays up.
fn classify(e: &wasmi::Error) -> CodecError {
    match e.as_trap_code() {
        Some(TrapCode::OutOfFuel) => {
            CodecError::resource_limit(format!("fuel budget exhausted: {e}"))
        }
        Some(TrapCode::GrowthOperationLimited) => {
            CodecError::resource_limit(format!("memory cap hit: {e}"))
        }
        Some(TrapCode::StackOverflow) => {
            CodecError::resource_limit(format!("stack exhausted: {e}"))
        }
        _ => CodecError::trap(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"(module
        (memory (export "memory") 1)
        (func (export "tilo_fp_abi_version") (result i32) i32.const 1)
        (func (export "tilo_fp_alloc") (param i32) (result i32) i32.const 1024)
        (func (export "tilo_fp_detect") (param i32 i32) (result i64) i64.const 0)
        (func (export "tilo_fp_import") (param i32 i32) (result i64) i64.const 0)
        (func (export "tilo_fp_export") (param i32 i32) (result i64) i64.const 0))"#;

    fn load(wat: &str) -> anyhow::Result<LoadedProvider> {
        let wasm = wat::parse_str(wat)?;
        LoadedProvider::from_bytes(&engine(), &wasm, Limits::default())
    }

    #[test]
    fn provider_modules_are_validated_at_load_and_bounded_at_call() {
        load(MINIMAL).unwrap();

        let detect = r#"(func (export "tilo_fp_detect") (param i32 i32) (result i64) i64.const 0)"#;
        for (name, from, to, want) in [
            (
                "missing export",
                "tilo_fp_export",
                "something_else",
                "missing export tilo_fp_export",
            ),
            (
                "wrong signature",
                detect,
                r#"(func (export "tilo_fp_detect") (param i32) (result i64) i64.const 0)"#,
                "tilo_fp_detect has signature",
            ),
            (
                "foreign imports",
                "(memory",
                r#"(import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32))) (memory"#,
                "wasi_snapshot_preview1::fd_write",
            ),
            (
                "other ABI version",
                "(result i32) i32.const 1)",
                "(result i32) i32.const 2)",
                "ABI version 2",
            ),
        ] {
            let err = load(&MINIMAL.replace(from, to)).err().unwrap().to_string();
            assert!(err.contains(want), "case {name}: {err}");
        }

        // An out-of-bounds response is a trap, not a read.
        let wat = MINIMAL.replace(
            r#"(func (export "tilo_fp_import") (param i32 i32) (result i64) i64.const 0)"#,
            r#"(func (export "tilo_fp_import") (param i32 i32) (result i64) i64.const 0x0010000000000000)"#,
        );
        let err = load(&wat).unwrap().import_doc(b"x").unwrap_err();
        assert_eq!(err.kind, "trap");
        assert!(err.message.contains("out-of-bounds response"), "{err:?}");
    }
}
