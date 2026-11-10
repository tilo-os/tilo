//! Every old `tilo-<name>` of a VM-wide component still works: run as a link
//! to `tilo`, it runs its program from Tilo's program directory with the
//! arguments it was given, `--help` included, and adds nothing of its own. A
//! session component's names link to their programs directly instead, so
//! nothing of `tilo` runs at all.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use tilo_test_env::SpawnUnderEnvLock as _;

/// The old names, as `tilo` lists them in `src/aliases.rs`.
fn moved() -> Vec<String> {
    let source = include_str!("../src/aliases.rs");
    let list = source
        .split_once("pub const MOVED: &[&str] = &[")
        .and_then(|(_, rest)| rest.split_once("];"))
        .expect("aliases.rs declares MOVED")
        .0;
    list.split(',')
        .map(|item| item.trim().trim_matches('"').to_owned())
        .filter(|name| !name.is_empty())
        .collect()
}

#[test]
fn it_says_which_release_it_is() {
    // `cli-release.sh build` sets TILO_CLI_VERSION from the tag it builds, so a person reporting
    // a problem can say which release they have; a build from the tree says the crate's own.
    let out = Command::new(tilo_test_env::package_binary!("tilo"))
        .arg("--version")
        .output_locked()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let said = String::from_utf8(out.stdout).unwrap();
    let version = said
        .trim()
        .strip_prefix("tilo ")
        .unwrap_or_else(|| panic!("{said:?}"));
    assert!(!version.is_empty(), "{said:?}");
}

#[test]
fn every_old_name_runs_its_help_through_the_alias() {
    let tmp = tempfile::tempdir().unwrap();
    let libexec = tmp.path().join("libexec");
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&libexec).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let moved = moved();
    assert!(moved.len() > 10, "{moved:?}");
    for name in &moved {
        let program = libexec.join(name);
        std::fs::write(&program, "#!/bin/sh\nprintf '%s|%s\\n' \"$0\" \"$*\"\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let alias = bin.join(name);
        std::os::unix::fs::symlink(tilo_test_env::package_binary!("tilo"), &alias).unwrap();
        let out = Command::new(&alias)
            .args(["--help", "x y"])
            .env("TILO_LIBEXEC_DIR", &libexec)
            .output_locked()
            .unwrap();
        assert!(out.status.success(), "{name}: {out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            format!("{}|--help x y\n", program.display()),
            "{name} runs its program with its arguments"
        );
        assert!(out.stderr.is_empty(), "{name} adds nothing: {out:?}");
    }
    // A missing program says so in one sentence.
    std::fs::remove_file(libexec.join("tilo-vm")).unwrap();
    let out = Command::new(bin.join("tilo-vm"))
        .arg("--help")
        .env("TILO_LIBEXEC_DIR", &libexec)
        .output_locked()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        "tilo-vm: tilo-vm isn't on this computer\n"
    );
}
