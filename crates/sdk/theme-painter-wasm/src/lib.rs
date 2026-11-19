//! Painter sets loaded from wasm: a theme's own painters, run with nothing to
//! reach (`docs/architecture/theming.md`, "Part painters"). A module built
//! against `tilo_theme_sdk` (`export_painter!`) exports a buffer and one call,
//! and imports nothing; loading refuses one that imports anything, so a
//! painter has no clock, no I/O, no files and nothing to ask.
//!
//! The interpreter, its fuel, its memory cap and its failures are
//! `tilo_wasm_host`'s; this crate is the guest's side of it: the bytes it
//! exchanges and the rule that a set whose call failed is left to the toolkit
//! for good.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tilo_lock::MutexRecover as _;
use tilo_theme_sdk::kurbo::Size;
use tilo_theme_sdk::wire::{self, PaintRequest};
use tilo_theme_sdk::{
    PaintedPart, ParamSpec, Part, PartPainter, PartState, PartTokens, Refusal, States, Transition,
};
use tilo_wasm_host::wasmi as host;
use tilo_wasm_host::{Failure, ImportPolicy, Limits};
use wasmi::{Instance, Linker, Memory, Store, StoreLimits, TypedFunc};

/// Fuel one call may burn: well past what any part takes to draw (a dithered panel is a few hundred
/// thousand), short enough that a looping painter is stopped within a frame.
pub const FUEL_PER_CALL: u64 = 40_000_000;

/// Linear memory one set may hold, bytes.
pub const MAX_MEMORY: usize = 64 << 20;

/// Largest answer the toolkit reads, bytes.
pub const MAX_ANSWER: usize = 16 << 20;

/// The limits a set executes under: one memory, one table and one instance, with
/// `trap_on_grow_failure` set by the host layer so growing past the memory cap
/// stops the call.
const LIMITS: Limits = Limits {
    fuel_per_call: FUEL_PER_CALL,
    // This set runs on wasmi, which has no interrupt, so the deadline is not enforced here;
    // it is named so a painter that later runs on a compiled engine is bounded by the clock
    // too. A painter that loops forever is stopped by its fuel within a frame.
    deadline: Some(Duration::from_millis(250)),
    max_memory_bytes: MAX_MEMORY,
    max_memories: 1,
    max_tables: 1,
    max_instances: 1,
};

const BUFFER: &str = "tilo_theme_buffer";
const CALL: &str = "tilo_theme_call";
const MEMORY: &str = "memory";

struct Guest {
    store: Store<StoreLimits>,
    memory: Memory,
    buffer: TypedFunc<u32, u32>,
    call: TypedFunc<u32, u64>,
}

