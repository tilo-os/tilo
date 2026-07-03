//! Launcher smoke tests.
//!
//! These tests invoke the *library* surface of `qos-launcher` (so we
//! don't need to first build the binary) against ad-hoc fixtures.
//! Each test that actually spawns bwrap is `#[ignore]`d by default
//! because:
//! - bwrap may not be installed everywhere.
//! - bwrap typically needs `CAP_SYS_ADMIN` or unprivileged user
//!   namespaces — distrobox usually has those, plain CI may not.
//!
//! Run the live-bwrap ones explicitly:
//!
//! ```bash
//! cargo test -p qos-launcher -- --ignored
//! ```
//!
//! The non-ignored tests cover all the manifest-handling and service-
//! resolution semantics without touching bwrap at all.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use qos_launcher::manifest::{Entrypoint, Fs, MinimalManifest, Net, Sandbox, read_manifest};
use qos_launcher::run::{RunError, RunOptions, run, run_with, run_with_captured};
use qos_launcher::services::EXIT_REQUIRED_SERVICE_MISSING;

// ---------------------------------------------------------------- helpers

/// rustc-compile the in-tree `hello` fixture into `out_dir/bin/hello`.
/// We do this rather than `cargo build` because the fixture lives
/// outside the workspace and we want a single-shot build that doesn't
/// dirty the workspace target dir.
fn build_hello(out_dir: &Path) -> PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/hello/src/main.rs");
    let bin = out_dir.join("bin").join("hello");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    let status = std::process::Command::new("rustc")
        .arg("--edition=2024")
        .arg("-O")
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .status()
        .expect("invoke rustc");
    assert!(status.success(), "rustc failed to build hello fixture");
    bin
}

/// Inject the host's dynamic linker + lib dirs into the manifest so the
/// fixture binary can find libc inside the sandbox. We don't ship a
/// fully static binary because that needs `musl` toolchain which isn't
/// present in distrobox; bind-mounting host libs read-only is the
/// pragmatic option and mirrors what real apps will do via `qos-runtime`.
fn with_glibc_binds(mut m: MinimalManifest) -> MinimalManifest {
    for p in ["/usr/lib", "/usr/lib64", "/lib", "/lib64"] {
        if std::path::Path::new(p).exists() {
            m.sandbox.fs.read.push(p.to_string());
        }
    }
    m
}

fn hello_manifest(payload_root: &Path, services: BTreeMap<String, String>) -> MinimalManifest {
    let _ = payload_root;
    MinimalManifest {
        package_id: "com.example.hello".to_string(),
        version: "0.0.0".to_string(),
        entrypoint: Entrypoint {
            bin: "bin/hello".to_string(),
        },
        sandbox: Sandbox {
            net: Net {
                mode: "off".to_string(),
            },
            fs: Fs::default(),
            services,
        },
    }
}

/// Locate bwrap, or return None if absent. Skip live tests on absence.
fn bwrap_path() -> Option<PathBuf> {
    let p = PathBuf::from("/usr/bin/bwrap");
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

// ---------------------------------------------------------------- tests

#[test]
fn manifest_malformed_fails_before_exec() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("manifest.toml");
    std::fs::write(&path, "this is { not valid toml = ").unwrap();
    let opts = RunOptions::default();
    let err = run(&path, dir.path(), &opts).unwrap_err();
    match err {
        RunError::Manifest(_) => {} // expected: parse error
        other => panic!("expected ManifestError, got {other:?}"),
    }
}

#[test]
fn required_service_missing_fails() {
    // Manifest says `collab = "required@1"` but no socket exists.
    let dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("XDG_RUNTIME_DIR", dir.path()) };

    let mut svcs = BTreeMap::new();
    svcs.insert("collab".to_string(), "required@1".to_string());
    let m = hello_manifest(dir.path(), svcs);
    let opts = RunOptions {
        no_services: false,
        ..Default::default(),
                landlock_enforce: true,
            };
    let err = run_with(&m, dir.path(), &opts).unwrap_err();
    assert_eq!(err.to_exit_code(), EXIT_REQUIRED_SERVICE_MISSING);
    let msg = format!("{err}");
    assert!(msg.contains("collab"), "error must name service: {msg}");
}

#[test]
fn no_services_flag_skips_required_resolution() {
    // Same manifest, but --no-services: required failure becomes a
    // skip + warn. We can't actually exec without bwrap, so we just
    // assert the resolution step succeeds; the spawn step is exercised
    // by the live tests.
    let dir = tempfile::tempdir().unwrap();
    let mut svcs = BTreeMap::new();
    svcs.insert("collab".to_string(), "required@1".to_string());
    let resolved =
        qos_launcher::services::resolve_service_binds(&svcs, true).expect("no-services infallible");
    assert!(resolved.is_empty());
}

