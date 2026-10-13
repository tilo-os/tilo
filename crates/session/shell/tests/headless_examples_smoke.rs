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
    clippy::expect_used,
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

/// The `tilo-control` client, built by the workspace beside the compositor:
/// Cargo only exports `CARGO_BIN_EXE_*` for this package's own binaries.
fn control_binary() -> PathBuf {
    let binary = shell_binary().with_file_name("tilo-control");
    assert!(
        binary.exists(),
        "expected tilo-control beside the compositor at {}; build the workspace first",
        binary.display()
    );
    binary
}

fn run_script(script_name: &str) -> std::process::Output {
    run_script_with(script_name, &[])
}

/// `script_name` with extra environment, for proofs that need more than the compositor.
fn run_script_with(script_name: &str, env: &[(&str, PathBuf)]) -> std::process::Output {
    tilo_test_env::require_isolated_desktop();
    let script = repo_root()
        .join("scripts")
        .join("headless-examples")
        .join(script_name);
    Command::new("python3")
        .arg(&script)
        .env("TILO_SHELL_BINARY", shell_binary())
        .envs(env.iter().map(|(key, value)| (*key, value)))
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

/// A model drives a live compositor over MCP: initialize, tools, lazy resources, an
/// invocation read back independently, and a refusal that arrives as `isError`.
#[test]
#[ignore = "spawns a real headless compositor; opt in with --ignored"]
fn mcp_live_proof_drives_a_compositor_over_mcp() {
    let out = run_script_with(
        "mcp-live-proof.py",
        &[("TILO_CONTROL_BINARY", control_binary())],
    );
    assert!(
        out.status.success(),
        "mcp-live-proof.py failed: stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Two agents share one machine without colliding on pixels or keys. The compositor
/// must be built with `--features agent-control` (the nightly lane builds it so) and the
/// terminal app must already be built (`TILO_TERMINAL_BINARY`, else `target/debug/terminal`).
#[cfg(feature = "agent-control")]
#[test]
#[ignore = "spawns a real headless compositor and a terminal; opt in with --ignored"]
fn agent_seats_proof_keeps_two_agents_apart() {
    let out = run_script("agent-seats-proof.py");
    assert!(
        out.status.success(),
        "agent-seats-proof.py failed: stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
