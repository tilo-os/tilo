//! wasmtime behind [`Backend`], compiling with Cranelift or Winch: the only module that names
//! wasmtime. It is the tiered engine's compiled tier, and runs alone to compare engines.
//! Imports and calls pass raw values (`ValRaw`), with a call's arguments checked against the
//! export's type, instead of converting every value through `Val`.

use std::collections::HashMap;
use std::mem::MaybeUninit;

use wasmtime::{
    Caller, Config, Func, FuncType, Instance, Linker, Memory, Module, ResourceLimiter, Store,
    Strategy, Trap, ValRaw, ValType,
};

use super::{
    Backend, CallError, Cx, EngineKind, EngineLimits, GROWTH_LIMITED, OUT_OF_FUEL, Unwind, Wasm,
    import, trap_text,
};
use crate::abi::{self, IMPORTS};

pub(super) struct Wasmtime {
    engine: wasmtime::Engine,
}

impl Wasmtime {
    pub(super) fn new(kind: EngineKind) -> Result<Self, String> {
        let mut cfg = Config::new();
        cfg.strategy(match kind {
            EngineKind::Winch => Strategy::Winch,
            EngineKind::Tiered | EngineKind::Wasmi | EngineKind::Cranelift => Strategy::Cranelift,
        });
        cfg.consume_fuel(true);
        // A trap reports what wasm says, as wasmi does, without walking the guest's stack.
        cfg.wasm_backtrace_max_frames(None);
        let engine = wasmtime::Engine::new(&cfg)
            .map_err(|e| format!("cannot make the {} engine: {e}", kind.name()))?;
        Ok(Self { engine })
    }

    pub(super) const fn engine(&self) -> &wasmtime::Engine {
        &self.engine
    }

    /// A module [`Module::serialize`] wrote, by this engine or one of the same version,
    /// configuration and CPU features; wasmtime refuses any other.
    pub(super) fn deserialize(&self, serialized: &[u8]) -> Result<Module, String> {
        // SAFETY: wasmtime runs what the bytes hold as machine code, so they must be what
        // `Module::serialize` wrote. The tiering's disk cache hands over only an entry whose
        // digest matches, from the directory this process keeps its own compiled modules in;
        // wasmtime checks the version, configuration and CPU features it was written for.
        unsafe { Module::deserialize(&self.engine, serialized) }.map_err(|e| e.to_string())
    }
}

/// Memory and table growth past what the instance may have traps, as wasmi's
/// `trap_on_grow_failure` does.
struct Limits {
    max_memory_bytes: usize,
}

/// A growth [`Limits`] refused.
#[derive(Debug)]
struct GrowthLimited;

impl std::fmt::Display for GrowthLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(GROWTH_LIMITED)
    }
}

impl std::error::Error for GrowthLimited {}

impl ResourceLimiter for Limits {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > self.max_memory_bytes || maximum.is_some_and(|max| desired > max) {
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
}

pub(super) struct Slot {
    cx: Option<Cx>,
    limits: Limits,
    /// The instance's exported memory, found once when it is made.
    memory: Option<Memory>,
}

/// An export's handle and type, resolved on its first call.
#[derive(Clone)]
struct Export {
    func: Func,
    params: Vec<ValType>,
    results: usize,
    result: Option<ValType>,
}

pub(super) struct Pooled {
    store: Store<Slot>,
    instance: Instance,
    funcs: HashMap<String, Export>,
    /// The parameters and results of a call, reused from call to call.
    raw: Vec<ValRaw>,
}

fn val_type(c: u8) -> ValType {
    match c {
        b'l' => ValType::I64,
        b'f' => ValType::F64,
        _ => ValType::I32,
    }
}

fn to_raw(w: Wasm) -> ValRaw {
    match w {
        Wasm::I32(i) => ValRaw::i32(i),
        Wasm::I64(i) => ValRaw::i64(i),
        Wasm::F64(f) => ValRaw::f64(f.to_bits()),
    }
}

/// The value of type `ty` in `raw`.
fn wasm(ty: u8, raw: ValRaw) -> Wasm {
    match ty {
        b'l' => Wasm::I64(raw.get_i64()),
        b'f' => Wasm::F64(f64::from_bits(raw.get_f64())),
        _ => Wasm::I32(raw.get_i32()),
    }
}

/// Whether `w` is a value of type `ty`.
fn fits(w: &Wasm, ty: &ValType) -> bool {
    matches!(
        (w, ty),
        (Wasm::I32(_), ValType::I32) | (Wasm::I64(_), ValType::I64) | (Wasm::F64(_), ValType::F64)
    )
}

/// Import `id`, whose parameters have the types `params` (`i`, `l`, `f`), with its parameters
/// in `values` and its result written there.
fn run_import(
    id: usize,
    params: &str,
    caller: &Caller<'_, Slot>,
    values: &mut [MaybeUninit<ValRaw>],
) -> wasmtime::Result<()> {
    let args: Vec<Wasm> = params
        .bytes()
        .zip(values.iter())
        // SAFETY: wasmtime writes every parameter of the import's type before calling it.
        .map(|(ty, value)| wasm(ty, unsafe { value.assume_init() }))
        .collect();
    let bytes: &[u8] = caller
        .data()
        .memory
        .as_ref()
        .map_or(&[][..], |m| m.data(caller));
    match import(caller.data().cx.as_ref(), id, bytes, &args) {
        Ok(out) => {
            if let (Some(slot), Some(w)) = (values.first_mut(), out) {
                *slot = MaybeUninit::new(to_raw(w));
            }
            Ok(())
        }
        Err(fail) => Err(wasmtime::Error::new(Unwind(fail))),
    }
}

fn classify(e: &wasmtime::Error) -> CallError {
    if let Some(unwind) = e.downcast_ref::<Unwind>() {
        return unwind.into();
    }
    if e.downcast_ref::<GrowthLimited>().is_some() {
        return CallError::MemoryLimit(GROWTH_LIMITED.into());
    }
    match e.downcast_ref::<Trap>() {
        Some(Trap::OutOfFuel) => CallError::OutOfFuel(OUT_OF_FUEL.into()),
        Some(trap) => CallError::Trap(trap_text(&trap.to_string())),
        None => CallError::Trap(e.to_string()),
    }
}

impl Backend for Wasmtime {
    type Module = Module;
    type Linker = Linker<Slot>;
    type Instance = Pooled;

