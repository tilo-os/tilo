//! wasmtime behind [`Backend`], compiling with Cranelift or Winch: the only module that names
//! wasmtime. It is the tiered engine's compiled tier, and runs alone to compare engines.
//! Imports and calls pass raw values (`ValRaw`), with a call's arguments checked against the
//! export's type, instead of converting every value through `Val`.

use std::collections::HashMap;
use std::mem::MaybeUninit;

use super::wasmtime_host as shared;
use tilo_wasm_host::{Failure, ImportPolicy};
use wasmtime::{
    Caller, Func, FuncType, Instance, Linker, Memory, Module, ResourceLimiter, Store, ValRaw,
    ValType,
};

use super::{
    Backend, CallError, Cx, EngineKind, EngineLimits, HostFail, Imports, Unwind, Wasm,
    guest_answer, guest_import, import,
};
use crate::abi::{self, IMPORTS};

pub(super) struct Wasmtime {
    engine: shared::Engine,
}

impl Wasmtime {
    pub(super) fn new(kind: EngineKind) -> Result<Self, String> {
        let strategy = match kind {
            EngineKind::Winch => shared::Strategy::Winch,
            EngineKind::Tiered | EngineKind::Wasmi | EngineKind::Cranelift => {
                shared::Strategy::Cranelift
            }
        };
        let engine = shared::engine(strategy)
            .map_err(|error| format!("cannot make the {} engine: {error}", kind.name()))?;
        Ok(Self { engine })
    }

    pub(super) fn engine(&self) -> &wasmtime::Engine {
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

pub(super) struct Slot {
    cx: Option<Cx>,
    limits: shared::GrowthLimits,
    /// The instance's exported memory, found once when it is made.
    memory: Option<Memory>,
}

impl shared::Limited for Slot {
    fn limits(&mut self) -> &mut dyn ResourceLimiter {
        &mut self.limits
    }
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
    /// The module's instance, or a linked set's, in the order they were made.
    instances: Vec<Instance>,
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

fn run_guest_import(
    module: &str,
    name: &str,
    params: &str,
    mut caller: Caller<'_, Slot>,
    values: &mut [MaybeUninit<ValRaw>],
) -> wasmtime::Result<()> {
    let args: Vec<Wasm> = params
        .bytes()
        .zip(values.iter())
        // SAFETY: wasmtime writes every parameter of the import's type before calling it.
        .map(|(ty, value)| wasm(ty, unsafe { value.assume_init() }))
        .collect();
    let answer = {
        let bytes: &[u8] = caller
            .data()
            .memory
            .as_ref()
            .map_or(&[][..], |m| m.data(&caller));
        guest_import(caller.data().cx.as_ref(), module, name, bytes, &args)
            .map_err(|fail| wasmtime::Error::new(Unwind(fail)))?
    };
    let (value, written) = guest_answer(answer);
    if let Some((offset, bytes)) = written {
        let Some(memory) = caller.data().memory else {
            return Err(wasmtime::Error::new(Unwind(HostFail::Trap(
                "the module has no memory to answer in".into(),
            ))));
        };
        let data = memory.data_mut(&mut caller);
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let end = start
            .checked_add(bytes.len())
            .filter(|&end| end <= data.len())
            .ok_or_else(|| {
                wasmtime::Error::new(Unwind(HostFail::Trap(
                    "the answer does not fit the module's memory".into(),
                )))
            })?;
        data.get_mut(start..end)
            .unwrap_or_default()
            .copy_from_slice(&bytes);
    }
    if let (Some(slot), Some(w)) = (values.first_mut(), value) {
        *slot = MaybeUninit::new(to_raw(w));
    }
    Ok(())
}

fn classify(e: &wasmtime::Error) -> CallError {
    if let Some(unwind) = e.downcast_ref::<Unwind>() {
        return unwind.into();
    }
    shared::classify(e).into()
}

impl Backend for Wasmtime {
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
        for &(module, name, params, result) in imports.get(IMPORTS.len()..).unwrap_or_default() {
            let ty = FuncType::new(
                &self.engine,
                params.bytes().map(val_type),
                result.bytes().map(val_type),
            );
            // SAFETY: as above, for the import of the host's own ABI.
            unsafe {
                linker.func_new_unchecked(
                    module,
                    name,
                    ty,
                    move |caller: Caller<'_, Slot>, values: &mut [MaybeUninit<ValRaw>]| {
                        run_guest_import(module, name, params, caller, values)
                    },
                )
            }
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
        // Instantiation calls each module's start function inside the guest.
        let _starting = shared::in_call(&self.engine);
        let mut store = shared::store(
            &self.engine,
            Slot {
                cx: None,
                limits: shared::GrowthLimits::new(*limits),
                memory: None,
            },
            limits,
        )?;
        let (first, rest) = modules
            .split_first()
            .ok_or_else(|| Failure::Link("no module to instantiate".into()))?;
        let instance = linker
            .instantiate(&mut store, first)
            .map_err(|error| shared::classify(&error))?;
        // The imports the engine links are the first module's, and answer in its memory.
        store.data_mut().memory = instance.get_memory(&mut store, "memory");
        let mut instances = vec![instance];
        for module in rest {
            let mut imports = Vec::new();
            for import in module.imports() {
                let export = instances
                    .iter()
                    .rev()
                    .find_map(|instance| instance.get_export(&mut store, import.name()))
                    .ok_or_else(|| {
                        Failure::Link(format!(
                            "no module before it exports {}::{}",
                            import.module(),
                            import.name()
                        ))
                    })?;
                imports.push(export);
            }
            let instance = Instance::new(&mut store, module, &imports)
                .map_err(|error| shared::classify(&error))?;
            instances.push(instance);
        }
        Ok(Pooled {
            store,
            instances,
            funcs: HashMap::new(),
            raw: Vec::new(),
        })
    }

    fn call(
        &self,
        pooled: &mut Pooled,
        export: &str,
        args: &[Wasm],
        limits: &EngineLimits,
        cx: Cx,
    ) -> Result<Option<Wasm>, CallError> {
        pooled.store.data_mut().cx = Some(cx);
        let outcome = call_export(&self.engine, pooled, export, args, limits);
        // What the call spent, charged to the unit that made it: a call that ran out spent it
        // all.
        let spent = limits
            .fuel_per_call
            .saturating_sub(pooled.store.get_fuel().unwrap_or(0));
        let cx = pooled.store.data_mut().cx.take();
        if let Some(costs) = cx.and_then(|cx| cx.costs) {
            costs.fuel(spent);
        }
        outcome
    }
}

fn call_export(
    engine: &shared::Engine,
    pooled: &mut Pooled,
    export: &str,
    args: &[Wasm],
    limits: &EngineLimits,
) -> Result<Option<Wasm>, CallError> {
    let _call = shared::begin(engine, &mut pooled.store, limits)?;
    let found = if let Some(found) = pooled.funcs.get(export) {
        found.clone()
    } else {
        let func = pooled
            .instances
            .iter()
            .rev()
            .find_map(|instance| instance.get_func(&mut pooled.store, export))
            .ok_or_else(|| Failure::NoExport(export.to_owned()))?;
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
        return Err(CallError::from(Failure::Trap(format!(
            "`{export}` takes {:?}, not {args:?}",
            found.params
        ))));
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
        (Some(_), _) => {
            return Err(CallError::from(Failure::Trap(
                "unsupported wasm value type".to_owned(),
            )));
        }
        (None, _) => None,
    })
}
