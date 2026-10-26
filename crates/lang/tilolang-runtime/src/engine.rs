//! The wasm engine. The only module that names wasmi.
//!
//! * One wasmi engine per process: every runtime (every room's host, the surface, a block)
//!   compiles into it, and a module is compiled once per process, cached by the content hash of
//!   its bytes. Each runtime keeps its own instances (and so its own stores and limits).
//! * Each module normally has one instance. A call that arrives while the module's
//!   instance is already running (a flow in a module invoking an action of the same module)
//!   gets a second instance from the same compiled module, which costs microseconds; guests
//!   keep no state between calls, so instances are interchangeable.
//! * Runaway code is stopped by fuel, a fixed budget per call. Memory growth beyond the
//!   per-instance limit traps.
//! * Imports reach the host through the [`Host`] trait; the engine knows nothing of the ABI
//!   beyond the signatures in [`crate::abi`].

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use wasmi::errors::HostError;
use wasmi::{
    Caller, CompilationMode, Config, Extern, Func, FuncType, Instance, Linker, Module, Store,
    StoreLimits, StoreLimitsBuilder, TrapCode, Val, ValType,
};

use crate::abi::{self, IMPORTS};
use crate::ids::ModuleHash;

/// A wasm scalar crossing the boundary. Handles and bools travel as `I32`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Wasm {
    I32(i32),
    I64(i64),
    F64(f64),
}

/// Why a host import did not return a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostFail {
    /// Trap the guest with this message.
    Trap(String),
    /// Unwind the guest because a flow is waiting for a decision.
    Suspend,
}

/// The host side of the imports.
pub trait Host {
    /// Run import number `id` of [`IMPORTS`]. `memory` is the caller's exported memory, or
    /// empty when it has none.
    ///
    /// # Errors
    ///
    /// A [`HostFail`] traps or suspends the guest.
    fn import(&self, id: usize, memory: &[u8], args: &[Wasm]) -> Result<Option<Wasm>, HostFail>;
}

/// Why a call into a module failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CallError {
    #[error("trap: {0}")]
    Trap(String),
    #[error("the flow is suspended")]
    Suspended,
    #[error("out of fuel: {0}")]
    OutOfFuel(String),
    #[error("memory limit: {0}")]
    MemoryLimit(String),
    #[error("module has no export `{0}`")]
    NoExport(String),
    #[error("cannot link module: {0}")]
    Link(String),
}

/// Per-call and per-instance limits.
#[derive(Clone, Copy, Debug)]
pub struct EngineLimits {
    /// Fuel for one call into a module, nested calls excluded.
    pub fuel_per_call: u64,
    /// Linear memory an instance may reach.
    pub max_memory_bytes: usize,
}

impl Default for EngineLimits {
    fn default() -> Self {
        Self {
            fuel_per_call: 2_000_000_000,
            max_memory_bytes: 64 * 1024 * 1024,
        }
    }
}

/// How the process's engine turns wasm into what it runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Translation {
    /// Validate a module when it is compiled, translate each function on its first call.
    #[default]
    Lazy,
    /// Validate and translate every function when the module is compiled.
    Eager,
}

/// The process's engine and the modules compiled into it.
struct Shared {
    engine: wasmi::Engine,
    translation: Translation,
    modules: Mutex<HashMap<ModuleHash, Module>>,
}

static SHARED: OnceLock<Shared> = OnceLock::new();

fn shared() -> &'static Shared {
    SHARED.get_or_init(|| Shared::new(Translation::default()))
}

impl Shared {
    fn new(translation: Translation) -> Self {
        let mut cfg = Config::default();
        cfg.consume_fuel(true);
        cfg.compilation_mode(match translation {
            Translation::Lazy => CompilationMode::LazyTranslation,
            Translation::Eager => CompilationMode::Eager,
        });
        Self {
            engine: wasmi::Engine::new(&cfg),
            translation,
            modules: Mutex::new(HashMap::new()),
        }
    }
}

/// Choose how the process's engine translates, before any runtime exists. Returns the choice in
/// effect: once the engine is made it stays as it was made.
pub fn set_translation(translation: Translation) -> Translation {
    SHARED.get_or_init(|| Shared::new(translation)).translation
}

/// Where the time of the calls into wasm went, while a profile is on
/// ([`Engine::profile`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EngineProfile {
    /// Calls into a module.
    pub calls: u64,
    /// Time inside those calls, imports included.
    pub in_calls: Duration,
    /// Of that, time in the host's imports.
    pub in_imports: Duration,
    /// Imports called.
    pub imports: u64,
    /// Time compiling modules (only modules new to the process are compiled).
    pub compiling: Duration,
}

struct Slot {
    host: Option<Rc<dyn Host>>,
    limits: StoreLimits,
    profile: Option<Rc<Cell<EngineProfile>>>,
}

struct Pooled {
    store: Store<Slot>,
    instance: Instance,
    /// Export handles and result types, resolved on first use.
    funcs: HashMap<String, (Func, Vec<ValType>)>,
}

struct Compiled {
    module: Module,
    idle: Vec<Pooled>,
}

