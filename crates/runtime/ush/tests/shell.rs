//! ush's own shell on this device, through the real `tilo-ush` binary:
//! Tilo's commands in-process over real files, a pipe between them, a
//! program that is none of them run on the machine, and a file command
//! carried out outside a guest, keeping what it replaced.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::Path;
use std::process::{Command, Output};

fn sh(dir: &Path, data: &Path, script: &str) -> Output {
    let shell = dir.join("ush-sh");
    if !shell.exists() {
        std::os::unix::fs::symlink(tilo_test_env::package_binary!("tilo-ush"), &shell).unwrap();
    }
    Command::new(&shell)
        .arg("-c")
        .arg(script)
        .current_dir(dir)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", dir)
        .env("XDG_DATA_HOME", data)
        // No guest here: the broker is not there.
        .env("TILO_USH_BROKER", dir.join("no-broker.sock"))
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn the_shell_runs_tilos_commands_and_falls_back_to_the_machine() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::create_dir(dir.join("notes")).unwrap();
    for name in ["a.md", "b.md", "c.txt"] {
        std::fs::write(dir.join("notes").join(name), format!("{name}\n")).unwrap();
    }

    let out = sh(
        dir,
        data.path(),
        "ls notes | wc -l; cat notes/*.md | grep -c md",
    );
    assert_eq!(text(&out.stdout), "3\n2\n", "{}", text(&out.stderr));
    assert!(out.status.success());

    // `uname` is none of Tilo's own commands: on a Linux device it runs
    // there, and its output pipes into one of them.
    let out = sh(dir, data.path(), "uname -s | tr a-z A-Z");
    assert_eq!(text(&out.stdout), "LINUX\n", "{}", text(&out.stderr));

    // Outside a guest a file command keeps what it replaces aside.
    let out = sh(
        dir,
        data.path(),
        "echo old > f.txt; echo new > g.txt; mv g.txt f.txt; cat f.txt",
    );
    assert_eq!(
        text(&out.stdout).lines().last(),
        Some("new"),
        "{}",
        text(&out.stderr)
    );
    assert!(!dir.join("g.txt").exists());
    let kept = data.path().join("ush/recoverable");
    assert!(
        std::fs::read_dir(&kept).is_ok_and(|mut entries| entries.next().is_some()),
        "the replaced f.txt is kept in {}",
        kept.display()
    );

    let out = sh(dir, data.path(), "no-such-program-here");
    assert_eq!(out.status.code(), Some(127));
    assert!(
        text(&out.stderr).contains("command not found"),
        "{}",
        text(&out.stderr)
    );
}
