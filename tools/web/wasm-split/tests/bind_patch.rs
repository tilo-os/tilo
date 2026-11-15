//! A body-only change patches wasm-bindgen's earlier output to exactly what a
//! full bind writes, and anything else asks for the full bind
//! (src/bin/bind_patch.rs).
//!
//! The fixtures are small modules bound by the pinned wasm-bindgen: `bound0`
//! is its real output for `linked0`, a module with locals of two types, calls,
//! an indirect call and a table, and `bound1` the real output for `linked1`,
//! whose only change is one `i32.const` value in `$e`. `linked-import` names
//! its imports `unused`, `kept` and `later`; the bind drops the two nothing
//! calls, so `kept` sits at a different index in its output than in the module
//! it read, as a real bind's dropped imports move every import after them.
//! `linked-import-edit` grows the body that calls `kept`, and
//! `linked-import-dropped` calls `later`, which the bind dropped. They are
//! kept as hex (the repository takes no new binary blobs) and written out
//! here.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wasm-bind-patch-test-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a test directory");
    dir
}

/// Writes a fixture module into `dir`, reading its hex from the fixtures.
fn module(dir: &Path, name: &str) -> PathBuf {
    let hex = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/bind-patch")
            .join(format!("{name}.hex")),
    )
    .unwrap_or_else(|error| panic!("reading {name}.hex: {error}"));
    let bytes: Vec<u8> = (0..hex.trim().len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex.trim()[at..at + 2], 16).expect("hex"))
        .collect();
    let modules = dir.join("modules");
    std::fs::create_dir_all(&modules).expect("a fixture directory");
    let path = modules.join(format!("{name}.wasm"));
    std::fs::write(&path, bytes).expect("writing a fixture module");
    path
}

fn run(args: &[&OsStr]) -> std::process::ExitStatus {
    Command::new(env!("CARGO_BIN_EXE_bind_patch"))
        .args(args)
        .status()
        .expect("running wasm-bind-patch")
}

fn record(dir: &Path) -> std::process::ExitStatus {
    record_pair(dir, "linked0", "bound0")
}

fn record_pair(dir: &Path, linked: &str, bound: &str) -> std::process::ExitStatus {
    run(&[
        OsStr::new("record"),
        OsStr::new("--linked"),
        module(dir, linked).as_os_str(),
        OsStr::new("--bound"),
        module(dir, bound).as_os_str(),
        OsStr::new("--cache"),
        cache(dir).as_os_str(),
    ])
}

fn cache(dir: &Path) -> PathBuf {
    let cache = dir.join("cache");
    std::fs::create_dir_all(&cache).expect("a cache directory");
    cache
}

fn apply(dir: &Path, linked: &str, out: &Path) -> std::process::ExitStatus {
    run(&[
        OsStr::new("apply"),
        OsStr::new("--linked"),
        module(dir, linked).as_os_str(),
        OsStr::new("--cache"),
        cache(dir).as_os_str(),
        OsStr::new("--out"),
        out.as_os_str(),
    ])
}

#[test]
fn a_body_change_patches_to_exactly_what_a_fresh_bind_writes() {
    let dir = workspace("patch");
    assert!(record(&dir).success());
    let out = dir.join("patched.wasm");
    assert!(apply(&dir, "linked1", &out).success());
    let patched = std::fs::read(&out).expect("the patched module");
    let fresh = std::fs::read(module(&dir, "bound1")).expect("the fresh bind");
    assert!(
        patched == fresh,
        "the patched module is not the module a full bind writes"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_change_outside_bodies_asks_for_a_full_bind() {
    let dir = workspace("section");
    assert!(record(&dir).success());
    let out = dir.join("patched.wasm");
    let status = apply(&dir, "linked-changed-section", &out);
    assert_eq!(status.code(), Some(3), "an export changed");
    assert!(
        !out.exists(),
        "nothing is written when the map does not fit"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A body that changed size moves its function in the bind's order, and the
/// function and type sections with it; the module is written again and is
/// what a fresh bind writes.
#[test]
fn a_body_that_changed_size_patches_to_exactly_what_a_fresh_bind_writes() {
    let dir = workspace("size");
    assert!(record(&dir).success());
    let out = dir.join("patched.wasm");
    assert!(apply(&dir, "linked-changed-size", &out).success());
    let patched = std::fs::read(&out).expect("the patched module");
    let fresh = std::fs::read(module(&dir, "bound-changed-size")).expect("the fresh bind");
    assert!(
        patched == fresh,
        "a body that changed size is not written as a full bind writes it"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A function added shifts the positions the bind emits every function after
/// it at, and the type, export, element and name indices with them; the
/// patched module is what a fresh bind writes.
#[test]
fn a_function_added_patches_to_exactly_what_a_fresh_bind_writes() {
    let dir = workspace("fn-add");
    assert!(record(&dir).success());
    let out = dir.join("patched.wasm");
    assert!(apply(&dir, "linked-fn-add", &out).success());
    let patched = std::fs::read(&out).expect("the patched module");
    let fresh = std::fs::read(module(&dir, "bound-fn-add")).expect("the fresh bind");
    assert!(
        patched == fresh,
        "the patched module is not the module a full bind writes"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A function removed is the same change the other way: the kept bind has
/// the function, the linked module has not.
#[test]
fn a_function_removed_patches_to_exactly_what_a_fresh_bind_writes() {
    let dir = workspace("fn-remove");
    assert!(record_pair(&dir, "linked-fn-add", "bound-fn-add").success());
    let out = dir.join("patched.wasm");
    assert!(apply(&dir, "linked0", &out).success());
    let patched = std::fs::read(&out).expect("the patched module");
    let fresh = std::fs::read(module(&dir, "bound0")).expect("the fresh bind");
    assert!(
        patched == fresh,
        "the patched module is not the module a full bind writes"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A body that changed size is written again with every function index
/// renumbered, and the imports are not where they were in the module the
/// bind read: one the bind kept sits at its own index in the bound module,
/// not the index it had before binding. A body that calls it must call it
/// there, as a full bind does.
#[test]
fn a_changed_body_calls_a_kept_import_where_the_bind_writes_it() {
    let dir = workspace("import-shift");
    assert!(record_pair(&dir, "linked-import", "bound-import").success());
    let out = dir.join("patched.wasm");
    let status = apply(&dir, "linked-import-edit", &out);
    assert!(status.success(), "the patch took: {status:?}");
    let patched = std::fs::read(&out).expect("the patched module");
    let fresh = std::fs::read(module(&dir, "bound-import-edit")).expect("the fresh bind");
    assert!(
        patched == fresh,
        "the patched module is not the module a full bind writes"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// An import the bind dropped has no counterpart to call: a body that starts
/// calling it asks for a full bind instead of calling whatever function has
/// its index there.
#[test]
fn a_body_that_calls_an_import_the_bind_dropped_asks_for_a_full_bind() {
    let dir = workspace("import-dropped");
    assert!(record_pair(&dir, "linked-import", "bound-import").success());
    let out = dir.join("patched.wasm");
    let status = apply(&dir, "linked-import-dropped", &out);
    assert_eq!(status.code(), Some(3), "an import the bind dropped");
    assert!(
        !out.exists(),
        "nothing is written when the map does not fit"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
