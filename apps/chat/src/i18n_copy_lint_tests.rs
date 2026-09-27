//! Keeps English prose out of Chat's copy builders; the scan itself lives in
//! `tilo-copy-lint`, shared with the shell and the desktop.

/// Literals that look like prose but are not copy. Each entry is
/// `(path under src/, literal)`.
const ALLOWED: &[(&str, &str)] = &[
    // The app's name, a brand that is not translated.
    ("app.rs", "Chat"),
    ("component.rs", "Chat"),
];

#[test]
fn copy_builders_take_catalogue_strings_not_english_literals() {
    tilo_copy_lint::CopyLint::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))
        .allow(ALLOWED)
        .min_files(20)
        .check();
}
