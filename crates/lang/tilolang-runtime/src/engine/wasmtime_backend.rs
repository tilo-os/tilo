//! wasmtime behind [`Backend`], compiling with Cranelift or Winch: the only module that names
//! wasmtime. Built with the `wasmtime` feature, to compare engines.

use std::collections::HashMap;

use wasmtime::{
    Caller, Config, Extern, Func, FuncType, Instance, Linker, Module, ResourceLimiter, Store,
    Strategy, Trap, Val, ValType,
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
            EngineKind::Wasmi | EngineKind::Cranelift => Strategy::Cranelift,
        });
        cfg.consume_fuel(true);
        // A trap reports what wasm says, as wasmi does, without walking the guest's stack.
        cfg.wasm_backtrace_max_frames(None);
        let engine = wasmtime::Engine::new(&cfg)
            .map_err(|e| format!("cannot make the {} engine: {e}", kind.name()))?;
        Ok(Self { engine })
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
}

pub(super) struct Pooled {
    store: Store<Slot>,
    instance: Instance,
    /// Export handles and result types, resolved on first use.
    funcs: HashMap<String, (Func, Vec<ValType>)>,
}

fn val_type(c: u8) -> ValType {
    match c {
        b'l' => ValType::I64,
        b'f' => ValType::F64,
        _ => ValType::I32,
    }
}

fn to_wasm(v: &Val) -> Result<Wasm, CallError> {
    match v {
        Val::I32(i) => Ok(Wasm::I32(*i)),
        Val::I64(i) => Ok(Wasm::I64(*i)),
        Val::F64(bits) => Ok(Wasm::F64(f64::from_bits(*bits))),
        _ => Err(CallError::Trap("unsupported wasm value type".into())),
    }
}

fn from_wasm(w: Wasm) -> Val {
    match w {
        Wasm::I32(i) => Val::I32(i),
        Wasm::I64(i) => Val::I64(i),
        Wasm::F64(f) => Val::F64(f.to_bits()),
    }
}

fn zero(ty: &ValType) -> Val {
    match ty {
        ValType::I64 => Val::I64(0),
        ValType::F32 => Val::F32(0),
        ValType::F64 => Val::F64(0),
        _ => Val::I32(0),
    }
}

fn run_import(
    id: usize,
    mut caller: Caller<'_, Slot>,
    params: &[Val],
    results: &mut [Val],
) -> wasmtime::Result<()> {
    let args = params
        .iter()
        .map(to_wasm)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| wasmtime::Error::msg(e.to_string()))?;
    let memory = caller.get_export("memory").and_then(Extern::into_memory);
    let bytes: &[u8] = memory.as_ref().map_or(&[][..], |m| m.data(&caller));
    match import(caller.data().cx.as_ref(), id, bytes, &args) {
        Ok(out) => {
            if let (Some(slot), Some(w)) = (results.first_mut(), out) {
                *slot = from_wasm(w);
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
            linker
                .func_new(
                    abi::MODULE,
                    name,
                    ty,
                    move |caller: Caller<'_, Slot>, params: &[Val], results: &mut [Val]| {
                        run_import(id, caller, params, results)
                    },
                )
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
            },
        );
        store.limiter(|slot| &mut slot.limits);
        store
            .set_fuel(limits.fuel_per_call)
            .map_err(|e| CallError::Link(e.to_string()))?;
        let instance = linker
            .instantiate(&mut store, module)
            .map_err(|e| CallError::Link(e.to_string()))?;
        Ok(Pooled {
            store,
            instance,
            funcs: HashMap::new(),
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
    let (func, results) = if let Some(found) = pooled.funcs.get(export) {
        found.clone()
    } else {
        let func = pooled
            .instance
            .get_func(&mut pooled.store, export)
            .ok_or_else(|| CallError::NoExport(export.to_owned()))?;
        let results: Vec<ValType> = func.ty(&pooled.store).results().collect();
        pooled
            .funcs
            .insert(export.to_owned(), (func, results.clone()));
        (func, results)
    };
    let inputs: Vec<Val> = args.iter().copied().map(from_wasm).collect();
    let mut outputs: Vec<Val> = results.iter().map(zero).collect();
    func.call(&mut pooled.store, &inputs, &mut outputs)
        .map_err(|e| classify(&e))?;
    outputs.first().map(to_wasm).transpose()
}
