//! The tiered engine: a module runs before its compile finishes, moves to compiled code between
//! calls, gives the same results on both tiers, starts compiled from the disk cache, refuses a
//! damaged cache entry, and runs out of fuel and memory the same way on both.
//!
//! Each test makes tiers of its own, as a process of its own would, so tests neither share
//! compiled modules nor wait on each other's compiles.

#![allow(
    clippy::unwrap_used,
    clippy::unreachable,
    reason = "test assertions: a failure is the test failing"
)]

use std::path::Path;
use std::rc::Rc;

use super::{
    CallError, Engine, EngineKind, EngineLimits, GROWTH_LIMITED, Host, HostFail, OUT_OF_FUEL,
    Shared, Tiers, Translation, Wasm, tiering,
};
use crate::ids::ModuleHash;

/// A module without imports: arithmetic, a loop, a loop that never ends, and memory growth.
const MODULE: &str = r#"
(module
  (memory (export "memory") 1)
  (func (export "add") (param i32 i32) (result i32)
    local.get 0
    local.get 1
    i32.add)
  ;; sum over i < n of (i * 2654435761) xor (i >> 3), wrapping
  (func (export "mix") (param $n i64) (result i64)
    (local $i i64) (local $acc i64)
    (block $done
      (loop $next
        (br_if $done (i64.ge_u (local.get $i) (local.get $n)))
        (local.set $acc
          (i64.add (local.get $acc)
            (i64.xor (i64.mul (local.get $i) (i64.const 2654435761))
                     (i64.shr_u (local.get $i) (i64.const 3)))))
        (local.set $i (i64.add (local.get $i) (i64.const 1)))
        (br $next)))
    local.get $acc)
  (func (export "spin")
    (loop $forever (br $forever)))
  (func (export "grow") (result i32)
    (memory.grow (i32.const 4096)))
)
"#;

struct NoImports;

impl Host for NoImports {
    fn import(&self, _: usize, _: &[u8], _: &[Wasm]) -> Result<Option<Wasm>, HostFail> {
        Err(HostFail::Trap("this module has no imports".into()))
    }
}

fn limits() -> EngineLimits {
    EngineLimits {
        fuel_per_call: 10_000_000,
        max_memory_bytes: 1 << 24,
    }
}

/// The tiered engine of a process of its own, keeping compiled modules in `dir` if one is
/// given.
struct Process(&'static Shared);

impl Process {
    fn new(dir: Option<&Path>) -> Self {
        let tiers = Tiers::new(Translation::Lazy).unwrap();
        if let Some(dir) = dir {
            tiers.top.keep_in(dir.to_owned()).unwrap();
        }
        Self(Box::leak(Box::new(Shared::Tiered(Box::new(tiers)))))
    }

    fn top(&self) -> &'static tiering::Tiering {
        match self.0 {
            Shared::Tiered(tiers) => &tiers.top,
            _ => unreachable!("made tiered"),
        }
    }

    /// A runtime's engine in this process.
    fn engine(&self) -> Engine {
        Engine::on(self.0, EngineKind::Tiered, limits()).unwrap()
    }
}

fn call(
    engine: &Engine,
    module: &ModuleHash,
    export: &str,
    args: &[Wasm],
) -> Result<Option<Wasm>, CallError> {
    engine.call(module, export, args, Rc::new(NoImports))
}

fn bytes() -> Vec<u8> {
    wat::parse_str(MODULE).unwrap()
}

#[test]
fn a_module_runs_before_its_compile_finishes_and_moves_to_compiled_code_between_calls() {
    let process = Process::new(None);
    process.top().pause(true);
    let engine = process.engine();
    let module = engine.compile(&bytes()).unwrap();

    // The compile is held: the module runs on wasmi.
    assert_eq!(engine.tier(&module), Some(EngineKind::Wasmi));
    let interpreted = call(&engine, &module, "mix", &[Wasm::I64(100_000)]).unwrap();
    assert_eq!(
        call(&engine, &module, "add", &[Wasm::I32(2), Wasm::I32(40)]).unwrap(),
        Some(Wasm::I32(42))
    );
    assert_eq!(engine.tier(&module), Some(EngineKind::Wasmi));

    process.top().pause(false);
    engine.settle_compile(&module);
    assert_eq!(engine.compile_failure(&module), None);
    // Ready, but nothing moves until the next call.
    assert_eq!(engine.tier(&module), Some(EngineKind::Wasmi));
    let compiled = call(&engine, &module, "mix", &[Wasm::I64(100_000)]).unwrap();
    assert_eq!(engine.tier(&module), Some(EngineKind::Cranelift));
    assert_eq!(compiled, interpreted);
    assert_eq!(
        call(&engine, &module, "add", &[Wasm::I32(2), Wasm::I32(40)]).unwrap(),
        Some(Wasm::I32(42))
    );
}

