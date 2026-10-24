//! Tests that change what the whole process sees, and so must have it to
//! themselves.
//!
//! Some state has no narrower scope than the process: the wall clock a test
//! holds with `tilo_clock::freeze_wall` is read by every thread, the broker
//! threads of other tests included. Nextest already runs every test in a
//! process of its own; `cargo test` runs them as threads of one. A test that
//! holds such state runs its body through [`in_own_process`], which under
//! `cargo test` starts the test binary again for that one test, so no other
//! test ever shares the process with it.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;
use std::{env, fs};

/// Names the test a child process was started to run.
const CHILD_TEST: &str = "TILO_TEST_OWN_PROCESS";
/// Where the child records that the body ran to its end.
const CHILD_PROOF: &str = "TILO_TEST_OWN_PROCESS_PROOF";

/// Run `body`, the body of the test named `test` in module `module`
/// (`module_path!()` and the test function's name), in a process no other
/// test shares.
///
/// Under nextest, which runs each test in a process of its own, and in the
/// child started for it, `body` runs here. Otherwise the test binary is run
/// again for exactly this test, and the test passes when that run ran
/// `body` to its end.
///
/// # Panics
///
/// When the child cannot be started, fails, or exits without running `body`.
#[track_caller]
pub fn in_own_process(module: &str, test: &str, body: impl FnOnce()) {
    // libtest names a test by its path within the crate.
    let name = match module.split_once("::") {
        Some((_, within)) => format!("{within}::{test}"),
        None => test.to_owned(),
    };
    let per_test = env::var_os("NEXTEST_EXECUTION_MODE")
        .is_some_and(|mode| mode == OsStr::new("process-per-test"));
    if per_test {
        body();
        return;
    }
    if env::var_os(CHILD_TEST).is_some_and(|child| child == OsStr::new(&name)) {
        body();
        let proof = env::var_os(CHILD_PROOF).expect("the child knows where to record its end");
        fs::write(proof, &name).expect("the child records its end");
        return;
    }
    let scratch = tempfile::tempdir().expect("a directory for the child's proof");
    let proof = scratch.path().join("ran");
    let binary = env::current_exe().expect("the test binary");
    let status = run_child(&binary, &name, &proof);
    assert!(
        status.success(),
        "{name}, in a process of its own, failed: {status}"
    );
    assert_eq!(
        fs::read_to_string(&proof).ok().as_deref(),
        Some(name.as_str()),
        "the process started for {name} did not run it"
    );
}

fn run_child(binary: &Path, name: &str, proof: &Path) -> std::process::ExitStatus {
    let mut command = Command::new(binary);
    command
        .args(["--exact", name, "--nocapture", "--test-threads", "1"])
        .env(CHILD_TEST, name)
        .env(CHILD_PROOF, proof);
    // The child copies the environment as it starts: start it while no
    // other test's scope has the environment changed.
    let started = {
        let _environment = crate::lock();
        command.spawn()
    };
    started
        .and_then(|mut child| child.wait())
        .unwrap_or_else(|error| panic!("run {name} in a process of its own: {error}"))
}
