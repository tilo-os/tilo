//! CI gate for the `scripts/headless-examples/` Python scripts.
//!
//! Every script in that dir advertises a documented, copy-pastable
//! flow (full-loop.py = spawn+drive+snapshot, library-demo.py = same
//! via the NiriosClient API). Without a gate they'll bit-rot the next
//! time someone tweaks the wire format or renames a `test_id`.
//!
//! Each test:
//! 1. Confirms the compiled compositor binary exists at
//!    `target/debug/quick-desktop` (the script's hard-coded path).
//! 2. Shells out to `python3 scripts/headless-examples/<script>` with
//!    the test-runner's CWD = repo root.
//! 3. Asserts exit code 0 and that the script's documented output
//!    files were written.
//!
//! ## Why `#[ignore]` by default
//!
//! These tests spawn a real `quick-desktop --backend headless` child,
//! which needs a wgpu adapter. CI runners without a software-rasterizer
//! Mesa would fail on adapter init. Opt-in:
//!
//! ```text
//! cargo build -p quick-desktop --bin quick-desktop
//! cargo test -p quick-desktop --test headless_examples_smoke -- --ignored --test-threads 1
//! ```

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<repo>/quick-shell`; the repo root is one up.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("quick-shell parent")
        .to_path_buf()
}

fn assert_binary_exists() {
    let bin = repo_root().join("target").join("debug").join("quick-desktop");
    assert!(
        bin.exists(),
        "expected compositor binary at {bin:?} — run \
         `cargo build -p quick-desktop --bin quick-desktop` before this test"
    );
}

fn run_script(script_name: &str) -> std::process::Output {
    let script = repo_root()
        .join("scripts")
        .join("headless-examples")
        .join(script_name);
    Command::new("python3")
        .arg(&script)
        .current_dir(repo_root())
        .output()
        .expect("spawn python3")
}

#[test]
#[ignore = "spawns a real headless compositor; opt in with --ignored"]
fn full_loop_writes_two_pngs() {
    assert_binary_exists();
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
fn library_demo_writes_two_pngs_via_qos_client() {
    assert_binary_exists();
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

    // Sanity: library-demo prints a state_dump excerpt. If the wire
    // format ever drops the chrome.overlays object, this regression
    // surfaces here.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("chrome.overlays = "),
        "library-demo.py should print chrome.overlays from state_dump (proves \
         the response shape is still {{state: {{chrome: ...}}}}); got: {stdout}"
    );
}
