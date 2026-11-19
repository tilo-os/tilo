//! What the shared layer bounds and refuses: a spent budget, a growth past the limit, an import a
//! guest with nothing to reach may not have, and an instance whose call failed never being asked
//! again.

use wasmi::{Instance, Linker, Store, StoreLimits, TypedFunc};

use crate::wasmi as host;
use crate::{Failure, GROWTH_LIMITED, ImportPolicy, Limits, OUT_OF_FUEL, reusable};

fn limits() -> Limits {
    Limits {
        fuel_per_call: 10_000,
        deadline: None,
        max_memory_bytes: 1 << 16,
        max_memories: 1,
        max_tables: 1,
        max_instances: 1,
    }
}

/// A store of the module `bytes` under `limits`, with nothing linked.
fn start(bytes: &[u8], limits: &Limits) -> (Store<StoreLimits>, Instance) {
    let engine = host::engine(host::Translation::Lazy);
    let module = host::compile(&engine, bytes, ImportPolicy::None).unwrap();
    let mut store = host::store(&engine, host::store_limits(limits), limits).unwrap();
    let instance = Linker::<StoreLimits>::new(&engine)
        .instantiate_and_start(&mut store, &module)
        .unwrap();
    (store, instance)
}

/// A host with one instance, following the rule: once a call fails, the instance is never asked
/// again.
struct One {
    store: Store<StoreLimits>,
    spin: TypedFunc<(), ()>,
    limits: Limits,
    alive: bool,
    calls: u32,
}

impl One {
    fn call(&mut self) -> Result<(), Failure> {
        if !self.alive {
            return Err(Failure::Trap("the instance failed earlier".to_owned()));
        }
        self.calls = self.calls.saturating_add(1);
        host::begin(&mut self.store, &self.limits)?;
        let outcome = self
            .spin
            .call(&mut self.store, ())
            .map_err(|error| host::classify(&error));
        self.alive = reusable(&outcome);
        outcome
    }
}

const SPIN: &str = r#"(module (func (export "spin") (loop $forever (br $forever))))"#;

#[test]
fn a_spent_budget_is_an_out_of_fuel_failure_and_its_instance_is_not_asked_again() {
    let limits = limits();
    let (store, instance) = start(&wat::parse_str(SPIN).unwrap(), &limits);
    let spin = instance.get_typed_func::<(), ()>(&store, "spin").unwrap();
    let mut one = One {
        store,
        spin,
        limits,
        alive: true,
        calls: 0,
    };
    assert_eq!(one.call(), Err(Failure::OutOfFuel(OUT_OF_FUEL.to_owned())));
    assert!(!one.alive, "a failed instance is retired");
    assert!(one.call().is_err());
    assert_eq!(one.calls, 1, "the failed instance is never called again");
}

#[test]
fn a_growth_past_the_limit_is_a_memory_limit_failure() {
    let limits = limits();
    let grow = wat::parse_str(
        r#"(module
            (memory (export "memory") 1)
            (func (export "grow") (result i32) (memory.grow (i32.const 4))))"#,
    )
    .unwrap();
    let (mut store, instance) = start(&grow, &limits);
    let grow = instance.get_typed_func::<(), i32>(&store, "grow").unwrap();
    host::begin(&mut store, &limits).unwrap();
    let outcome = grow
        .call(&mut store, ())
        .map_err(|error| host::classify(&error));
    assert_eq!(
        outcome,
        Err(Failure::MemoryLimit(GROWTH_LIMITED.to_owned()))
    );
    assert!(!reusable(&outcome));
}

#[test]
fn a_guest_that_may_import_nothing_is_refused_its_first_import() {
    let engine = host::engine(host::Translation::Lazy);
    let clock = wat::parse_str(r#"(module (import "env" "clock" (func (result i64))))"#).unwrap();
    assert_eq!(
        host::compile(&engine, &clock, ImportPolicy::None).err(),
        Some(Failure::Imports("env::clock".to_owned()))
    );
    assert!(
        host::compile(&engine, &clock, ImportPolicy::Any).is_ok(),
        "a host that links its imports loads the same module"
    );
    assert!(matches!(
        host::compile(&engine, b"not wasm", ImportPolicy::None),
        Err(Failure::Invalid(_))
    ));
}

#[test]
fn only_a_call_that_succeeded_leaves_its_instance_usable() {
    assert!(reusable(&Ok::<(), Failure>(())));
    assert!(!reusable(&Err::<(), Failure>(Failure::Trap(
        "stopped".to_owned()
    ))));
}
