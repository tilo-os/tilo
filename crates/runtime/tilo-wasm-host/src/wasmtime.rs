//! wasmtime behind the shared layer: the compiled tier a native runtime executes beside the
//! interpreter, and Winch to compare engines with. Never built for wasm32.
//!
//! It is also the engine that can stop a call by the clock: an engine ticks a counter of its
//! own ([`TICKER`]), and every store is given a call's number of ticks to reach. A guest past
//! its ticks stops at its next epoch check, which compiled code keeps at every function entry
//! and every loop's back edge, so a loop that never calls the host is stopped as surely as one
//! that does.

use std::fmt;
use std::thread;
use std::time::Duration;

use wasmtime::{Config, Engine, Module, ResourceLimiter, Store, Strategy as Compiler};

use crate::failure::{DEADLINE_EXCEEDED, Deadline, trap_text};
use crate::{Failure, GROWTH_LIMITED, ImportPolicy, Limits, OUT_OF_FUEL};

/// How often an engine's epoch advances, and so how much later than its deadline a call can
/// stop: a call stops within one tick of running past it.
pub const TICK: Duration = Duration::from_millis(5);

/// The ticks a call with `deadline` may take: one per [`TICK`], rounded up, plus the tick
/// the engine may be partway through when the call starts.
fn ticks(deadline: Duration) -> u64 {
    let tick = TICK.as_nanos().max(1);
    let ticks = deadline.as_nanos().div_ceil(tick);
    u64::try_from(ticks).unwrap_or(u64::MAX).saturating_add(1)
}

/// Advance `engine`'s epoch every [`TICK`] until the engine is dropped: the ticker a deadline
/// is measured in.
///
/// The thread holds the engine weakly, so it ends with the engine rather than keeping it alive.
fn tick(engine: &Engine) {
    let weak = engine.weak();
    let spawned = thread::Builder::new()
        .name("wasm-epoch".to_owned())
        .spawn(move || {
            while let Some(engine) = weak.upgrade() {
                engine.increment_epoch();
                // Dropped before the sleep: the ticker must never be what keeps the engine
                // alive, or the thread would outlive the engine it exists for.
                drop(engine);
                thread::sleep(TICK);
            }
        });
    // An engine without a ticker still executes; its calls then have the fuel bound alone,
    // which is what they had before there was a deadline.
    let _ = spawned;
}

/// Which compiler wasmtime uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// The optimizing compiler.
    Cranelift,
    /// The baseline compiler, to compare engines with.
    Winch,
}

/// The engine, fuel metering on, epochs counted for a call's deadline, and no guest stack
/// walked for a trap, as wasmi's trap text is what every engine reports.
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
    config.epoch_interruption(true);
    config.wasm_backtrace_max_frames(None);
    let engine = Engine::new(&config)?;
    tick(&engine);
    Ok(engine)
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
        if maximum.is_some_and(|max| desired > max) {
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
    store.limiter(|data: &mut D| data.limits());
    // The deadline's last tick fails the call as a spent budget, with a text that says which
    // budget it was.
    store.epoch_deadline_callback(|_| Err(wasmtime::Error::new(Deadline)));
    begin(&mut store, limits)?;
    Ok(store)
}

/// Give the store a whole call's budget: every call starts with the call's fuel and, if it has
/// one, the call's deadline, whatever the last call left.
///
/// # Errors
///
/// The engine does not meter fuel, which a host cannot make: [`engine`] always turns it on.
pub fn begin<T>(store: &mut Store<T>, limits: &Limits) -> Result<(), Failure> {
    store
        .set_fuel(limits.fuel_per_call)
        .map_err(|error| Failure::Link(error.to_string()))?;
    match limits.deadline {
        Some(deadline) => store.set_epoch_deadline(ticks(deadline)),
        // Epoch interruption on and no deadline armed traps every call at once, so a call
        // without a deadline is given one the ticker cannot reach in a process's lifetime.
        None => store.set_epoch_deadline(u64::MAX),
    }
    Ok(())
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
/// trap text, a spent budget named as such, and a call stopped by its deadline.
pub fn classify(error: &wasmtime::Error) -> Failure {
    if error.downcast_ref::<GrowthLimited>().is_some() {
        return Failure::MemoryLimit(GROWTH_LIMITED.to_owned());
    }
    if error.downcast_ref::<Deadline>().is_some() {
        return Failure::Deadline(DEADLINE_EXCEEDED.to_owned());
    }
    match error.downcast_ref::<wasmtime::Trap>() {
        Some(wasmtime::Trap::OutOfFuel) => Failure::OutOfFuel(OUT_OF_FUEL.to_owned()),
        Some(trap) => Failure::Trap(trap_text(&trap.to_string())),
        None => Failure::Trap(error.to_string()),
    }
}
