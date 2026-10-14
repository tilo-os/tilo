//! Keeps English prose out of what the agent hands its sheet: the words a
//! person reads come from the host's catalogue (see [`crate::StockNotice`]),
//! or verbatim from PAM and polkit.

#[test]
fn the_agent_hands_its_sheet_no_english_literals() {
    tilo_copy_lint::CopyLint::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))
        .skip_files(&["tests.rs", "copy_lint_tests.rs"])
        .min_files(2)
        .check();
}
