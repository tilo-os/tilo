//! CI gate for the `scripts/headless-examples/` Python scripts. Each
//! advertises a documented, copy-pastable flow (full-loop.py =
//! spawn+drive+snapshot, library-demo.py = same via the `TiloClient` API);
//! without a gate they bit-rot the next time someone tweaks the wire
//! format or renames a `test_id`.
//!
//! `#[ignore]` by default: spawns a real `shell --backend headless` child,
//! needing a wgpu adapter (Mesa llvmpipe is enough). Nightly runs them;
//! opt in locally with:
//! ```text
//! cargo test -p shell --test it headless_examples_smoke:: -- --ignored --test-threads 1
//! ```
//! The scripts are pointed at the compositor Cargo built for this test run
//! through `TILO_SHELL_BINARY`, not at a possibly stale `target/debug/shell`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<repo>/crates/session/shell`.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("shell crate must live under <repo>/crates/session")
        .to_path_buf()
}

/// The compositor Cargo built for this integration-test run.
fn shell_binary() -> PathBuf {
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_shell"));
    assert!(
        binary.exists(),
        "expected compositor binary at {}",
        binary.display()
    );
    binary
}

fn run_script(script_name: &str) -> std::process::Output {
    tilo_test_env::require_isolated_desktop();
    let script = repo_root()
        .join("scripts")
        .join("headless-examples")
        .join(script_name);
    Command::new("python3")
        .arg(&script)
        .env("TILO_SHELL_BINARY", shell_binary())
        .current_dir(repo_root())
        .output()
        .expect("spawn python3")
}

#[test]
#[ignore = "spawns a real headless compositor; opt in with --ignored"]
fn full_loop_writes_two_pngs() {
    // Pre-clean so we know the script produced the artefacts itself.
    let _ = std::fs::remove_file("/tmp/full-loop-palette.png");
    let _ = std::fs::remove_file("/tmp/full-loop-final.png");

    let out = run_script("full-loop.py");
    assert!(
        out.status.success(),
        "full-loop.py failed: stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let palette = std::fs::metadata("/tmp/full-loop-palette.png").expect("palette png");
    let final_ = std::fs::metadata("/tmp/full-loop-final.png").expect("final png");
    assert!(palette.len() > 1024, "palette PNG should be > 1KB");
    assert!(final_.len() > 1024, "final PNG should be > 1KB");
}

#[test]
#[ignore = "spawns a real headless compositor; opt in with --ignored"]
fn library_demo_writes_two_pngs_via_tilo_client() {
    let _ = std::fs::remove_file("/tmp/library-demo-palette.png");
    let _ = std::fs::remove_file("/tmp/library-demo-final.png");

    let out = run_script("library-demo.py");
    assert!(
        out.status.success(),
        "library-demo.py failed: stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let palette = std::fs::metadata("/tmp/library-demo-palette.png").expect("palette png");
    let final_ = std::fs::metadata("/tmp/library-demo-final.png").expect("final png");
    assert!(palette.len() > 1024, "palette PNG should be > 1KB");
    assert!(final_.len() > 1024, "final PNG should be > 1KB");

    // library-demo prints a state_dump excerpt, so a wire format that drops
    // the chrome.overlays object regresses here.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("chrome.overlays = "),
        "library-demo.py should print chrome.overlays from state_dump (proves \
         the response shape is still {{state: {{chrome: ...}}}}); got: {stdout}"
    );
}
