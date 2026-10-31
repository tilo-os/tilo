//! The engine the suite runs on, and the bench's compute workload computing what the same
//! arithmetic computes natively, on whichever engine that is.

use tilolang_runtime::{AmStore, InvokeOutcome, Principal, Runtime, SimpleAuthority, Value};

/// The suites run on each engine by naming it in `TILOLANG_ENGINE`; a run that names an engine
/// this build lacks fails here instead of passing on the default one.
#[test]
fn the_suite_runs_on_the_engine_it_names() {
    let named = std::env::var(tilolang_runtime::engine::ENGINE_VARIABLE).ok();
    assert_eq!(
        tilolang_runtime::configured().kind.name(),
        named
            .as_deref()
            .unwrap_or_else(|| tilolang_runtime::EngineKind::default().name())
    );
}

/// `mix` of `bench/compute/app.tl`, in Rust: wasm's i64 arithmetic wraps and divides towards
/// zero, as these do.
fn mix(x: i64) -> i64 {
    const M: i64 = 6_364_136_223_846_793_005;
    const C: i64 = 1_442_695_040_888_963_407;
    let a = x.wrapping_mul(M).wrapping_add(C);
    let b = a.wrapping_mul(M).wrapping_add(C);
    let c = b.wrapping_mul(M).wrapping_add(C);
    (c / 65536) % 1_000_003
}

#[test]
fn the_compute_workload_computes_what_rust_does() {
    let package = tilolang_tilo::bench::compute_package().unwrap();
    let action = |name: &str| {
        let found = package.meta.actions.iter().find(|a| a.name == name);
        found.unwrap().id.clone()
    };
    let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
    rt.load_wasm(&package.wasm).unwrap();
    let lab = rt.create("Lab").unwrap();
    let who = Principal::new("test");

    let n = 4096;
    let out = rt
        .invoke_action(
            &who,
            (
                action("sweep_range").as_str(),
                vec![Value::resource(&lab), Value::Int(n)],
            ),
        )
        .unwrap();
    assert_eq!(out, InvokeOutcome::Done(Value::Unit));
    let swept: i64 = (0..n).map(mix).sum();
    assert_eq!(rt.read(&lab, "result").unwrap(), Value::Int(swept));

    let out = rt
        .invoke_action(
            &who,
            (action("nested_loops").as_str(), vec![Value::resource(&lab)]),
        )
        .unwrap();
    assert_eq!(out, InvokeOutcome::Done(Value::Unit));
    let looped: i64 = (0..1 << 20).map(mix).sum();
    assert_eq!(rt.read(&lab, "result").unwrap(), Value::Int(looped));
}
