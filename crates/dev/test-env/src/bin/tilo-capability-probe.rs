//! What this machine offers of the capabilities some tests need, said where
//! the tests run. `tools/ci/capability_filter.py` runs this first, as the
//! tests' runner starts a test binary, and leaves out the tests whose probe
//! fails (`.config/nextest-capabilities.toml`), so a run reports them as not
//! run instead of letting them skip and pass. A program rather than a test: a
//! family's build builds it once and t hands it to every shard (t.toml's
//! `starts`), so no shard compiles anything to probe.
//!
//! It never fails over a missing capability: reporting one is its job.

#[cfg(target_os = "linux")]
fn main() {
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

/// The probes are Linux's: elsewhere it reports nothing, which the filter
/// refuses as a probe that did not run.
#[cfg(not(target_os = "linux"))]
fn main() {}
