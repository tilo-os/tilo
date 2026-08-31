//! Smoke test for `terminal --shots PATH`.
//!
//! Invokes the freshly-built binary with `--shots <tmp>` and asserts the
//! resulting file exists and starts with the canonical PNG magic header.
//!
//! Runs serially (no `--test-threads` requirement here — just one test)
//! and is hermetic: it writes into the OS tempdir under a unique name.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::fs;
use std::process::Command;

/// PNG magic header — first 8 bytes of every PNG file.
const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

#[test]
fn terminal_shots_flag_writes_png() {
    let bin = env!("CARGO_BIN_EXE_terminal");
    let out = std::env::temp_dir().join(format!("terminal-shots-smoke-{}.png", std::process::id()));
    // Idempotent: clear any leftover from a previous run.
    let _ = fs::remove_file(&out);

    let status = Command::new(bin)
        .arg("--shots")
        .arg(&out)
        .status()
        .expect("spawn terminal --shots");
    assert!(status.success(), "terminal --shots exited with {status:?}");

    let bytes = fs::read(&out).expect("read shots PNG");
    assert!(bytes.len() >= 8, "PNG must have at least an 8-byte header");
    assert_eq!(
        &bytes[..8],
        &PNG_MAGIC,
        "expected PNG magic header at start of {}",
        out.display()
    );

    // Cleanup so a re-run doesn't pile up tempfiles.
    let _ = fs::remove_file(&out);
}
