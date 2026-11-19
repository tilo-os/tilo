//! What the compiled engine adds to the shared layer: a call that outlives its deadline is
//! stopped while it executes, wherever in the guest it is, and fails as a spent budget, and an
//! idle engine ticks nothing.

use std::thread;
use std::time::{Duration, Instant};

use wasmtime::{Instance, Linker, TypedFunc};

use crate::wasmtime as host;
use crate::{DEADLINE_EXCEEDED, Failure, ImportPolicy, Limits, reusable};

fn limits() -> Limits {
    Limits {
        // Enough fuel to loop well past the deadline, so the clock is what stops the call, and
        // little enough that an engine which never ticked fails this test instead of hanging it.
        fuel_per_call: 20_000_000_000,
        deadline: Some(Duration::from_millis(100)),
        max_memory_bytes: 1 << 16,
        max_memories: 1,
        max_tables: 1,
        max_instances: 1,
    }
}

const SPIN: &str = r#"(module (func (export "spin") (loop $forever (br $forever))))"#;

const ANSWER: &str = r#"(module (func (export "answer") (result i32) (i32.const 42)))"#;

/// An instance of `wat` on `engine`, under `limits`, and its store.
fn guest(
    engine: &host::Engine,
    wat: &str,
    limits: &Limits,
) -> Result<(Instance, wasmtime::Store<host::GrowthLimits>), Failure> {
    let bytes = wat::parse_str(wat).map_err(|error| Failure::Invalid(error.to_string()))?;
    let module = host::compile(engine, &bytes, ImportPolicy::None)?;
    let mut store = host::store(engine, host::GrowthLimits::new(*limits), limits)?;
    let instance = {
        // The start functions execute inside the guest, under the same clock as a call.
        let _starting = host::in_call(engine);
        Linker::<host::GrowthLimits>::new(engine)
            .instantiate(&mut store, &module)
            .map_err(|error| host::classify(&error))?
    };
    Ok((instance, store))
}

#[test]
fn a_looping_guest_is_stopped_by_its_deadline_in_bounded_time() {
    let limits = limits();
    let engine = host::engine(host::Strategy::Cranelift).unwrap();
    for round in 1..=2 {
        let (instance, mut store) = guest(&engine, SPIN, &limits).unwrap();
        let spin = instance
            .get_typed_func::<(), ()>(&mut store, "spin")
            .unwrap();
        let call = host::begin(&engine, &mut store, &limits).unwrap();
        let started = Instant::now();
        let outcome = spin
            .call(&mut store, ())
            .map_err(|error| host::classify(&error));
        let ran = started.elapsed();
        drop(call);
        assert_eq!(
            outcome,
            Err(Failure::Deadline(DEADLINE_EXCEEDED.to_owned())),
            "round {round}"
        );
        assert!(
            !reusable(&outcome),
            "an instance stopped by its deadline is retired as a spent budget's is"
        );
        assert!(
            ran < Duration::from_secs(1),
            "round {round}: a call past its 100 ms deadline ran {ran:?}"
        );
    }
}

#[test]
fn an_idle_engine_ticks_nothing() {
    let engine = host::engine(host::Strategy::Cranelift).unwrap();
    let idle = engine.ticks();
    thread::sleep(Duration::from_millis(200));
    assert_eq!(
        engine.ticks(),
        idle,
        "an engine with no call in it woke its clock"
    );

    let limits = limits();
    let (instance, mut store) = guest(&engine, ANSWER, &limits).unwrap();
    let answer = instance
        .get_typed_func::<(), i32>(&mut store, "answer")
        .unwrap();
    {
        let _call = host::begin(&engine, &mut store, &limits).unwrap();
        assert_eq!(
            answer
                .call(&mut store, ())
                .map_err(|error| host::classify(&error)),
            Ok(42)
        );
    }
    let idle = engine.ticks();
    thread::sleep(Duration::from_millis(200));
    assert_eq!(
        engine.ticks(),
        idle,
        "the clock kept ticking after its call returned"
    );
}

#[test]
fn a_call_that_finishes_inside_its_deadline_keeps_working() {
    let limits = limits();
    let engine = host::engine(host::Strategy::Cranelift).unwrap();
    let (instance, mut store) = guest(&engine, ANSWER, &limits).unwrap();
    let answer = instance
        .get_typed_func::<(), i32>(&mut store, "answer")
        .unwrap();
    for _ in 0..2 {
        let _call = host::begin(&engine, &mut store, &limits).unwrap();
        assert_eq!(
            answer
                .call(&mut store, ())
                .map_err(|error| host::classify(&error)),
            Ok(42)
        );
    }
}

#[test]
fn a_call_with_no_deadline_is_not_stopped_by_the_clock() {
    let limits = Limits {
        deadline: None,
        fuel_per_call: 100_000,
        ..limits()
    };
    let engine = host::engine(host::Strategy::Cranelift).unwrap();
    let (instance, mut store) = guest(&engine, SPIN, &limits).unwrap();
    let spin: TypedFunc<(), ()> = instance
        .get_typed_func::<(), ()>(&mut store, "spin")
        .unwrap();
    // Fuel is the only bound here, so this call spends the store's budget and stops on it.
    let _call = host::begin(&engine, &mut store, &limits).unwrap();
    let outcome = spin
        .call(&mut store, ())
        .map_err(|error| host::classify(&error));
    assert!(matches!(outcome, Err(Failure::OutOfFuel(_))));
}
