//! A painter set running as the machine code the device's daemon compiled for it: wasmtime's
//! runtime, no compiler. The same guest interface, limits and refusals as the interpreter
//! ([`super::Guest`]), so a theme cannot tell which one drew it.
//!
//! The artifact is checked to be the one made for these module bytes before it is loaded, and
//! wasmtime checks the version, configuration and target as it deserializes. A set imports
//! nothing: the linker is empty, so an import is refused rather than answered.

use std::sync::OnceLock;

use tilo_theme_sdk::Refusal;
use tilo_wasm_host::Failure;
use wasmtime::{Engine, Linker, Memory, Module, ResourceLimiter, Store, TypedFunc};

use super::{BUFFER, CALL, FUEL_PER_CALL, MAX_ANSWER, MAX_MEMORY, MEMORY};

/// The engine every compiled set is deserialized on: the runtime configuration, no compiler.
fn engine() -> Result<&'static Engine, Refusal> {
    static ENGINE: OnceLock<Option<Engine>> = OnceLock::new();
    ENGINE
        .get_or_init(|| tilo_wasm_aot::runtime_engine().ok())
        .as_ref()
        .ok_or_else(|| Refusal::Unavailable("the compiled painter engine was not made".to_owned()))
}

/// What one set may hold: memory past the cap, or a second memory or table, stops it.
struct Caps;

impl ResourceLimiter for Caps {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > MAX_MEMORY || maximum.is_some_and(|max| desired > max) {
            return Err(wasmtime::Error::msg(tilo_wasm_host::GROWTH_LIMITED));
        }
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > 1 {
            return Err(wasmtime::Error::msg(tilo_wasm_host::GROWTH_LIMITED));
        }
        Ok(true)
    }

    fn instances(&self) -> usize {
        1
    }

    fn memories(&self) -> usize {
        1
    }

    fn tables(&self) -> usize {
        1
    }
}

pub(super) struct Guest {
    store: Store<Caps>,
    memory: Memory,
    buffer: TypedFunc<u32, u32>,
    call: TypedFunc<u32, u64>,
}

impl Guest {
    /// The set whose module is `bytes` and whose compiled artifact is `entry`, started under the
    /// call budget.
    pub(super) fn new(bytes: &[u8], entry: &[u8]) -> Result<Self, Refusal> {
        let engine = engine()?;
        let serialized = tilo_wasm_aot::open_for(entry, bytes).map_err(Refusal::Invalid)?;
        // SAFETY: the entry was checked to be the one the daemon made for these module bytes, and
        // wasmtime checks the version, configuration and target it was made for.
        let module = unsafe { Module::deserialize(engine, serialized) }
            .map_err(|e| Refusal::Invalid(e.to_string()))?;
        if let Some(import) = module.imports().next() {
            return Err(Refusal::Imports(format!(
                "{}::{}",
                import.module(),
                import.name()
            )));
        }
        let mut store = Store::new(engine, Caps);
        store.limiter(|caps| caps);
        // The engine counts epochs; no ticker advances them here, so the deadline is set past
        // any epoch and a call is bounded by its fuel alone, as it is on the interpreter.
        store.set_epoch_deadline(u64::MAX);
        store
            .set_fuel(FUEL_PER_CALL)
            .map_err(|e| Refusal::Start(e.to_string()))?;
        let instance = Linker::new(engine)
            .instantiate(&mut store, &module)
            .map_err(|error| Refusal::Start(error.to_string()))?;
        let memory = instance
            .get_memory(&mut store, MEMORY)
            .ok_or_else(|| Refusal::Exports(MEMORY.to_owned()))?;
        let buffer = instance
            .get_typed_func::<u32, u32>(&mut store, BUFFER)
            .map_err(|_| Refusal::Exports(format!("{BUFFER}(i32) -> i32")))?;
        let call = instance
            .get_typed_func::<u32, u64>(&mut store, CALL)
            .map_err(|_| Refusal::Exports(format!("{CALL}(i32) -> i64")))?;
        Ok(Self {
            store,
            memory,
            buffer,
            call,
        })
    }

    pub(super) fn exchange(&mut self, request: &[u8]) -> Result<Vec<u8>, Failure> {
        let len = u32::try_from(request.len())
            .map_err(|_| Failure::Trap("the request is larger than a wasm address".to_owned()))?;
        // A whole call's budget, as the interpreter is given one.
        self.store
            .set_fuel(FUEL_PER_CALL)
            .map_err(|error| Failure::Trap(error.to_string()))?;
        let at = self
            .buffer
            .call(&mut self.store, len)
            .map_err(|error| classify(&error))?;
        let at = usize::try_from(at).unwrap_or(usize::MAX);
        self.memory
            .write(&mut self.store, at, request)
            .map_err(|error| Failure::Trap(error.to_string()))?;
        let answer = self
            .call
            .call(&mut self.store, len)
            .map_err(|error| classify(&error))?;
        let at = usize::try_from(answer >> 32).unwrap_or(usize::MAX);
        let len = usize::try_from(answer & u64::from(u32::MAX)).unwrap_or(usize::MAX);
        if len > MAX_ANSWER {
            return Err(Failure::Trap(format!(
                "the answer claims {len} bytes, past the {MAX_ANSWER} the toolkit reads"
            )));
        }
        let mut bytes = vec![0; len];
        self.memory
            .read(&self.store, at, &mut bytes)
            .map_err(|error| Failure::Trap(error.to_string()))?;
        Ok(bytes)
    }
}

/// A failed call, as the interpreter reports one: the fuel text where the budget was spent, the
/// growth text where memory was refused, and the trap's own words otherwise.
fn classify(error: &wasmtime::Error) -> Failure {
    let text = error.to_string();
    if text.contains("fuel") {
        Failure::OutOfFuel(tilo_wasm_host::OUT_OF_FUEL.to_owned())
    } else if text.contains(tilo_wasm_host::GROWTH_LIMITED) {
        Failure::MemoryLimit(tilo_wasm_host::GROWTH_LIMITED.to_owned())
    } else {
        Failure::Trap(text)
    }
}