#[derive(Debug)]
struct SuspendMarker;

impl std::fmt::Display for SuspendMarker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("flow suspended")
    }
}
impl HostError for SuspendMarker {}

#[derive(Debug)]
struct TrapMessage(String);

impl std::fmt::Display for TrapMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl HostError for TrapMessage {}

/// One runtime's view of the process's engine: its instances of the modules it loaded, and
/// its limits.
pub struct Engine {
    linker: Linker<Slot>,
    limits: EngineLimits,
    modules: RefCell<HashMap<ModuleHash, Compiled>>,
    profile: RefCell<Option<Rc<Cell<EngineProfile>>>>,
}

fn val_type(c: u8) -> ValType {
    match c {
        b'l' => ValType::I64,
        b'f' => ValType::F64,
        _ => ValType::I32,
    }
}

fn signature(params: &str, result: &str) -> FuncType {
    FuncType::new(params.bytes().map(val_type), result.bytes().map(val_type))
}

fn to_wasm(v: &Val) -> Result<Wasm, wasmi::Error> {
    match v {
        Val::I32(i) => Ok(Wasm::I32(*i)),
        Val::I64(i) => Ok(Wasm::I64(*i)),
        Val::F64(f) => Ok(Wasm::F64(f.to_float())),
        _ => Err(wasmi::Error::new("unsupported wasm value type")),
    }
}

fn from_wasm(w: Wasm) -> Val {
    match w {
        Wasm::I32(i) => Val::I32(i),
        Wasm::I64(i) => Val::I64(i),
        Wasm::F64(f) => Val::from(f),
    }
}

fn run_import(
    id: usize,
    caller: &Caller<'_, Slot>,
    params: &[Val],
    results: &mut [Val],
) -> Result<(), wasmi::Error> {
    let host = caller
        .data()
        .host
        .clone()
        .ok_or_else(|| wasmi::Error::new("import called outside a host call"))?;
    let args = params.iter().map(to_wasm).collect::<Result<Vec<_>, _>>()?;
    let memory = caller.get_export("memory").and_then(Extern::into_memory);
    let bytes: &[u8] = memory.as_ref().map_or(&[][..], |m| m.data(caller));
    let started = caller.data().profile.as_ref().map(|_| Instant::now());
    let result = host.import(id, bytes, &args);
    if let (Some(profile), Some(started)) = (&caller.data().profile, started) {
        let mut p = profile.get();
        p.imports = p.imports.saturating_add(1);
        p.in_imports = p.in_imports.saturating_add(started.elapsed());
        profile.set(p);
    }
    match result {
        Ok(out) => {
            if let (Some(slot), Some(w)) = (results.first_mut(), out) {
                *slot = from_wasm(w);
            }
            Ok(())
        }
        Err(HostFail::Trap(msg)) => Err(wasmi::Error::host(TrapMessage(msg))),
        Err(HostFail::Suspend) => Err(wasmi::Error::host(SuspendMarker)),
    }
}

fn classify(e: &wasmi::Error) -> CallError {
    if e.downcast_ref::<SuspendMarker>().is_some() {
        return CallError::Suspended;
    }
    if let Some(t) = e.downcast_ref::<TrapMessage>() {
        return CallError::Trap(t.0.clone());
    }
    match e.as_trap_code() {
        Some(TrapCode::OutOfFuel) => CallError::OutOfFuel(e.to_string()),
        Some(TrapCode::GrowthOperationLimited) => CallError::MemoryLimit(e.to_string()),
        _ => CallError::Trap(e.to_string()),
    }
}

