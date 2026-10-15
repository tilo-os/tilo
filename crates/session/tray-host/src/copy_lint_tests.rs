//! Keeps English prose out of what the tray hands its host: item titles and
//! menu labels come verbatim from the applications, and the host words
//! everything else from its own catalogue.

#[test]
fn the_tray_hands_its_host_no_english_literals() {
    tilo_copy_lint::CopyLint::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))
        .skip_files(&["copy_lint_tests.rs"])
        .min_files(3)
        .check();
}
