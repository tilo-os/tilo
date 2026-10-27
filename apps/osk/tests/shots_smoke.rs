//! Smoke test for `osk --shots PATH`.
//!
//! Invokes the freshly-built binary with `--shots <tmp>` and asserts the
//! resulting file exists and starts with the canonical PNG magic header.
//! Mirrors `apps/file-explorer/tests/shots_smoke.rs`.

use std::fs;
use std::process::Command;

const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

#[test]
fn osk_shots_flag_writes_png() {
    let bin = tilo_test_env::package_binary!("osk");
    let out = std::env::temp_dir().join(format!("osk-shots-smoke-{}.png", std::process::id()));
    let _ = fs::remove_file(&out);

    let status = Command::new(bin)
        .arg("--shots")
        .arg(&out)
        .status()
        .expect("spawn osk --shots");
    assert!(status.success(), "osk --shots exited with {status:?}");

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
