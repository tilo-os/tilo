//! wasmi behind the shared layer: the interpreter every host has, and the browser build's only
//! engine. It executes a module from the bytes, so a host can load and call one before any
//! compile finishes.

use wasmi::{
    CompilationMode, Config, Engine, Module, ResourceLimiter, Store, StoreLimits,
    StoreLimitsBuilder, TrapCode,
};

use crate::failure::trap_text;
use crate::{Failure, GROWTH_LIMITED, ImportPolicy, Limits, OUT_OF_FUEL};

/// How wasmi turns wasm into what it executes: validate a module when it loads, or translate
/// every function at load.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Translation {
    /// Validate when the module is compiled, translate each function on its first call.
    #[default]
    Lazy,
    /// Validate and translate every function when the module is compiled.
    Eager,
}

/// The engine, fuel metering on: a store without fuel for the next call cannot make one.
pub fn engine(translation: Translation) -> Engine {
    let mut config = Config::default();
    config.consume_fuel(true);
    config.compilation_mode(match translation {
        Translation::Lazy => CompilationMode::LazyTranslation,
        Translation::Eager => CompilationMode::Eager,
    });
    Engine::new(&config)
}

/// The limits a store enforces, built for wasmi: growing a memory or a table past them traps, as
/// wasm's own `memory.grow` failure would.
pub fn store_limits(limits: &Limits) -> StoreLimits {
    StoreLimitsBuilder::new()
        .memory_size(limits.max_memory_bytes)
        .memories(limits.max_memories)
        .tables(limits.max_tables)
        .instances(limits.max_instances)
        .table_elements(limits.max_table_elements)
        .trap_on_grow_failure(true)
        .build()
}

/// Store data that holds the [`StoreLimits`] its store's limiter reads.
pub trait Limited {
    /// The limits, as the store's [`ResourceLimiter`].
    fn limits(&mut self) -> &mut dyn ResourceLimiter;
}

impl Limited for StoreLimits {
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
    store.limiter(D::limits);
    refuel(&mut store, limits.fuel_per_call)?;
    Ok(store)
}

/// Give the store a whole call's budget: every call starts with `fuel`, whatever the last one
/// left.
///
/// # Errors
///
/// The engine does not meter fuel, which a host cannot make: [`engine`] always turns it on.
pub fn refuel<D>(store: &mut Store<D>, fuel: u64) -> Result<(), Failure> {
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

/// What a failed call means, the same as wasmtime reports it: wasm's trap text, and a spent
/// budget or a refused growth named as such.
pub fn classify(error: &wasmi::Error) -> Failure {
    match error.as_trap_code() {
        Some(TrapCode::OutOfFuel) => Failure::OutOfFuel(OUT_OF_FUEL.to_owned()),
        Some(TrapCode::GrowthOperationLimited) => Failure::MemoryLimit(GROWTH_LIMITED.to_owned()),
        Some(code) => Failure::Trap(trap_text(code.trap_message())),
        None => Failure::Trap(error.to_string()),
    }
}