    fn compile(&self, bytes: &[u8]) -> Result<Module, String> {
        Module::new(&self.engine, bytes).map_err(|e| e.to_string())
    }

    fn exports(module: &Module) -> Vec<String> {
        module.exports().map(|e| e.name().to_owned()).collect()
    }

    fn linker(&self) -> Result<Linker<Slot>, String> {
        let mut linker = Linker::<Slot>::new(&self.engine);
        for (id, (_, name, params, result)) in IMPORTS.iter().enumerate() {
            let ty = FuncType::new(
                &self.engine,
                params.bytes().map(val_type),
                result.bytes().map(val_type),
            );
            let params: &'static str = params;
            // SAFETY: `run_import` reads exactly the parameters of `ty` and writes at most its
            // one result, of the type `ty` declares (the host's import answers with the
            // result type of `IMPORTS`).
            unsafe {
                linker.func_new_unchecked(
                    abi::MODULE,
                    name,
                    ty,
                    move |caller: Caller<'_, Slot>, values: &mut [MaybeUninit<ValRaw>]| {
                        run_import(id, params, &caller, values)
                    },
                )
            }
            .map_err(|e| format!("defining import {name}: {e}"))?;
        }
        Ok(linker)
    }

    fn instantiate(
        &self,
        linker: &Linker<Slot>,
        module: &Module,
        limits: &EngineLimits,
    ) -> Result<Pooled, CallError> {
        let mut store = Store::new(
            &self.engine,
            Slot {
                cx: None,
                limits: Limits {
                    max_memory_bytes: limits.max_memory_bytes,
                },
                memory: None,
            },
        );
        store.limiter(|slot| &mut slot.limits);
        store
            .set_fuel(limits.fuel_per_call)
            .map_err(|e| CallError::Link(e.to_string()))?;
        let instance = linker
            .instantiate(&mut store, module)
            .map_err(|e| CallError::Link(e.to_string()))?;
        store.data_mut().memory = instance.get_memory(&mut store, "memory");
        Ok(Pooled {
            store,
            instance,
            funcs: HashMap::new(),
            raw: Vec::new(),
        })
    }

    fn call(
        pooled: &mut Pooled,
        export: &str,
        args: &[Wasm],
        fuel: u64,
        cx: Cx,
    ) -> Result<Option<Wasm>, CallError> {
        pooled.store.data_mut().cx = Some(cx);
        let outcome = run(pooled, export, args, fuel);
        pooled.store.data_mut().cx = None;
        outcome
    }
}

fn run(
    pooled: &mut Pooled,
    export: &str,
    args: &[Wasm],
    fuel: u64,
) -> Result<Option<Wasm>, CallError> {
    pooled
        .store
        .set_fuel(fuel)
        .map_err(|e| CallError::Link(e.to_string()))?;
    let found = if let Some(found) = pooled.funcs.get(export) {
        found.clone()
    } else {
        let func = pooled
            .instance
            .get_func(&mut pooled.store, export)
            .ok_or_else(|| CallError::NoExport(export.to_owned()))?;
        let ty = func.ty(&pooled.store);
        let found = Export {
            func,
            params: ty.params().collect(),
            results: ty.results().len(),
            result: ty.results().next(),
        };
        pooled.funcs.insert(export.to_owned(), found.clone());
        found
    };
    if args.len() != found.params.len()
        || !args.iter().zip(&found.params).all(|(a, ty)| fits(a, ty))
    {
        return Err(CallError::Trap(format!(
            "`{export}` takes {:?}, not {args:?}",
            found.params
        )));
    }
    pooled.raw.clear();
    pooled.raw.extend(args.iter().copied().map(to_raw));
    pooled
        .raw
        .resize(args.len().max(found.results), ValRaw::i32(0));
    // SAFETY: the parameters were just checked against the export's type, and the buffer holds
    // its parameters and its results; no value is a reference.
    unsafe {
        found.func.call_unchecked(
            &mut pooled.store,
            std::ptr::from_mut(pooled.raw.as_mut_slice()),
        )
    }
    .map_err(|e| classify(&e))?;
    Ok(match (found.result, pooled.raw.first()) {
        (Some(ValType::I64), Some(r)) => Some(Wasm::I64(r.get_i64())),
        (Some(ValType::F64), Some(r)) => Some(Wasm::F64(f64::from_bits(r.get_f64()))),
        (Some(ValType::I32), Some(r)) => Some(Wasm::I32(r.get_i32())),
        (Some(_), _) => return Err(CallError::Trap("unsupported wasm value type".into())),
        (None, _) => None,
    })
}
