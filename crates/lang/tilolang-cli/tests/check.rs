//! `tilolang check`: every package in `apps/tl` checks clean, the platform's packages it uses
//! (`use so.tilo.device`) found as `build` and `layout-check` find them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::path::Path;
use std::process::Command;

#[test]
fn every_package_in_apps_tl_checks_clean() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl");
    let mut dirs: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        // A tilolang package is a directory of `.tl` files; a native app beside them (the
        // session's committed `surface.toml`) is what packages `use`, not one to check.
        .filter(|path| tilolang::source::source_paths(path).is_ok_and(|files| !files.is_empty()))
        .collect();
    dirs.sort();
    assert!(dirs.iter().any(|dir| dir.ends_with("shell")), "{dirs:?}");
    let mut failed = Vec::new();
    for dir in &dirs {
        let output = Command::new(env!("CARGO_BIN_EXE_tilolang"))
            .arg("check")
            .arg(dir)
            .output()
            .unwrap();
        if !output.status.success() {
            failed.push(format!(
                "{}: {}\n{}",
                dir.display(),
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}