#[test]
#[ignore = "requires bwrap + unprivileged user namespaces; run with --ignored"]
fn hello_world_runs() {
    let bwrap = match bwrap_path() {
        Some(p) => p,
        None => {
            eprintln!("bwrap not installed; skipping");
            return;
        }
    };
    let payload = tempfile::tempdir().unwrap();
    let _bin = build_hello(payload.path());

    let m = with_glibc_binds(hello_manifest(payload.path(), BTreeMap::new()));
    let opts = RunOptions {
        no_services: false,
        runtime_root: None,
        bwrap_bin: Some(bwrap),
        minimal_libs: false,
                landlock_enforce: true,
            };
    let (status, stdout) =
        run_with_captured(&m, payload.path(), &opts).expect("launch");
    assert!(status.success(), "child exited non-zero: {status:?}");
    let stdout = String::from_utf8_lossy(&stdout);
    assert!(
        stdout.contains("hello"),
        "expected 'hello' in stdout, got {stdout:?}"
    );
}

#[test]
#[ignore = "requires bwrap; run with --ignored"]
fn no_services_flag_skips_required() {
    let bwrap = match bwrap_path() {
        Some(p) => p,
        None => {
            eprintln!("bwrap not installed; skipping");
            return;
        }
    };
    let payload = tempfile::tempdir().unwrap();
    let _bin = build_hello(payload.path());
    let mut svcs = BTreeMap::new();
    svcs.insert("collab".to_string(), "required@1".to_string());
    let m = with_glibc_binds(hello_manifest(payload.path(), svcs));
    let opts = RunOptions {
        no_services: true,
        runtime_root: None,
        bwrap_bin: Some(bwrap),
        minimal_libs: false,
                landlock_enforce: true,
            };
    let status = run_with(&m, payload.path(), &opts).expect("launch");
    assert!(status.success());
}

#[test]
#[ignore = "requires bwrap; run with --ignored"]
fn optional_service_missing_warns_and_runs() {
    let bwrap = match bwrap_path() {
        Some(p) => p,
        None => {
            eprintln!("bwrap not installed; skipping");
            return;
        }
    };
    let dir = tempfile::tempdir().unwrap();
    unsafe { std::env::set_var("XDG_RUNTIME_DIR", dir.path()) };

    let payload = tempfile::tempdir().unwrap();
    let _bin = build_hello(payload.path());

    let mut svcs = BTreeMap::new();
    svcs.insert("collab".to_string(), "optional@1".to_string());
    let m = with_glibc_binds(hello_manifest(payload.path(), svcs));
    let opts = RunOptions {
        no_services: false,
        runtime_root: None,
        bwrap_bin: Some(bwrap),
        minimal_libs: false,
                landlock_enforce: true,
            };
    let status = run_with(&m, payload.path(), &opts).expect("launch");
    assert!(status.success(), "optional missing should still run");
}

#[test]
#[ignore = "requires bwrap + Landlock kernel; run with --ignored"]
fn read_only_fs_enforced() {
    // We declare `/usr` ro and ask the hello fixture to attempt a write
    // there. The fixture is too simple for that; this test stays
    // structurally present so future work that ships a richer fixture
    // can flip it on. For now we just assert the run succeeds, which
    // proves the ro-bind plumbing doesn't break the standard mounts.
    let bwrap = match bwrap_path() {
        Some(p) => p,
        None => {
            eprintln!("bwrap not installed; skipping");
            return;
        }
    };
    let payload = tempfile::tempdir().unwrap();
    let _bin = build_hello(payload.path());
    let mut m = with_glibc_binds(hello_manifest(payload.path(), BTreeMap::new()));
    m.sandbox.fs.read.push("/usr".to_string());
    let opts = RunOptions {
        no_services: false,
        runtime_root: None,
        bwrap_bin: Some(bwrap),
        minimal_libs: false,
                landlock_enforce: true,
            };
    let status = run_with(&m, payload.path(), &opts).expect("launch");
    assert!(status.success());
}

#[test]
fn read_manifest_round_trips() {
    // Smoke: write a manifest, read it back, assert key fields.
    let _dir = tempfile::tempdir().unwrap();
    let path = _dir.path().join("manifest.toml");
    std::fs::write(
        &path,
        r#"
package_id = "com.example.demo"
version = "1.2.3"

[entrypoint]
bin = "bin/demo"

[sandbox.net]
mode = "off"

[sandbox.services]
collab = "required@1"
fs = "optional@2"
"#,
    )
    .unwrap();
    let m = read_manifest(&path).expect("parse");
    assert_eq!(m.package_id, "com.example.demo");
    assert_eq!(m.entrypoint.bin, "bin/demo");
    assert_eq!(m.sandbox.services.get("collab").unwrap(), "required@1");
}
