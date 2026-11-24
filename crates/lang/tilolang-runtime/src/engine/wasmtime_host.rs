//! wasmtime behind the shared layer (`tilo_wasm_host`): the compiled tier a native runtime
//! executes beside the interpreter, and Winch to compare engines with. Never built for wasm32.
//!
//! It lives with the runtime, its only host, rather than behind a feature of the shared layer:
//! there a feature one build turns on reaches every engine library that build links, and the
//! interpreter-only ones would carry a compiler they never run.
//!
//! It is also the engine that can stop a call by the clock: while a call is inside the guest
//! its engine's epoch advances every [`TICK`], and the store is given the call's number of ticks
//! to reach. A guest past its ticks stops at its next epoch check, which compiled code keeps at
//! every function entry and every loop's back edge, so a loop that never calls the host is
//! stopped as surely as one that does. The clock advances only while a call is in flight: an
//! engine with no call in it wakes no thread.

use std::fmt;
use std::ops::Deref;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[cfg(any(feature = "compile", test))]
use tilo_wasm_host::ImportPolicy;
use tilo_wasm_host::{
    DEADLINE_EXCEEDED, Deadline, Failure, GROWTH_LIMITED, Limits, OUT_OF_FUEL, trap_text,
};
#[cfg(any(feature = "compile", test))]
use wasmtime::Module;
use wasmtime::{ResourceLimiter, Store};

/// How often an engine's epoch advances while a call is in flight, and so how much later than
/// its deadline a call can stop: a call stops within one tick of running past it.
pub const TICK: Duration = Duration::from_millis(5);

/// The ticks a call with `deadline` may take: one per [`TICK`], rounded up, plus the tick
/// the engine may be partway through when the call starts.
fn ticks(deadline: Duration) -> u64 {
    let tick = TICK.as_nanos().max(1);
    let ticks = deadline.as_nanos().div_ceil(tick);
    u64::try_from(ticks).unwrap_or(u64::MAX).saturating_add(1)
}

/// An engine and the clock that bounds its calls.
///
/// It derefs to the engine it wraps, so it goes wherever wasmtime takes an engine.
pub struct Engine {
    inner: wasmtime::Engine,
    clock: Arc<Clock>,
    ticker: Option<JoinHandle<()>>,
}

impl Deref for Engine {
    type Target = wasmtime::Engine;

    fn deref(&self) -> &wasmtime::Engine {
        &self.inner
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.clock.stop();
        if let Some(ticker) = self.ticker.take() {
            ticker.join().ok();
        }
    }
}

impl Engine {
    /// The ticks this engine's clock has made.
    #[cfg(test)]
    pub(crate) fn ticks(&self) -> u64 {
        self.clock.ticks()
    }
}

/// An engine's epoch, advanced while calls are in flight.
struct Clock {
    state: Mutex<State>,
    wake: Condvar,
}

#[derive(Default)]
struct State {
    /// Calls inside guest code.
    calls: usize,
    /// The engine is gone: the ticker ends.
    gone: bool,
    /// Ticks made so far.
    ticks: u64,
}

impl Clock {
    /// A clock for `engine`, and the ticker that advances it, if the thread could be started.
    fn new(engine: &wasmtime::Engine) -> (Arc<Self>, Option<JoinHandle<()>>) {
        let clock = Arc::new(Self {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
        });
        let weak = engine.weak();
        let ticking = Arc::clone(&clock);
        let ticker = thread::Builder::new()
            .name("wasm-epoch".to_owned())
            .spawn(move || ticking.tick(&weak))
            .ok();
        (clock, ticker)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Advance the epoch every [`TICK`] while a call is in flight, and wait with no deadline
    /// while none is: an idle engine wakes this thread never.
    ///
    /// It holds the engine weakly, so it ends with the engine rather than keeping it alive.
    fn tick(&self, engine: &wasmtime::EngineWeak) {
        let mut state = self.lock();
        loop {
            while !state.gone && state.calls == 0 {
                state = self
                    .wake
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            if state.gone {
                return;
            }
            // Dropped before the wait: neither the clock nor its ticker may keep the engine
            // alive, or the thread would outlive the engine it exists for.
            match engine.upgrade() {
                Some(engine) => engine.increment_epoch(),
                None => return,
            }
            state.ticks = state.ticks.saturating_add(1);
            state = self
                .wake
                .wait_timeout(state, TICK)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Count a call in, and keep the epoch advancing until the returned guard is dropped.
    fn arm(&self) -> InCall<'_> {
        let mut state = self.lock();
        state.calls = state.calls.saturating_add(1);
        drop(state);
        self.wake.notify_one();
        InCall { clock: self }
    }

    fn disarm(&self) {
        let mut state = self.lock();
        state.calls = state.calls.saturating_sub(1);
        let idle = state.calls == 0;
        drop(state);
        if idle {
            self.wake.notify_all();
        }
    }

    fn stop(&self) {
        let mut state = self.lock();
        state.gone = true;
        drop(state);
        self.wake.notify_all();
    }

    #[cfg(test)]
    fn ticks(&self) -> u64 {
        self.lock().ticks
    }
}

/// A call inside guest code: while it lives the engine's epoch advances every [`TICK`], so the
/// call's deadline can arrive. Dropped when the call returns, and an engine with no call in it
/// then ticks nothing.
#[must_use = "dropped at once, the call's deadline never arrives"]
pub struct InCall<'a> {
    clock: &'a Clock,
}

impl Drop for InCall<'_> {
    fn drop(&mut self) {
        self.clock.disarm();
    }
}

/// Advance `engine`'s clock while the returned guard lives: give guest code outside [`begin`]
/// the same deadline, its start function included.
#[must_use = "dropped at once, the guest gets no deadline"]
pub fn in_call(engine: &Engine) -> InCall<'_> {
    engine.clock.arm()
}

