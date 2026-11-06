//! The session's log keeps a panic: a record of why, its backtrace included.
//!
//! The backtrace names the test's own function, which the symbol table this
//! package's tests keep gives it (`[profile.test.package.tilo-session-logging]`
//! in the workspace's Cargo.toml; CI strips every other test executable's).

#![cfg(target_os = "linux")]
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion, and the panic is the thing recorded"
)]

/// A session that panics leaves a record of why, its backtrace included:
/// without one, "the session just disappeared" is all the person has.
#[test]
fn a_panic_leaves_a_record_with_its_backtrace() {
    tilo_test_env::in_own_process(
        module_path!(),
        "a_panic_leaves_a_record_with_its_backtrace",
        || {
            let home = tilo_test_env::TestHome::new().expect("a home of its own");
            tilo_session_logging::install();
            let ended = std::thread::Builder::new()
                .name("tile-laying".into())
                .spawn(|| panic!("a stepped-on tile"))
                .expect("the thread starts");
            assert!(ended.join().is_err(), "the thread panicked");
            let crashes = home.cache().join("tilo/crashes");
            let mut records = std::fs::read_dir(&crashes)
                .expect("the crash records")
                .map(|record| record.expect("a crash record").path())
                .collect::<Vec<_>>();
            assert_eq!(records.len(), 1, "{records:#?}");
            let record = std::fs::read_to_string(records.remove(0)).expect("the record");
            assert!(record.contains("message:   a stepped-on tile"), "{record}");
            assert!(record.contains("thread:    tile-laying"), "{record}");
            assert!(
                record.contains("backtrace:")
                    && record.contains("a_panic_leaves_a_record_with_its_backtrace"),
                "the record keeps the backtrace of a panic nobody saw: {record}"
            );
        },
    );
}
