//! The wasmi interpreter behind [`Backend`]: the only module that names wasmi.

use std::collections::HashMap;

use tilo_wasm_host::wasmi as shared;
use tilo_wasm_host::{Failure, ImportPolicy};
use wasmi::errors::HostError;
use wasmi::{
    Caller, Extern, Func, FuncType, Instance, Linker, Memory, Module, ResourceLimiter, Store,
    StoreLimits, Val, ValType,
};

use super::{
    Backend, CallError, Cx, EngineLimits, HostFail, Imports, Translation, Unwind, Wasm,
    guest_answer, guest_import, import,
};
use crate::abi::{self, IMPORTS};

pub(super) struct Wasmi {
    engine: wasmi::Engine,
}

impl Wasmi {
    pub(super) fn new(translation: Translation) -> Self {
        Self {
            engine: shared::engine(translation),
        }
    }
}

pub(super) struct Slot {
    cx: Option<Cx>,
    limits: StoreLimits,
    /// The first module's exported memory, once it is made: what an import answers in. In a
    /// linked set the calling code may be a later module's, which exports no memory of its
    /// own but shares this one.
    memory: Option<Memory>,
}

impl shared::Limited for Slot {
    fn limits(&mut self) -> &mut dyn ResourceLimiter {
        &mut self.limits
    }
}

pub(super) struct Pooled {
    store: Store<Slot>,
    /// The module's instance, or a linked set's, in the order they were made.
    instances: Vec<Instance>,
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
    let memory = caller
        .data()
        .memory
        .or_else(|| caller.get_export("memory").and_then(Extern::into_memory));
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

fn run_guest_import(
    module: &str,
    name: &str,
    mut caller: Caller<'_, Slot>,
    params: &[Val],
    results: &mut [Val],
) -> Result<(), wasmi::Error> {
    let args = params.iter().map(to_wasm).collect::<Result<Vec<_>, _>>()?;
    let memory = caller
        .data()
        .memory
        .or_else(|| caller.get_export("memory").and_then(Extern::into_memory));
    let answer = {
        let bytes: &[u8] = memory.as_ref().map_or(&[][..], |m| m.data(&caller));
        guest_import(caller.data().cx.as_ref(), module, name, bytes, &args)
            .map_err(|fail| wasmi::Error::host(Unwind(fail)))?
    };
    let (value, written) = guest_answer(answer);
    if let Some((offset, bytes)) = written {
        let Some(memory) = memory else {
            return Err(wasmi::Error::host(Unwind(HostFail::Trap(
                "the module has no memory to answer in".into(),
            ))));
        };
        let data = memory.data_mut(&mut caller);
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let end = start
            .checked_add(bytes.len())
            .filter(|&end| end <= data.len())
            .ok_or_else(|| {
                wasmi::Error::host(Unwind(HostFail::Trap(
                    "the answer does not fit the module's memory".into(),
                )))
            })?;
        data.get_mut(start..end)
            .unwrap_or_default()
            .copy_from_slice(&bytes);
    }
    if let (Some(slot), Some(w)) = (results.first_mut(), value) {
        *slot = from_wasm(w);
    }
    Ok(())
}

fn classify(e: &wasmi::Error) -> CallError {
    if let Some(unwind) = e.downcast_ref::<Unwind>() {
        return unwind.into();
    }
    shared::classify(e).into()
}

impl Backend for Wasmi {
    type Module = Module;
    type Linker = Linker<Slot>;
    type Instance = Pooled;

    fn compile(&self, bytes: &[u8]) -> Result<Module, Failure> {
        shared::compile(&self.engine, bytes, ImportPolicy::Any)
    }

    fn exports(module: &Module) -> Vec<String> {
        module.exports().map(|e| e.name().to_owned()).collect()
    }

    fn linker(&self, imports: &[Imports]) -> Result<Linker<Slot>, String> {
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
        for &(module, name, params, result) in imports.get(IMPORTS.len()..).unwrap_or_default() {
            let ty = FuncType::new(params.bytes().map(val_type), result.bytes().map(val_type));
            linker
                .func_new(
                    module,
                    name,
                    ty,
                    move |caller: Caller<'_, Slot>, params: &[Val], results: &mut [Val]| {
                        run_guest_import(module, name, caller, params, results)
                    },
                )
                .map_err(|e| format!("defining import {module}::{name}: {e}"))?;
        }
        Ok(linker)
    }

    fn instantiate(
        &self,
        linker: &Linker<Slot>,
        modules: &[Module],
        limits: &EngineLimits,
    ) -> Result<Pooled, Failure> {
        let mut store = shared::store(
            &self.engine,
            Slot {
                cx: None,
                limits: shared::store_limits(limits),
                memory: None,
            },
            limits,
        )?;
        let (first, rest) = modules
            .split_first()
            .ok_or_else(|| Failure::Link("no module to instantiate".into()))?;
        let first = linker
            .instantiate_and_start(&mut store, first)
            .map_err(|error| shared::classify(&error))?;
        store.data_mut().memory = first.get_memory(&store, "memory");
        let mut instances = vec![first];
        for module in rest {
            let imports = module
                .imports()
                .map(|import| {
                    instances
                        .iter()
                        .rev()
                        .find_map(|instance| instance.get_export(&store, import.name()))
                        .ok_or_else(|| {
                            Failure::Link(format!(
                                "no module before it exports {}::{}",
                                import.module(),
                                import.name()
                            ))
                        })
                })
                .collect::<Result<Vec<Extern>, _>>()?;
            let instance = Instance::new(&mut store, module, &imports)
                .map_err(|error| shared::classify(&error))?;
            instances.push(instance);
        }
        Ok(Pooled {
            store,
            instances,
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
        // What the call spent, charged to the unit that made it: a call that ran out spent it
        // all.
        let spent = fuel.saturating_sub(pooled.store.get_fuel().unwrap_or(0));
        let cx = pooled.store.data_mut().cx.take();
        if let Some(costs) = cx.and_then(|cx| cx.costs) {
            costs.fuel(spent);
        }
        outcome
    }
}

fn run(
    pooled: &mut Pooled,
    export: &str,
    args: &[Wasm],
    fuel: u64,
) -> Result<Option<Wasm>, CallError> {
    shared::refuel(&mut pooled.store, fuel)?;
    let (func, results) = if let Some(found) = pooled.funcs.get(export) {
        found.clone()
    } else {
        let func = pooled
            .instances
            .iter()
            .rev()
            .find_map(|instance| instance.get_func(&pooled.store, export))
            .ok_or_else(|| Failure::NoExport(export.to_owned()))?;
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
        .map_err(|e| CallError::from(Failure::Trap(e.to_string())))
}