/// Which compiler wasmtime uses.
#[cfg(any(feature = "compile", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// The optimizing compiler.
    Cranelift,
    /// The baseline compiler, to compare engines with.
    #[cfg(feature = "compile")]
    Winch,
}

/// The engine, fuel metering on, epochs counted for a call's deadline, and no guest stack
/// walked for a trap, as wasmi's trap text is what every engine reports.
///
/// The configuration is the one the device daemon compiles with, made in one place: a module
/// it compiled is only accepted by an engine configured the same way.
///
/// # Errors
///
/// The engine cannot be made on this machine.
#[cfg(any(feature = "compile", test))]
pub fn engine(strategy: Strategy) -> Result<Engine, wasmtime::Error> {
    #[allow(
        unused_mut,
        reason = "a runtime-only build has no compiler to choose, so the configuration is not moved"
    )]
    let mut config = tilo_wasm_aot::config();
    // Winch is a comparison engine only, so only a build that can compile can select it.
    #[cfg(feature = "compile")]
    config.strategy(match strategy {
        Strategy::Cranelift => wasmtime::Strategy::Cranelift,
        Strategy::Winch => wasmtime::Strategy::Winch,
    });
    #[cfg(not(feature = "compile"))]
    let _ = strategy;
    let inner = wasmtime::Engine::new(&config)?;
    let (clock, ticker) = Clock::new(&inner);
    Ok(Engine {
        inner,
        clock,
        ticker,
    })
}

/// The engine a runtime deserializes with: the shared configuration, no compiler linked.
///
/// # Errors
///
/// The engine cannot be made on this machine.
pub fn runtime_engine() -> Result<Engine, wasmtime::Error> {
    let inner = wasmtime::Engine::new(&tilo_wasm_aot::config())?;
    let (clock, ticker) = Clock::new(&inner);
    Ok(Engine {
        inner,
        clock,
        ticker,
    })
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
    let mut store = Store::new(&engine.inner, data);
    store.limiter(|data: &mut D| data.limits());
    // The deadline's last tick fails the call as a spent budget, with a text that says which
    // budget it was.
    store.epoch_deadline_callback(|_| Err(wasmtime::Error::new(Deadline)));
    budget(&mut store, limits)?;
    Ok(store)
}

/// Give the store a whole call's budget: every call starts with the call's fuel and, if it has
/// one, the call's deadline, whatever the last call left.
///
/// # Errors
///
/// The engine does not meter fuel, which a host cannot make: [`engine`] always turns it on.
fn budget<T>(store: &mut Store<T>, limits: &Limits) -> Result<(), Failure> {
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

/// Give the store a whole call's budget and advance the engine's clock until the returned guard
/// is dropped, which is when the call returns.
///
/// # Errors
///
/// The engine does not meter fuel, which a host cannot make: [`engine`] always turns it on.
pub fn begin<'a, T>(
    engine: &'a Engine,
    store: &mut Store<T>,
    limits: &Limits,
) -> Result<InCall<'a>, Failure> {
    budget(store, limits)?;
    Ok(in_call(engine))
}

/// Compile and validate `bytes`, refusing any import `policy` does not admit.
///
/// # Errors
///
/// The bytes are not a wasm module ([`Failure::Invalid`]), or the module imports what the policy
/// admits nothing named ([`Failure::Imports`]).
#[cfg(any(feature = "compile", test))]
pub fn compile(engine: &Engine, bytes: &[u8], policy: ImportPolicy) -> Result<Module, Failure> {
    let module =
        Module::new(&engine.inner, bytes).map_err(|error| Failure::Invalid(error.to_string()))?;
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

#[cfg(test)]
#[path = "wasmtime_host_tests.rs"]
mod tests;
