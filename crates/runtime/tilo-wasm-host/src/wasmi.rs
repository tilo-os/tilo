//! wasmi behind the shared layer: the interpreter every host has, and the browser build's only
//! engine. It executes a module from the bytes, so a host can load and call one before any
//! compile finishes.
//!
//! wasmi counts the fuel a call spends and nothing else: it has no epoch, no interrupt and no
//! way to reach into a running instance from outside, so [`Limits::deadline`](crate::Limits)
//! cannot stop a call here and every call is bounded by its fuel alone.

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
    // Relaxed SIMD's operations are defined only to within a tolerance, so a guest's answer
    // could differ between engines and machines. A guest here is a theme's painter or a
    // document's plugin, whose answer is cached and compared, so the engine every host shares
    // offers the deterministic features only.
    config.wasm_relaxed_simd(false);
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

/// A store holding `data`, enforcing `limits` and starting with a whole call's budget.
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
    begin(&mut store, limits)?;
    Ok(store)
}

/// Give the store a whole call's budget: every call starts with the call's fuel, whatever the
/// last one left. The deadline has nothing to act on here (see this module's note), so it is
/// the fuel alone that bounds a call.
///
/// # Errors
///
/// The engine does not meter fuel, which a host cannot make: [`engine`] always turns it on.
pub fn begin<D>(store: &mut Store<D>, limits: &Limits) -> Result<(), Failure> {
    store
        .set_fuel(limits.fuel_per_call)
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
