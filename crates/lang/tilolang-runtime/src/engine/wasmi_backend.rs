//! The wasmi interpreter behind [`Backend`]: the only module that names wasmi.

use std::collections::HashMap;

use wasmi::errors::HostError;
use wasmi::{
    Caller, CompilationMode, Config, Extern, Func, FuncType, Instance, Linker, Module, Store,
    StoreLimits, StoreLimitsBuilder, TrapCode, Val, ValType,
};

use super::{
    Backend, CallError, Cx, EngineLimits, GROWTH_LIMITED, OUT_OF_FUEL, Translation, Unwind, Wasm,
    import, trap_text,
};
use crate::abi::{self, IMPORTS};

pub(super) struct Wasmi {
    engine: wasmi::Engine,
}

impl Wasmi {
    pub(super) fn new(translation: Translation) -> Self {
        let mut cfg = Config::default();
        cfg.consume_fuel(true);
        cfg.compilation_mode(match translation {
            Translation::Lazy => CompilationMode::LazyTranslation,
            Translation::Eager => CompilationMode::Eager,
        });
        Self {
            engine: wasmi::Engine::new(&cfg),
        }
    }
}

pub(super) struct Slot {
    cx: Option<Cx>,
    limits: StoreLimits,
}

pub(super) struct Pooled {
    store: Store<Slot>,
    instance: Instance,
    /// Export handles and result types, resolved on first use.
    funcs: HashMap<String, (Func, Vec<ValType>)>,
}

impl HostError for Unwind {}

fn val_type(c: u8) -> ValType {
    match c {
        b'l' => ValType::I64,
        b'f' => ValType::F64,
        _ => ValType::I32,
    }
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
    let args = params.iter().map(to_wasm).collect::<Result<Vec<_>, _>>()?;
    let memory = caller.get_export("memory").and_then(Extern::into_memory);
    let bytes: &[u8] = memory.as_ref().map_or(&[][..], |m| m.data(caller));
    match import(caller.data().cx.as_ref(), id, bytes, &args) {
        Ok(out) => {
            if let (Some(slot), Some(w)) = (results.first_mut(), out) {
                *slot = from_wasm(w);
            }
            Ok(())
        }
        Err(fail) => Err(wasmi::Error::host(Unwind(fail))),
    }
}

fn classify(e: &wasmi::Error) -> CallError {
    if let Some(unwind) = e.downcast_ref::<Unwind>() {
        return unwind.into();
    }
    match e.as_trap_code() {
        Some(TrapCode::OutOfFuel) => CallError::OutOfFuel(OUT_OF_FUEL.into()),
        Some(TrapCode::GrowthOperationLimited) => CallError::MemoryLimit(GROWTH_LIMITED.into()),
        Some(code) => CallError::Trap(trap_text(code.trap_message())),
        None => CallError::Trap(e.to_string()),
    }
}

impl Backend for Wasmi {
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
            let ty = FuncType::new(params.bytes().map(val_type), result.bytes().map(val_type));
            linker
                .func_new(
                    abi::MODULE,
                    name,
                    ty,
                    move |caller: Caller<'_, Slot>, params: &[Val], results: &mut [Val]| {
                        run_import(id, &caller, params, results)
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
        let store_limits = StoreLimitsBuilder::new()
            .memory_size(limits.max_memory_bytes)
            .trap_on_grow_failure(true)
            .build();
        let mut store = Store::new(
            &self.engine,
            Slot {
                cx: None,
                limits: store_limits,
            },
        );
        store.limiter(|slot| &mut slot.limits);
        store
            .set_fuel(limits.fuel_per_call)
            .map_err(|e| CallError::Link(e.to_string()))?;
        let instance = linker
            .instantiate_and_start(&mut store, module)
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
