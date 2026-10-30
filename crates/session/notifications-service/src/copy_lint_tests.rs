//! Keeps English prose out of what the daemon publishes: a notification's
//! words are the sender's own.

#[test]
fn the_daemon_publishes_no_english_literals() {
    tilo_copy_lint::CopyLint::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))
        .skip_files(&["tests.rs", "copy_lint_tests.rs"])
        .min_files(4)
        .check();
}
