//! Smoke test for `settings --shots PATH`.
//!
//! Invokes the freshly-built binary with `--shots <tmp>` and asserts the
//! resulting file exists and starts with the canonical PNG magic header.
//! Same pattern the terminal and file-explorer smoke tests use.

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

const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

#[test]
fn settings_shots_flag_writes_png() {
    let bin = env!("CARGO_BIN_EXE_settings");
    let out = std::env::temp_dir().join(format!("settings-shots-smoke-{}.png", std::process::id()));
    let _ = fs::remove_file(&out);

    let status = Command::new(bin)
        .arg("--shots")
        .arg(&out)
        .status()
        .expect("spawn settings --shots");
    assert!(status.success(), "settings --shots exited with {status:?}");

    let bytes = fs::read(&out).expect("read shots PNG");
    assert!(bytes.len() >= 8, "PNG must have at least an 8-byte header");
    assert_eq!(
        &bytes[..8],
        &PNG_MAGIC,
        "expected PNG magic header at start of {}",
        out.display()
    );

    let _ = fs::remove_file(&out);
}