impl Engine {
    /// An engine with fuel metering and the full import table linked.
    ///
    /// # Errors
    ///
    /// Fails only if the import table is inconsistent, which is a bug.
    pub fn new(limits: EngineLimits) -> Result<Self, String> {
        let mut linker = Linker::<Slot>::new(&shared().engine);
        for (id, (_, name, params, result)) in IMPORTS.iter().enumerate() {
            linker
                .func_new(
                    abi::MODULE,
                    name,
                    signature(params, result),
                    move |caller: Caller<'_, Slot>, params: &[Val], results: &mut [Val]| {
                        run_import(id, &caller, params, results)
                    },
                )
                .map_err(|e| format!("defining import {name}: {e}"))?;
        }
        Ok(Self {
            linker,
            limits,
            modules: RefCell::new(HashMap::new()),
            profile: RefCell::new(None),
        })
    }

    /// Start counting where the time of calls goes (or stop, with `false`); see
    /// [`Self::profiled`].
    pub fn profile(&self, on: bool) {
        *self.profile.borrow_mut() = on.then(|| Rc::new(Cell::new(EngineProfile::default())));
    }

    /// What the profile counted since it started.
    pub fn profiled(&self) -> Option<EngineProfile> {
        self.profile.borrow().as_ref().map(|p| p.get())
    }

    fn count(&self, f: impl FnOnce(&mut EngineProfile)) {
        if let Some(profile) = self.profile.borrow().as_ref() {
            let mut p = profile.get();
            f(&mut p);
            profile.set(p);
        }
    }

    /// Compile (or fetch from the cache) the module with these bytes.
    ///
    /// # Errors
    ///
    /// Fails when the bytes are not a valid module or do not link against the ABI.
    pub fn compile(&self, bytes: &[u8]) -> Result<ModuleHash, CallError> {
        let hash = ModuleHash::of_bytes(bytes);
        if self.modules.borrow().contains_key(&hash) {
            return Ok(hash);
        }
        let process = shared();
        let known = process
            .modules
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&hash)
            .cloned();
        let module = if let Some(module) = known {
            module
        } else {
            let started = Instant::now();
            let module =
                Module::new(&process.engine, bytes).map_err(|e| CallError::Link(e.to_string()))?;
            self.count(|p| p.compiling = p.compiling.saturating_add(started.elapsed()));
            process
                .modules
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(hash.clone(), module.clone());
            module
        };
        let first = self.instantiate(&module)?;
        self.modules.borrow_mut().insert(
            hash.clone(),
            Compiled {
                module,
                idle: vec![first],
            },
        );
        Ok(hash)
    }

    /// The names of the module's exports.
    pub fn exports(&self, module: &ModuleHash) -> Vec<String> {
        self.modules
            .borrow()
            .get(module)
            .map_or_else(Vec::new, |c| {
                c.module.exports().map(|e| e.name().to_owned()).collect()
            })
    }

    /// How many compiled modules the cache holds.
    pub fn cached_modules(&self) -> usize {
        self.modules.borrow().len()
    }

    fn instantiate(&self, module: &Module) -> Result<Pooled, CallError> {
        let limits = StoreLimitsBuilder::new()
            .memory_size(self.limits.max_memory_bytes)
            .trap_on_grow_failure(true)
            .build();
        let mut store = Store::new(
            &shared().engine,
            Slot {
                host: None,
                limits,
                profile: None,
            },
        );
        store.limiter(|slot| &mut slot.limits);
        store
            .set_fuel(self.limits.fuel_per_call)
            .map_err(|e| CallError::Link(e.to_string()))?;
        let instance = self
            .linker
            .instantiate_and_start(&mut store, module)
            .map_err(|e| CallError::Link(e.to_string()))?;
        Ok(Pooled {
            store,
            instance,
            funcs: HashMap::new(),
        })
    }

    fn checkout(&self, module: &ModuleHash) -> Result<(Module, Pooled), CallError> {
        let (compiled, idle) = {
            let mut modules = self.modules.borrow_mut();
            let c = modules
                .get_mut(module)
                .ok_or_else(|| CallError::Link(format!("module {module} is not compiled")))?;
            (c.module.clone(), c.idle.pop())
        };
        let pooled = match idle {
            Some(p) => p,
            None => self.instantiate(&compiled)?,
        };
        Ok((compiled, pooled))
    }

    /// Call `export` of `module` with `args`. Fuel is reset for the call.
    ///
    /// # Errors
    ///
    /// A [`CallError`] for a trap, a suspension, exhausted limits or a missing export.
    pub fn call(
        &self,
        module: &ModuleHash,
        export: &str,
        args: &[Wasm],
        host: Rc<dyn Host>,
    ) -> Result<Option<Wasm>, CallError> {
        let (_, mut pooled) = self.checkout(module)?;
        let profile = self.profile.borrow().clone();
        let started = profile.as_ref().map(|_| Instant::now());
        pooled.store.data_mut().host = Some(host);
        pooled.store.data_mut().profile.clone_from(&profile);
        let outcome = self.run(&mut pooled, export, args);
        pooled.store.data_mut().host = None;
        pooled.store.data_mut().profile = None;
        if let Some(started) = started {
            self.count(|p| {
                p.calls = p.calls.saturating_add(1);
                p.in_calls = p.in_calls.saturating_add(started.elapsed());
            });
        }
        if outcome.is_ok()
            && let Some(c) = self.modules.borrow_mut().get_mut(module)
        {
            c.idle.push(pooled);
        }
        outcome
    }

    fn run(
        &self,
        pooled: &mut Pooled,
        export: &str,
        args: &[Wasm],
    ) -> Result<Option<Wasm>, CallError> {
        pooled
            .store
            .set_fuel(self.limits.fuel_per_call)
            .map_err(|e| CallError::Link(e.to_string()))?;
        let (func, results) = if let Some(found) = pooled.funcs.get(export) {
            found.clone()
        } else {
            let func = pooled
                .instance
                .get_func(&pooled.store, export)
                .ok_or_else(|| CallError::NoExport(export.to_owned()))?;
            let results = func.ty(&pooled.store).results().to_vec();
            pooled
                .funcs
                .insert(export.to_owned(), (func, results.clone()));
            (func, results)
        };
        let inputs: Vec<Val> = args.iter().copied().map(from_wasm).collect();
        let mut outputs: Vec<Val> = results.iter().map(|t| Val::default(*t)).collect();
        func.call(&mut pooled.store, &inputs, &mut outputs)
            .map_err(|e| classify(&e))?;
        outputs
            .first()
            .map(to_wasm)
            .transpose()
            .map_err(|e| CallError::Trap(e.to_string()))
    }
}
