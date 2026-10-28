#![allow(
    clippy::panic,
    reason = "a test fixture that cannot be written fails the test that asked for it"
)]

use std::io::Write as _;
use std::path::Path;

/// Write `contents` to `path` as an executable a test then runs.
///
/// The file is written by a short-lived child process, never through a file
/// descriptor of this one. Tests share a process and spawn children from
/// many threads: a child forked by another test while this process held the
/// file open for writing would carry that descriptor until it execs, and
/// running the file in that window fails with "Text file busy" (`ETXTBSY`).
pub fn write_executable(path: &Path, contents: &str) {
    let mut writer = std::process::Command::new("/bin/sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("no shell to write {}: {e}", path.display()));
    let Some(mut stdin) = writer.stdin.take() else {
        panic!("the writer of {} has no stdin", path.display());
    };
    stdin
        .write_all(contents.as_bytes())
        .unwrap_or_else(|e| panic!("writing {} failed: {e}", path.display()));
    drop(stdin);
    let status = writer
        .wait()
        .unwrap_or_else(|e| panic!("the writer of {} was lost: {e}", path.display()));
    assert!(
        status.success(),
        "writing {} failed: {status}",
        path.display()
    );
}
