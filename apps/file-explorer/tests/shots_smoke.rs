//! Smoke test for `file-explorer --shots PATH`.
//!
//! Invokes the freshly-built binary with `--shots <tmp>` and asserts the
//! resulting file exists and starts with the canonical PNG magic header.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

use std::fs;
use std::process::Command;

const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

#[test]
fn file_explorer_shots_flag_writes_png() {
    let bin = env!("CARGO_BIN_EXE_file-explorer");
    let out = std::env::temp_dir().join(format!(
        "file-explorer-shots-smoke-{}.png",
        std::process::id()
    ));
    let _ = fs::remove_file(&out);

    let status = Command::new(bin)
        .arg("--shots")
        .arg(&out)
        .status()
        .expect("spawn file-explorer --shots");
    assert!(
        status.success(),
        "file-explorer --shots exited with {status:?}"
    );

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
