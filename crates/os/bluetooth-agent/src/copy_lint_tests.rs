//! Keeps English prose out of what the agent asks: the words a person reads
//! come from the sheet's catalogue, keyed by the question's details, or
//! verbatim from the device.

#[test]
fn the_agent_asks_no_english_literals() {
    tilo_copy_lint::CopyLint::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))
        .skip_files(&["tests.rs", "copy_lint_tests.rs"])
        .min_files(0)
        .check();
}