#[test]
fn the_results_are_the_same_on_both_tiers() {
    let process = Process::new(None);
    process.top().pause(true);
    let engine = process.engine();
    let module = engine.compile(&bytes()).unwrap();
    let inputs = [0_i64, 1, 7, 1000, 123_457];
    let before: Vec<_> = inputs
        .iter()
        .map(|n| call(&engine, &module, "mix", &[Wasm::I64(*n)]))
        .collect();
    process.top().pause(false);
    engine.settle_compile(&module);
    let after: Vec<_> = inputs
        .iter()
        .map(|n| call(&engine, &module, "mix", &[Wasm::I64(*n)]))
        .collect();
    assert_eq!(engine.tier(&module), Some(EngineKind::Cranelift));
    assert_eq!(before, after);
    let expected: i64 = (0..123_457_i64)
        .map(|i| i.wrapping_mul(2_654_435_761) ^ (i >> 3))
        .fold(0, i64::wrapping_add);
    assert_eq!(after.last(), Some(&Ok(Some(Wasm::I64(expected)))));
}

#[test]
fn running_out_of_fuel_or_memory_fails_the_same_on_both_tiers() {
    let process = Process::new(None);
    process.top().pause(true);
    let engine = process.engine();
    let module = engine.compile(&bytes()).unwrap();
    let on_wasmi = (
        call(&engine, &module, "spin", &[]),
        call(&engine, &module, "grow", &[]),
    );
    assert_eq!(engine.tier(&module), Some(EngineKind::Wasmi));
    process.top().pause(false);
    engine.settle_compile(&module);
    let on_cranelift = (
        call(&engine, &module, "spin", &[]),
        call(&engine, &module, "grow", &[]),
    );
    assert_eq!(engine.tier(&module), Some(EngineKind::Cranelift));
    assert_eq!(on_wasmi.0, Err(CallError::OutOfFuel(OUT_OF_FUEL.into())));
    assert_eq!(
        on_wasmi.1,
        Err(CallError::MemoryLimit(GROWTH_LIMITED.into()))
    );
    assert_eq!(on_cranelift, on_wasmi);
    // A trap drops only the instance it happened in: the next call runs.
    assert_eq!(
        call(&engine, &module, "add", &[Wasm::I32(1), Wasm::I32(1)]).unwrap(),
        Some(Wasm::I32(2))
    );
}

#[test]
fn a_module_compiled_once_starts_compiled_in_the_next_process() {
    let dir = tempfile::tempdir().unwrap();
    let first = Process::new(Some(dir.path()));
    let engine = first.engine();
    let module = engine.compile(&bytes()).unwrap();
    engine.settle_compile(&module);
    assert!(
        tiering::entry(dir.path(), &module).is_some(),
        "nothing kept on disk"
    );

    // The next process, its compiles held so that only the disk can make it compiled.
    let next = Process::new(Some(dir.path()));
    next.top().pause(true);
    let engine = next.engine();
    let started = engine.compile(&bytes()).unwrap();
    assert_eq!(started, module);
    assert_eq!(engine.tier(&module), Some(EngineKind::Cranelift));
    assert_eq!(
        call(&engine, &module, "add", &[Wasm::I32(20), Wasm::I32(22)]).unwrap(),
        Some(Wasm::I32(42))
    );
}

#[test]
fn a_damaged_cache_entry_is_not_loaded_and_the_module_is_compiled_again() {
    let dir = tempfile::tempdir().unwrap();
    let first = Process::new(Some(dir.path()));
    let engine = first.engine();
    let module = engine.compile(&bytes()).unwrap();
    engine.settle_compile(&module);
    let entry = tiering::entry(dir.path(), &module).unwrap();
    let kept = std::fs::read(&entry).unwrap();
    let mut damaged = kept.clone();
    *damaged.last_mut().unwrap() ^= 0x5a;
    std::fs::write(&entry, &damaged).unwrap();

    let next = Process::new(Some(dir.path()));
    next.top().pause(true);
    let engine = next.engine();
    engine.compile(&bytes()).unwrap();
    assert_eq!(engine.tier(&module), Some(EngineKind::Wasmi));
    assert_eq!(
        call(&engine, &module, "add", &[Wasm::I32(1), Wasm::I32(2)]).unwrap(),
        Some(Wasm::I32(3))
    );
    next.top().pause(false);
    engine.settle_compile(&module);
    call(&engine, &module, "add", &[Wasm::I32(1), Wasm::I32(2)]).unwrap();
    assert_eq!(engine.tier(&module), Some(EngineKind::Cranelift));
    // Compiled again and kept again, whole.
    assert_eq!(std::fs::read(&entry).unwrap().len(), kept.len());
    assert_ne!(std::fs::read(&entry).unwrap(), damaged);
}
