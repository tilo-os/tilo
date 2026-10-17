//! Running one test alone in its process.
//!
//! Some state is process-wide by design: an app's settings and
//! live-activity channels (one app, one collaboration manager per
//! process), and the authenticated host whose personal vocabulary every
//! text field reads. A test that installs it would otherwise reach other
//! tests running beside it on other threads under `cargo test`, as nextest's
//! process per test never would.
#![cfg(test)]

/// Names the one test a re-executed test binary runs in its own process.
const OWN_PROCESS_TEST: &str = "TILO_UI_OWN_PROCESS_TEST";

/// Whether the calling test runs its body here: only in a process that runs
/// nothing else. Otherwise it re-executes this binary for `test` (its path
/// without the crate name, such as `widget::text_input::tests::name`) alone,
/// asserts the child ran it and passed, and returns `false`.
pub(crate) fn in_own_process(test: &str) -> bool {
    if std::env::var(OWN_PROCESS_TEST).is_ok_and(|own| own == test) {
        return true;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(OWN_PROCESS_TEST, test)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("test result: ok. 1 passed;"),
        "{test} in its own process: {}\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

/// The path [`in_own_process`] takes for test `name` in `module`
/// (`module_path!()` at the test).
pub(crate) fn test_path(module: &str, name: &str) -> String {
    let module = module.split_once("::").map_or(module, |(_, path)| path);
    format!("{module}::{name}")
}
