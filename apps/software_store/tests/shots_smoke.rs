//! Smoke test for `software_store --shots PATH`.
//!
//! Invokes the freshly-built binary with `--shots <tmp>` and asserts the
//! resulting file exists and starts with the canonical PNG magic header.
//! Same pattern the terminal / settings / file-explorer smoke tests use.

use std::fs;
use std::process::Command;

const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

fn assert_shot(view: Option<&str>, viewport: Option<&str>, tag: &str) {
    let bin = env!("CARGO_BIN_EXE_software_store");
    let out = std::env::temp_dir().join(format!(
        "software-store-shots-{tag}-{}.png",
        std::process::id()
    ));
    let _ = fs::remove_file(&out);

    let mut cmd = Command::new(bin);
    cmd.arg("--shots").arg(&out);
    if let Some(v) = viewport {
        cmd.arg("--viewport").arg(v);
    }
    if let Some(view) = view {
        cmd.env("SOFTWARE_STORE_SHOT_VIEW", view);
    }
    let status = cmd.status().expect("spawn software_store --shots");
    assert!(
        status.success(),
        "software_store --shots exited with {status:?} ({tag})"
    );

    let bytes = fs::read(&out).expect("read shots PNG");
    assert!(
        bytes.len() >= 8,
        "PNG must have at least an 8-byte header ({tag})"
    );
    assert_eq!(
        &bytes[..8],
        &PNG_MAGIC,
        "expected PNG magic at start of {} ({tag})",
        out.display()
    );
    let _ = fs::remove_file(&out);
}

#[test]
fn shots_browse_home_writes_png() {
    assert_shot(None, None, "browse");
}

#[test]
fn shots_search_results_writes_png() {
    assert_shot(Some("search:editor"), None, "search");
}

#[test]
fn shots_detail_page_writes_png() {
    assert_shot(Some("detail:org.gimp.GIMP"), None, "detail");
}

#[test]
fn shots_remotes_screen_writes_png() {
    assert_shot(Some("remotes"), None, "remotes");
}

#[test]
fn shots_flatpakref_sheet_writes_png() {
    assert_shot(Some("flatpakref"), None, "flatpakref");
}
