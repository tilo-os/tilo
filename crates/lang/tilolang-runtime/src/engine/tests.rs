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

tilo_test_env::scenario! {
    fn a_module_runs_the_same_on_both_tiers;

    phase a_module_runs_before_its_compile_finishes_and_moves_to_compiled_code_between_calls;
    {
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

    phase the_results_are_the_same_on_both_tiers;
    {
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

    phase running_out_of_fuel_or_memory_fails_the_same_on_both_tiers;
    {
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
}

tilo_test_env::scenario! {
    fn compiled_modules_are_kept_on_disk_for_the_next_process;

    phase a_module_compiled_once_starts_compiled_in_the_next_process;
    {
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

    phase a_damaged_cache_entry_is_not_loaded_and_the_module_is_compiled_again;
    {
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
}

/// A start module as a split bundle's is: memory, table and a function every app calls, and
/// an entry per app that calls through the table entry its app's module fills.
const RUNTIME: &str = r#"
(module
  (type $entry (func (result i32)))
  (memory (export "memory") 1)
  (table (export "__indirect_function_table") 2 2 funcref)
  (func (export "double") (param i32) (result i32)
    (i32.mul (local.get 0) (i32.const 2)))
  (func (export "apply_a") (result i32)
    (call_indirect (type $entry) (i32.const 0)))
  (func (export "apply_b") (result i32)
    (call_indirect (type $entry) (i32.const 1)))
)
"#;

/// An app's module: imports what it uses of the start module by name, writes its own data and
/// fills its table entry.
fn app(entry: u32, at: u32, value: u32, add: u32) -> Vec<u8> {
    wat::parse_str(format!(
        r#"
(module
  (import "__wasm_split" "memory" (memory 1))
  (import "__wasm_split" "__indirect_function_table" (table 2 2 funcref))
  (import "__wasm_split" "double" (func $double (param i32) (result i32)))
  (data (i32.const {at}) "{bytes}")
  (func $entry (result i32)
    (i32.add (call $double (i32.load (i32.const {at}))) (i32.const {add})))
  (elem (i32.const {entry}) $entry)
)
"#,
        bytes = value
            .to_le_bytes()
            .iter()
            .fold(String::new(), |mut out, byte| {
                use std::fmt::Write as _;
                write!(out, "\\{byte:02x}").unwrap();
                out
            }),
    ))
    .unwrap()
}

tilo_test_env::scenario! {
    fn a_linked_set_answers_as_one_module;

    phase a_set_links_each_module_to_the_ones_before_it_on_both_tiers;
    {
        let process = Process::new(None);
        process.top().pause(true);
        let engine = process.engine();
        let runtime = wat::parse_str(RUNTIME).unwrap();
        let a = engine.compile_linked(&[&runtime, &app(0, 100, 21, 0)]).unwrap();
        let b = engine.compile_linked(&[&runtime, &app(1, 104, 7, 1)]).unwrap();
        assert_ne!(a, b);
        assert_eq!(engine.tier(&a), Some(EngineKind::Wasmi));
        assert_eq!(call(&engine, &a, "apply_a", &[]).unwrap(), Some(Wasm::I32(42)));
        assert_eq!(call(&engine, &b, "apply_b", &[]).unwrap(), Some(Wasm::I32(15)));
        // `b`'s module is not in `a`'s set: its table entry is empty there.
        assert!(call(&engine, &a, "apply_b", &[]).is_err());

        process.top().pause(false);
        engine.settle_compile(&a);
        engine.settle_compile(&b);
        assert_eq!(engine.compile_failure(&a), None);
        assert_eq!(call(&engine, &a, "apply_a", &[]).unwrap(), Some(Wasm::I32(42)));
        assert_eq!(engine.tier(&a), Some(EngineKind::Cranelift));
        assert_eq!(call(&engine, &b, "apply_b", &[]).unwrap(), Some(Wasm::I32(15)));
        assert_eq!(engine.tier(&b), Some(EngineKind::Cranelift));
        assert!(call(&engine, &a, "apply_b", &[]).is_err());
        // The runtime both sets share was compiled once, as a module of its own.
        assert!(process.top().compiled(&ModuleHash::of_bytes(&runtime)).is_some());
    }

    phase an_import_no_module_before_it_exports_does_not_link;
    {
        let process = Process::new(None);
        let engine = process.engine();
        let runtime = wat::parse_str(RUNTIME.replace("\"double\"", "\"twice\"")).unwrap();
        let refused = engine.compile_linked(&[&runtime, &app(0, 100, 1, 0)]);
        assert!(
            matches!(&refused, Err(CallError::Link(_))),
            "{refused:?}"
        );
    }
}

/// A device whose one import of its own ABI answers the byte at an offset of the caller's
/// memory, as the model ABI's imports read and write it.
struct Peek;

const PEEK_ABI: &[super::Imports] = &[("probe", "peek", "i", "i")];

impl Host for Peek {
    fn import(&self, _: usize, _: &[u8], _: &[Wasm]) -> Result<Option<Wasm>, HostFail> {
        Err(HostFail::Trap("only probe::peek is answered".into()))
    }

    fn guest_abi(&self) -> &'static [super::Imports] {
        PEEK_ABI
    }

    fn guest_import(
        &self,
        _: &str,
        _: &str,
        memory: &[u8],
        args: &[Wasm],
    ) -> Result<super::GuestAnswer, HostFail> {
        let [Wasm::I32(at)] = args else {
            return Err(HostFail::Trap("peek takes an offset".into()));
        };
        let byte = memory
            .get(usize::try_from(*at).unwrap())
            .ok_or_else(|| HostFail::Trap("no memory to peek at".into()))?;
        Ok(super::GuestAnswer::Value(Wasm::I32(i32::from(*byte))))
    }
}

tilo_test_env::scenario! {
    fn an_import_a_later_module_calls_answers_in_the_first_ones_memory;

    phase on_both_tiers;
    {
        let runtime = wat::parse_str(
            r#"
(module
  (type $entry (func (result i32)))
  (import "probe" "peek" (func $peek (param i32) (result i32)))
  (memory (export "memory") 1)
  (table (export "__indirect_function_table") 1 1 funcref)
  (export "peek" (func $peek))
  (func (export "apply") (result i32)
    (call_indirect (type $entry) (i32.const 0)))
)
"#,
        )
        .unwrap();
        let app = wat::parse_str(
            r#"
(module
  (import "__wasm_split" "memory" (memory 1))
  (import "__wasm_split" "__indirect_function_table" (table 1 1 funcref))
  (import "__wasm_split" "peek" (func $peek (param i32) (result i32)))
  (data (i32.const 200) "\4d")
  (func $entry (result i32) (call $peek (i32.const 200)))
  (elem (i32.const 0) $entry)
)
"#,
        )
        .unwrap();
        let process = Process::new(None);
        process.top().pause(true);
        let engine = process.engine();
        engine.link(PEEK_ABI).unwrap();
        let set = engine.compile_linked(&[&runtime, &app]).unwrap();
        let peek = || engine.call(&set, "apply", &[], Rc::new(Peek));
        assert_eq!(engine.tier(&set), Some(EngineKind::Wasmi));
        assert_eq!(peek().unwrap(), Some(Wasm::I32(0x4d)));
        process.top().pause(false);
        engine.settle_compile(&set);
        assert_eq!(peek().unwrap(), Some(Wasm::I32(0x4d)));
        assert_eq!(engine.tier(&set), Some(EngineKind::Cranelift));
    }
}
