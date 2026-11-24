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
    CallError, DEFAULT_LIMITS, Engine, EngineKind, EngineLimits, Host, HostFail, Shared, Tiers,
    Translation, Wasm,
};
use crate::ids::ModuleHash;
use tilo_wasm_host::{Failure, GROWTH_LIMITED, OUT_OF_FUEL};

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
        ..DEFAULT_LIMITS
    }
}

/// The tiered engine of a process of its own, reading the devices's compiled modules from `dir`
/// when one is given.
struct Process(&'static Shared);

impl Process {
    fn new(dir: Option<&Path>) -> Self {
        let tiers = Tiers::new(Translation::Lazy).unwrap();
        if let Some(dir) = dir {
            super::compiled_from(dir.to_path_buf());
        }
        Self(Box::leak(Box::new(Shared::Tiered(Box::new(tiers)))))
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

/// The same module, its bytes made distinct by an exported constant: each test that hands an
/// artifact to the process-wide registry uses its own module, so no test sees another's.
fn tagged(tag: i32) -> Vec<u8> {
    wat::parse_str(MODULE.replacen(
        "(module",
        &format!("(module (global (export \"tag\") i32 (i32.const {tag}))"),
        1,
    ))
    .unwrap()
}

/// The artifact the device daemon would make for `bytes`: compiled by the one compiler, for
/// this configuration and target.
fn artifact(bytes: &[u8]) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let cache = tilo_wasm_aot::Cache::at(dir.path()).unwrap();
    cache.ensure(bytes).unwrap()
}

tilo_test_env::scenario! {
    fn a_module_runs_the_same_on_both_tiers;

    phase a_module_without_an_artifact_runs_on_the_interpreter;
    {
        let process = Process::new(None);
        let engine = process.engine();
        let module = engine.compile(&tagged(1)).unwrap();
        assert_eq!(engine.tier(&module), Some(EngineKind::Wasmi));
        assert_eq!(
            call(&engine, &module, "add", &[Wasm::I32(2), Wasm::I32(40)]).unwrap(),
            Some(Wasm::I32(42))
        );
        let expected: i64 = (0..100_000_i64)
            .map(|i| i.wrapping_mul(2_654_435_761) ^ (i >> 3))
            .fold(0, i64::wrapping_add);
        assert_eq!(
            call(&engine, &module, "mix", &[Wasm::I64(100_000)]).unwrap(),
            Some(Wasm::I64(expected))
        );
        // Running out of fuel or memory fails the same way it always has.
        assert_eq!(
            call(&engine, &module, "spin", &[]),
            Err(CallError::from(Failure::OutOfFuel(OUT_OF_FUEL.to_owned())))
        );
        assert_eq!(
            call(&engine, &module, "grow", &[]),
            Err(CallError::from(Failure::MemoryLimit(
                GROWTH_LIMITED.to_owned()
            )))
        );
    }

    phase a_module_with_the_daemons_artifact_runs_compiled_and_answers_the_same;
    {
        let bytes = tagged(2);
        let entry = artifact(&bytes);
        assert_eq!(
            super::adopt_precompiled(&bytes, &entry).unwrap(),
            ModuleHash::of_bytes(&bytes)
        );
        let process = Process::new(None);
        let engine = process.engine();
        let module = engine.compile(&bytes).unwrap();
        assert_eq!(engine.tier(&module), Some(EngineKind::Cranelift));
        assert_eq!(engine.compile_failure(&module), None);
        assert_eq!(
            call(&engine, &module, "add", &[Wasm::I32(2), Wasm::I32(40)]).unwrap(),
            Some(Wasm::I32(42))
        );
        let expected: i64 = (0..100_000_i64)
            .map(|i| i.wrapping_mul(2_654_435_761) ^ (i >> 3))
            .fold(0, i64::wrapping_add);
        assert_eq!(
            call(&engine, &module, "mix", &[Wasm::I64(100_000)]).unwrap(),
            Some(Wasm::I64(expected))
        );
        assert_eq!(
            call(&engine, &module, "spin", &[]),
            Err(CallError::from(Failure::OutOfFuel(OUT_OF_FUEL.to_owned())))
        );
        assert_eq!(
            call(&engine, &module, "grow", &[]),
            Err(CallError::from(Failure::MemoryLimit(
                GROWTH_LIMITED.to_owned()
            )))
        );
    }

    phase an_artifact_of_another_module_is_refused_and_the_module_is_interpreted;
    {
        let bytes = tagged(3);
        let other = artifact(&tagged(4));
        assert!(super::adopt_precompiled(&bytes, &other).is_err());
        let process = Process::new(None);
        let engine = process.engine();
        let module = engine.compile(&bytes).unwrap();
        assert_eq!(engine.tier(&module), Some(EngineKind::Wasmi));
        assert_eq!(
            call(&engine, &module, "add", &[Wasm::I32(1), Wasm::I32(1)]).unwrap(),
            Some(Wasm::I32(2))
        );
    }
}

tilo_test_env::scenario! {
    fn compiled_modules_are_read_from_the_devices_cache;

    phase a_module_the_daemon_compiled_starts_compiled_and_a_damaged_entry_is_passed_over;
    {
        let dir = tempfile::tempdir().unwrap();
        let bytes = tagged(5);
        let cache = tilo_wasm_aot::Cache::at(dir.path()).unwrap();
        cache.ensure(&bytes).unwrap();

        let process = Process::new(Some(dir.path()));
        let engine = process.engine();
        let module = engine.compile(&bytes).unwrap();
        assert_eq!(engine.tier(&module), Some(EngineKind::Cranelift));
        assert_eq!(
            call(&engine, &module, "add", &[Wasm::I32(20), Wasm::I32(22)]).unwrap(),
            Some(Wasm::I32(42))
        );

        // An entry that is not whole is passed over: the module runs on the interpreter, and
        // nothing is compiled here to replace the entry.
        let path = dir.path().join(cache.name(&bytes));
        let kept = std::fs::read(&path).unwrap();
        let mut damaged = kept;
        *damaged.last_mut().unwrap() ^= 0x5a;
        std::fs::write(&path, &damaged).unwrap();
        let next = Process::new(Some(dir.path()));
        let engine = next.engine();
        let module = engine.compile(&bytes).unwrap();
        assert_eq!(engine.tier(&module), Some(EngineKind::Wasmi));
        assert_eq!(
            call(&engine, &module, "add", &[Wasm::I32(1), Wasm::I32(2)]).unwrap(),
            Some(Wasm::I32(3))
        );
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
        let runtime = wat::parse_str(RUNTIME).unwrap();
        let first = app(0, 100, 21, 0);
        let second = app(1, 104, 7, 1);

        // With no artifact, the set runs on the interpreter.
        let interpreted = Process::new(None).engine();
        let a = interpreted
            .compile_linked(&[&runtime, &first])
            .unwrap();
        let b = interpreted
            .compile_linked(&[&runtime, &second])
            .unwrap();
        assert_ne!(a, b);
        assert_eq!(interpreted.tier(&a), Some(EngineKind::Wasmi));
        assert_eq!(
            call(&interpreted, &a, "apply_a", &[]).unwrap(),
            Some(Wasm::I32(42))
        );
        assert_eq!(
            call(&interpreted, &b, "apply_b", &[]).unwrap(),
            Some(Wasm::I32(15))
        );
        // `b`'s module is not in `a`'s set: its table entry is empty there.
        assert!(call(&interpreted, &a, "apply_b", &[]).is_err());

        // With the daemon's artifact for each part, both sets start compiled.
        for module in [&runtime, &first, &second] {
            super::adopt_precompiled(module, &artifact(module)).unwrap();
        }
        let process = Process::new(None);
        let engine = process.engine();
        let a = engine.compile_linked(&[&runtime, &first]).unwrap();
        let b = engine.compile_linked(&[&runtime, &second]).unwrap();
        assert_eq!(engine.tier(&a), Some(EngineKind::Cranelift));
        assert_eq!(engine.tier(&b), Some(EngineKind::Cranelift));
        assert_eq!(engine.compile_failure(&a), None);
        assert_eq!(call(&engine, &a, "apply_a", &[]).unwrap(), Some(Wasm::I32(42)));
        assert_eq!(call(&engine, &b, "apply_b", &[]).unwrap(), Some(Wasm::I32(15)));
        assert!(call(&engine, &a, "apply_b", &[]).is_err());
    }

    phase an_import_no_module_before_it_exports_does_not_link;
    {
        let process = Process::new(None);
        let engine = process.engine();
        let runtime = wat::parse_str(RUNTIME.replace("\"double\"", "\"twice\"")).unwrap();
        let refused = engine.compile_linked(&[&runtime, &app(0, 100, 1, 0)]);
        assert!(
            matches!(&refused, Err(CallError::Wasm(Failure::Link(_)))),
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
        let engine = process.engine();
        engine.link(PEEK_ABI).unwrap();
        let set = engine.compile_linked(&[&runtime, &app]).unwrap();
        let peek = || engine.call(&set, "apply", &[], Rc::new(Peek));
        assert_eq!(engine.tier(&set), Some(EngineKind::Wasmi));
        assert_eq!(peek().unwrap(), Some(Wasm::I32(0x4d)));

        // The daemon's artifacts make the same set run compiled.
        for part in [&runtime, &app] {
            super::adopt_precompiled(part, &artifact(part)).unwrap();
        }
        let process = Process::new(None);
        let engine = process.engine();
        engine.link(PEEK_ABI).unwrap();
        let set = engine.compile_linked(&[&runtime, &app]).unwrap();
        let peek = || engine.call(&set, "apply", &[], Rc::new(Peek));
        assert_eq!(engine.tier(&set), Some(EngineKind::Cranelift));
        assert_eq!(peek().unwrap(), Some(Wasm::I32(0x4d)));
    }
}