impl Guest {
    /// Send `request` and read the answer; a trap, an exhausted budget or a refused growth is the
    /// engine's [`Failure`], and an answer that does not fit the set's memory is one too.
    fn exchange(&mut self, request: &[u8]) -> Result<Vec<u8>, Failure> {
        let len = u32::try_from(request.len())
            .map_err(|_| Failure::Trap("the request is larger than a wasm address".to_owned()))?;
        host::begin(&mut self.store, &LIMITS)?;
        let at = self
            .buffer
            .call(&mut self.store, len)
            .map_err(|error| host::classify(&error))?;
        let at = usize::try_from(at).unwrap_or(usize::MAX);
        self.memory
            .write(&mut self.store, at, request)
            .map_err(|error| Failure::Trap(error.to_string()))?;
        let answer = self
            .call
            .call(&mut self.store, len)
            .map_err(|error| host::classify(&error))?;
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

/// A painter set running in wasm.
pub struct WasmPainter {
    guest: Mutex<Guest>,
    params: Vec<ParamSpec>,
    broken: AtomicBool,
}

impl WasmPainter {
    /// One exchange, or `None` once the set is broken; a failed exchange breaks it.
    fn exchange(&self, request: &[u8]) -> Option<Vec<u8>> {
        if self.broken.load(Ordering::Relaxed) {
            return None;
        }
        let answer = self.guest.lock_recover().exchange(request);
        if !tilo_wasm_host::reusable(&answer) {
            self.broken.store(true, Ordering::Relaxed);
        }
        answer.ok()
    }

    /// Whether a call failed and the set now leaves every part to the toolkit.
    #[must_use]
    pub fn is_broken(&self) -> bool {
        self.broken.load(Ordering::Relaxed)
    }
}

/// Load the painter module `bytes`: validated, refused if it imports anything, started under the call
/// budget, and asked for its parameters.
///
/// # Errors
///
/// A [`Refusal`] naming what the set is not: a wasm module, a painter that
/// imports nothing, a set exporting what the SDK's painter does, one whose
/// start-up failed, or one whose parameters are not the SDK's list.
pub fn load(bytes: &[u8]) -> Result<WasmPainter, Refusal> {
    let engine = host::engine(host::Translation::Lazy);
    let module = host::compile(&engine, bytes, ImportPolicy::None).map_err(refusal)?;
    let mut store = host::store(&engine, host::store_limits(&LIMITS), &LIMITS).map_err(refusal)?;
    let instance: Instance = Linker::<StoreLimits>::new(&engine)
        .instantiate_and_start(&mut store, &module)
        .map_err(|error| refusal(host::classify(&error)))?;
    let memory = instance
        .get_memory(&store, MEMORY)
        .ok_or_else(|| refusal(Failure::NoExport(MEMORY.to_owned())))?;
    let buffer = instance
        .get_typed_func::<u32, u32>(&store, BUFFER)
        .map_err(|_| refusal(Failure::NoExport(format!("{BUFFER}(i32) -> i32"))))?;
    let call = instance
        .get_typed_func::<u32, u64>(&store, CALL)
        .map_err(|_| refusal(Failure::NoExport(format!("{CALL}(i32) -> i64"))))?;
    let mut guest = Guest {
        store,
        memory,
        buffer,
        call,
    };
    let params = guest
        .exchange(&wire::params_request())
        .ok()
        .and_then(|answer| wire::params(&answer))
        .ok_or(Refusal::Params)?;
    Ok(WasmPainter {
        guest: Mutex::new(guest),
        params,
        broken: AtomicBool::new(false),
    })
}

impl PartPainter for WasmPainter {
    fn paint(
        &self,
        part: Part,
        size: Size,
        state: PartState,
        progress: f32,
        tokens: &PartTokens,
    ) -> Option<PaintedPart> {
        let request = wire::paint_request(&PaintRequest {
            part,
            size,
            state,
            progress,
            tokens: tokens.clone(),
        });
        let answer = self.exchange(&request)?;
        let painted = wire::paint(&answer);
        if painted.is_none() {
            // Bytes that are not an answer: a painter that does not speak
            // the protocol is not asked again.
            self.broken.store(true, Ordering::Relaxed);
        }
        painted.flatten()
    }

    fn params(&self) -> Vec<ParamSpec> {
        self.params.clone()
    }

    fn transition(&self, part: Part, from: States, to: States) -> Transition {
        self.exchange(&wire::transition_request(part, from, to))
            .and_then(|answer| wire::transition(&answer))
            .unwrap_or(Transition::Inherit)
    }
}

/// How the shared host layer's failures read as a painter refusal.
fn refusal(failure: Failure) -> Refusal {
    match failure {
        Failure::Invalid(why) => Refusal::Invalid(why),
        Failure::Imports(name) => Refusal::Imports(name),
        Failure::NoExport(name) => Refusal::Exports(name),
        Failure::Trap(why)
        | Failure::OutOfFuel(why)
        | Failure::Deadline(why)
        | Failure::MemoryLimit(why)
        | Failure::Link(why) => Refusal::Start(why),
    }
}

#[cfg(test)]
mod tests;
