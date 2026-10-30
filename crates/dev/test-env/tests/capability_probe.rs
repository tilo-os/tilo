//! What this machine offers of the capabilities some tests need, said where
//! the tests run. `tools/ci/capability_filter.py` runs this first and leaves
//! out the tests whose probe fails (`.config/nextest-capabilities.toml`), so
//! a run reports them as not run instead of letting them skip and pass.
//!
//! It never fails over a missing capability: reporting one is its job.

#[cfg(target_os = "linux")]
#[test]
fn capability_probe() {
    for capability in tilo_test_env::Capability::ALL {
        println!(
            "tilo-capability-variable {capability:?} {}",
            capability.variable()
        );
    }
    for (name, probe) in tilo_test_env::PROBES {
        match probe() {
            Ok(()) => println!("tilo-capability-probe {name} available"),
            Err(error) => println!("tilo-capability-probe {name} unavailable: {error}"),
        }
    }
}
