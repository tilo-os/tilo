//! What the compiled engine adds to the shared layer: a call that outlives its deadline is
//! stopped while it executes, wherever in the guest it is, and fails as a spent budget.

use std::time::{Duration, Instant};

use wasmtime::{Instance, Linker, TypedFunc};

use crate::wasmtime as host;
use crate::{DEADLINE_EXCEEDED, Failure, ImportPolicy, Limits, reusable};

fn limits() -> Limits {
    Limits {
        // Enough fuel to loop for far longer than the deadline: the clock is what stops it.
        fuel_per_call: u64::MAX,
        deadline: Some(Duration::from_millis(100)),
        max_memory_bytes: 1 << 16,
        max_memories: 1,
        max_tables: 1,
        max_instances: 1,
    }
}

const SPIN: &str = r#"(module (func (export "spin") (loop $forever (br $forever))))"#;

#[test]
fn a_looping_guest_is_stopped_by_its_deadline_in_bounded_time() {
    let limits = limits();
    let engine = host::engine(host::Strategy::Cranelift).unwrap();
    let module =
        host::compile(&engine, &wat::parse_str(SPIN).unwrap(), ImportPolicy::None).unwrap();
    let mut store = host::store(&engine, host::GrowthLimits::new(limits), &limits).unwrap();
    let instance: Instance = Linker::<host::GrowthLimits>::new(&engine)
        .instantiate(&mut store, &module)
        .unwrap();
    let spin = instance
        .get_typed_func::<(), ()>(&mut store, "spin")
        .unwrap();
    host::begin(&mut store, &limits).unwrap();
    let started = Instant::now();
    let outcome = spin
        .call(&mut store, ())
        .map_err(|error| host::classify(&error));
    let ran = started.elapsed();
    assert_eq!(
        outcome,
        Err(Failure::Deadline(DEADLINE_EXCEEDED.to_owned()))
    );
    assert!(
        !reusable(&outcome),
        "an instance stopped by its deadline is retired as a spent budget's is"
    );
    assert!(
        ran < Duration::from_secs(5),
        "a call past its 100 ms deadline ran {ran:?}"
    );
}

#[test]
fn a_call_that_finishes_inside_its_deadline_keeps_working() {
    let limits = Limits {
        deadline: Some(Duration::from_secs(5)),
        ..limits()
    };
    let engine = host::engine(host::Strategy::Cranelift).unwrap();
    let module = host::compile(
        &engine,
        &wat::parse_str(r#"(module (func (export "answer") (result i32) (i32.const 42)))"#)
            .unwrap(),
        ImportPolicy::None,
    )
    .unwrap();
    let mut store = host::store(&engine, host::GrowthLimits::new(limits), &limits).unwrap();
    let instance: Instance = Linker::<host::GrowthLimits>::new(&engine)
        .instantiate(&mut store, &module)
        .unwrap();
    let answer = instance
        .get_typed_func::<(), i32>(&mut store, "answer")
        .unwrap();
    host::begin(&mut store, &limits).unwrap();
    assert_eq!(
        answer
            .call(&mut store, ())
            .map_err(|error| host::classify(&error)),
        Ok(42)
    );
    // The deadline is per call: the next call gets its own.
    host::begin(&mut store, &limits).unwrap();
    assert_eq!(
        answer
            .call(&mut store, ())
            .map_err(|error| host::classify(&error)),
        Ok(42)
    );
}

#[test]
fn a_call_with_no_deadline_is_not_stopped_by_the_ticker() {
    let limits = Limits {
        deadline: None,
        fuel_per_call: 100_000,
        ..limits()
    };
    let engine = host::engine(host::Strategy::Cranelift).unwrap();
    let module =
        host::compile(&engine, &wat::parse_str(SPIN).unwrap(), ImportPolicy::None).unwrap();
    let mut store = host::store(&engine, host::GrowthLimits::new(limits), &limits).unwrap();
    let instance: Instance = Linker::<host::GrowthLimits>::new(&engine)
        .instantiate(&mut store, &module)
        .unwrap();
    let spin: TypedFunc<(), ()> = instance
        .get_typed_func::<(), ()>(&mut store, "spin")
        .unwrap();
    // Fuel is the only bound here, so this call spends the store's budget and stops on it.
    host::begin(&mut store, &limits).unwrap();
    let outcome = spin
        .call(&mut store, ())
        .map_err(|error| host::classify(&error));
    assert!(matches!(outcome, Err(Failure::OutOfFuel(_))));
}
