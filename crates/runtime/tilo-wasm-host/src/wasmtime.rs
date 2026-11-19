//! wasmtime behind the shared layer: the compiled tier a native runtime executes beside the
//! interpreter, and Winch to compare engines with. Never built for wasm32.

use std::fmt;

use wasmtime::{Config, Engine, Module, ResourceLimiter, Store, Strategy as Compiler};

use crate::failure::trap_text;
use crate::{Failure, GROWTH_LIMITED, ImportPolicy, Limits, OUT_OF_FUEL};

/// Which compiler wasmtime uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// The optimizing compiler.
    Cranelift,
    /// The baseline compiler, to compare engines with.
    Winch,
}

/// The engine, fuel metering on and no guest stack walked for a trap, as wasmi's trap text is
/// what every engine reports.
///
/// # Errors
///
/// The strategy is not built into this wasmtime, or the engine cannot be made on this machine.
pub fn engine(strategy: Strategy) -> Result<Engine, wasmtime::Error> {
    let mut config = Config::new();
    config.strategy(match strategy {
        Strategy::Cranelift => Compiler::Cranelift,
        Strategy::Winch => Compiler::Winch,
    });
    config.consume_fuel(true);
    config.wasm_backtrace_max_frames(None);
    Engine::new(&config)
}

/// A memory or table growth past what the instance may hold.
#[derive(Debug)]
struct GrowthLimited;

impl fmt::Display for GrowthLimited {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(GROWTH_LIMITED)
    }
}

impl std::error::Error for GrowthLimited {}

/// The counts a store may hold and the growth it may make, for wasmtime's limiter: growth past
/// the limits traps, as wasmi's `trap_on_grow_failure` makes it.
pub struct GrowthLimits {
    limits: Limits,
}

impl GrowthLimits {
    /// Growth limits for a store enforcing `limits`.
    pub const fn new(limits: Limits) -> Self {
        Self { limits }
    }
}

impl ResourceLimiter for GrowthLimits {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > self.limits.max_memory_bytes || maximum.is_some_and(|max| desired > max) {
            return Err(wasmtime::Error::new(GrowthLimited));
        }
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > self.limits.max_table_elements || maximum.is_some_and(|max| desired > max) {
            return Err(wasmtime::Error::new(GrowthLimited));
        }
        Ok(true)
    }

    fn instances(&self) -> usize {
        self.limits.max_instances
    }

    fn tables(&self) -> usize {
        self.limits.max_tables
    }

    fn memories(&self) -> usize {
        self.limits.max_memories
    }
}

/// Store data that holds its [`GrowthLimits`].
pub trait Limited {
    /// The limits, as the store's [`ResourceLimiter`].
    fn limits(&mut self) -> &mut dyn ResourceLimiter;
}

impl Limited for GrowthLimits {
    fn limits(&mut self) -> &mut dyn ResourceLimiter {
        self
    }
}

/// A store holding `data`, enforcing `limits` and starting with a whole call's fuel.
///
/// # Errors
///
/// The engine does not meter fuel, which a host cannot make: [`engine`] always turns it on.
pub fn store<D: Limited + 'static>(
    engine: &Engine,
    data: D,
    limits: &Limits,
) -> Result<Store<D>, Failure> {
    let mut store = Store::new(engine, data);
    store.limiter(|data: &mut D| data.limits());
    refuel(&mut store, limits.fuel_per_call)?;
    Ok(store)
}

/// Give the store a whole call's budget: every call starts with `fuel`, whatever the last one
/// left.
///
/// # Errors
///
/// The engine does not meter fuel, which a host cannot make: [`engine`] always turns it on.
pub fn refuel<T>(store: &mut Store<T>, fuel: u64) -> Result<(), Failure> {
    store
        .set_fuel(fuel)
        .map_err(|error| Failure::Link(error.to_string()))
}

/// Compile and validate `bytes`, refusing any import `policy` does not admit.
///
/// # Errors
///
/// The bytes are not a wasm module ([`Failure::Invalid`]), or the module imports what the policy
/// admits nothing named ([`Failure::Imports`]).
pub fn compile(engine: &Engine, bytes: &[u8], policy: ImportPolicy) -> Result<Module, Failure> {
    let module = Module::new(engine, bytes).map_err(|error| Failure::Invalid(error.to_string()))?;
    if policy == ImportPolicy::None
        && let Some(import) = module.imports().next()
    {
        return Err(Failure::Imports(format!(
            "{}::{}",
            import.module(),
            import.name()
        )));
    }
    Ok(module)
}

/// What a failed call means, the same as wasmi reports it: a growth the limiter refused, wasm's
/// trap text, and a spent budget named as such.
pub fn classify(error: &wasmtime::Error) -> Failure {
    if error.downcast_ref::<GrowthLimited>().is_some() {
        return Failure::MemoryLimit(GROWTH_LIMITED.to_owned());
    }
    match error.downcast_ref::<wasmtime::Trap>() {
        Some(wasmtime::Trap::OutOfFuel) => Failure::OutOfFuel(OUT_OF_FUEL.to_owned()),
        Some(trap) => Failure::Trap(trap_text(&trap.to_string())),
        None => Failure::Trap(error.to_string()),
    }
}
