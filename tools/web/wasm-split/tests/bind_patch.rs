//! A body-only change patches wasm-bindgen's earlier output to exactly what a
//! full bind writes, and anything else asks for the full bind
//! (src/bin/bind_patch.rs).
//!
//! The fixtures are a small module with locals of two types, calls, an
//! indirect call and a table, bound by the pinned wasm-bindgen: `bound0` is
//! its real output for `linked0`, `bound1` the real output for `linked1`,
//! whose only change is one `i32.const` value in `$e`. They are kept as hex
//! (the repository takes no new binary blobs) and written out here.

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
    run(&[
        OsStr::new("record"),
        OsStr::new("--linked"),
        module(dir, "linked0").as_os_str(),
        OsStr::new("--bound"),
        module(dir, "bound0").as_os_str(),
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

#[test]
fn a_body_that_changed_size_asks_for_a_full_bind() {
    let dir = workspace("size");
    assert!(record(&dir).success());
    let out = dir.join("patched.wasm");
    let status = apply(&dir, "linked-changed-size", &out);
    assert_eq!(
        status.code(),
        Some(3),
        "an instruction added to a body moves the function the bind emits it at"
    );
    assert!(!out.exists());
    let _ = std::fs::remove_dir_all(&dir);
}
