//! The wasm engine. The only module that names wasmi.
//!
//! * Compiled modules are cached by the content hash of their bytes.
//! * Each module normally has one instance. A call that arrives while the module's
//!   instance is already running (a flow in a module invoking an action of the same module)
//!   gets a second instance from the same compiled module, which costs microseconds; guests
//!   keep no state between calls, so instances are interchangeable.
//! * Runaway code is stopped by fuel, a fixed budget per call. Memory growth beyond the
//!   per-instance limit traps.
//! * Imports reach the host through the [`Host`] trait; the engine knows nothing of the ABI
//!   beyond the signatures in [`crate::abi`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use wasmi::errors::HostError;
use wasmi::{
    Caller, Config, Extern, FuncType, Instance, Linker, Module, Store, StoreLimits,
    StoreLimitsBuilder, TrapCode, Val, ValType,
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

struct Slot {
    host: Option<Rc<dyn Host>>,
    limits: StoreLimits,
}

struct Pooled {
    store: Store<Slot>,
    instance: Instance,
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

/// Compiles, caches and calls modules.
pub struct Engine {
    engine: wasmi::Engine,
    linker: Linker<Slot>,
    limits: EngineLimits,
    modules: RefCell<HashMap<ModuleHash, Compiled>>,
}

fn val_type(c: char) -> ValType {
    match c {
        'l' => ValType::I64,
        'f' => ValType::F64,
        _ => ValType::I32,
    }
}

fn signature(params: &str, result: &str) -> FuncType {
    FuncType::new(
        params.chars().map(val_type).collect::<Vec<_>>(),
        result.chars().map(val_type).collect::<Vec<_>>(),
    )
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
    match host.import(id, bytes, &args) {
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
        let mut cfg = Config::default();
        cfg.consume_fuel(true);
        let engine = wasmi::Engine::new(&cfg);
        let mut linker = Linker::<Slot>::new(&engine);
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
            engine,
            linker,
            limits,
            modules: RefCell::new(HashMap::new()),
        })
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
        let module =
            Module::new(&self.engine, bytes).map_err(|e| CallError::Link(e.to_string()))?;
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
        let mut store = Store::new(&self.engine, Slot { host: None, limits });
        store.limiter(|slot| &mut slot.limits);
        store
            .set_fuel(self.limits.fuel_per_call)
            .map_err(|e| CallError::Link(e.to_string()))?;
        let instance = self
            .linker
            .instantiate_and_start(&mut store, module)
            .map_err(|e| CallError::Link(e.to_string()))?;
        Ok(Pooled { store, instance })
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
        pooled.store.data_mut().host = Some(host);
        let outcome = self.run(&mut pooled, export, args);
        pooled.store.data_mut().host = None;
        if outcome.is_ok() {
            if let Some(c) = self.modules.borrow_mut().get_mut(module) {
                c.idle.push(pooled);
            }
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
        let func = pooled
            .instance
            .get_func(&pooled.store, export)
            .ok_or_else(|| CallError::NoExport(export.to_owned()))?;
        let inputs: Vec<Val> = args.iter().copied().map(from_wasm).collect();
        let mut outputs: Vec<Val> = func
            .ty(&pooled.store)
            .results()
            .iter()
            .map(|t| Val::default(*t))
            .collect();
        func.call(&mut pooled.store, &inputs, &mut outputs)
            .map_err(|e| classify(&e))?;
        outputs
            .first()
            .map(to_wasm)
            .transpose()
            .map_err(|e| CallError::Trap(e.to_string()))
    }
}
