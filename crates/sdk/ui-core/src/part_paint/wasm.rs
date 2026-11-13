//! Painter sets loaded from wasm: a theme's own painters, run in the UI host with nothing to reach
//! (`docs/architecture/theming.md`, "Part painters"). A module built against `tilo_theme_sdk`
//! (`export_painter!`) exports a buffer and one call, and imports nothing; [`load`] refuses one that
//! imports anything, so a painter has no clock, no I/O, no files and no host to ask.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use tilo_lock::MutexRecover as _;
use tilo_theme_sdk::wire::{self, PaintRequest};
use vello::kurbo::Size;
use wasmi::{
    Config, Engine, Instance, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder,
    TypedFunc,
};

use super::{PaintedPart, ParamSpec, Part, PartPainter, PartState, PartTokens, States, Transition};

/// Fuel one call may burn: well past what any part takes to draw (a dithered panel is a few hundred
/// thousand), short enough that a looping painter is stopped within a frame.
pub const FUEL_PER_CALL: u64 = 40_000_000;

/// Linear memory one set may hold, bytes.
pub const MAX_MEMORY: usize = 64 << 20;

/// Largest answer the host reads, bytes.
pub const MAX_ANSWER: usize = 16 << 20;

const BUFFER: &str = "tilo_theme_buffer";
const CALL: &str = "tilo_theme_call";
const MEMORY: &str = "memory";

/// Why a module is not a painter set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Not a valid wasm module.
    Invalid(String),
    /// It imports something; a painter may import nothing.
    Imports(String),
    /// It lacks `memory`, `tilo_theme_buffer` or `tilo_theme_call` of the SDK's types.
    Exports(String),
    /// Its start-up trapped or ran past its budget.
    Start(String),
    /// Its parameters are not a well-formed list.
    Params,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(why) => write!(f, "not a wasm module: {why}"),
            Self::Imports(name) => write!(f, "a painter imports nothing, this one imports {name}"),
            Self::Exports(name) => write!(f, "a painter exports {name}"),
            Self::Start(why) => write!(f, "the painter's start-up failed: {why}"),
            Self::Params => f.write_str("the painter's parameters are malformed"),
        }
    }
}

impl std::error::Error for Refusal {}

struct Guest {
    store: Store<StoreLimits>,
    memory: Memory,
    buffer: TypedFunc<u32, u32>,
    call: TypedFunc<u32, u64>,
}

impl Guest {
    /// Send `request` and read the answer; `None` for a trap, an exhausted budget or an answer out of
    /// bounds.
    fn exchange(&mut self, request: &[u8]) -> Option<Vec<u8>> {
        let len = u32::try_from(request.len()).ok()?;
        self.store.set_fuel(FUEL_PER_CALL).ok()?;
        let at = usize::try_from(self.buffer.call(&mut self.store, len).ok()?).ok()?;
        self.memory.write(&mut self.store, at, request).ok()?;
        let answer = self.call.call(&mut self.store, len).ok()?;
        let at = usize::try_from(answer >> 32).ok()?;
        let len = usize::try_from(answer & u64::from(u32::MAX)).ok()?;
        if len > MAX_ANSWER {
            return None;
        }
        let mut bytes = vec![0; len];
        self.memory.read(&self.store, at, &mut bytes).ok()?;
        Some(bytes)
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
        if answer.is_none() {
            self.broken.store(true, Ordering::Relaxed);
        }
        answer
    }

    /// Whether a call failed and the set now leaves every part to the toolkit.
    #[must_use]
    pub fn is_broken(&self) -> bool {
        self.broken.load(Ordering::Relaxed)
    }
}

/// Load the painter module `bytes`: validated, refused if it imports anything, started under the call
/// budget, and asked for its parameters.
pub fn load(bytes: &[u8]) -> Result<WasmPainter, Refusal> {
    let mut config = Config::default();
    config.consume_fuel(true);
    let engine = Engine::new(&config);
    let module = Module::new(&engine, bytes).map_err(|e| Refusal::Invalid(e.to_string()))?;
    if let Some(import) = module.imports().next() {
        return Err(Refusal::Imports(format!(
            "{}::{}",
            import.module(),
            import.name()
        )));
    }
    let limits = StoreLimitsBuilder::new()
        .memory_size(MAX_MEMORY)
        .memories(1)
        .instances(1)
        .tables(1)
        .trap_on_grow_failure(true)
        .build();
    let mut store = Store::new(&engine, limits);
    store.limiter(|limits| limits);
    store
        .set_fuel(FUEL_PER_CALL)
        .map_err(|e| Refusal::Start(e.to_string()))?;
    let instance: Instance = Linker::<StoreLimits>::new(&engine)
        .instantiate_and_start(&mut store, &module)
        .map_err(|e| Refusal::Start(e.to_string()))?;
    let memory = instance
        .get_memory(&store, MEMORY)
        .ok_or_else(|| Refusal::Exports(MEMORY.to_owned()))?;
    let buffer = instance
        .get_typed_func::<u32, u32>(&store, BUFFER)
        .map_err(|_| Refusal::Exports(format!("{BUFFER}(i32) -> i32")))?;
    let call = instance
        .get_typed_func::<u32, u64>(&store, CALL)
        .map_err(|_| Refusal::Exports(format!("{CALL}(i32) -> i64")))?;
    let mut guest = Guest {
        store,
        memory,
        buffer,
        call,
    };
    let params = guest
        .exchange(&wire::params_request())
        .and_then(|answer| wire::params(&answer))
        .ok_or(Refusal::Params)?;
    Ok(WasmPainter {
        guest: Mutex::new(guest),
        params,
        broken: AtomicBool::new(false),
    })
}

/// The set `name` drawn by the painter module `bytes` ([`load`]), ready to register
/// ([`super::register_painter_set`]).
pub fn painter_set(name: &str, bytes: &[u8]) -> Result<super::PainterSet, Refusal> {
    Ok(super::PainterSet::new(
        name,
        std::sync::Arc::new(load(bytes)?),
    ))
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
