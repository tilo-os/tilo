//! Keeps English prose out of the desktop's copy builders; the scan itself
//! lives in `tilo-copy-lint`, shared with the shell.

/// Literals that look like prose but are not copy. Each entry is
/// `(path under src/, literal)`.
const ALLOWED: &[(&str, &str)] = &[];

#[test]
fn copy_builders_take_catalogue_strings_not_english_literals() {
    tilo_copy_lint::CopyLint::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))
        .allow(ALLOWED)
        // The activity pane's test fixture, compiled only under test.
        .skip_files(&["testing.rs"])
        .min_files(10)
        .check();
}
