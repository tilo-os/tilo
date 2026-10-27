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

const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// The default `--shots` render writes a PNG; on a host without extended
/// attributes the projection-backed scenarios are not offered, so they fail
/// instead of producing misleading images.
#[test]
fn shots_flag_writes_png_and_refuses_unavailable_projection_scenarios() {
    #[cfg(windows)]
    unavailable_projection_scenarios_fail_instead_of_producing_misleading_images();
    file_explorer_shots_flag_writes_png();
}

#[cfg(windows)]
fn unavailable_projection_scenarios_fail_instead_of_producing_misleading_images() {
    let directory = tempfile::tempdir().unwrap();
    for scenario in ["sync", "thumbnails-fs"] {
        let output = directory.path().join(format!("{scenario}.png"));
        let result = crate::common::app_command()
            .arg("--shots")
            .arg(&output)
            .args(["--scenario", scenario])
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(!output.exists());
        assert!(String::from_utf8_lossy(&result.stderr).contains("unknown screenshot scenario"));
    }
}

fn file_explorer_shots_flag_writes_png() {
    let out = std::env::temp_dir().join(format!(
        "file-explorer-shots-smoke-{}.png",
        std::process::id()
    ));
    let _ = fs::remove_file(&out);

    let status = crate::common::app_command()
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
