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

/// `tilo login --new-identity` makes one identity whose root this computer
/// holds, in Tilo's own layout, and keeps it: a second run says so and
/// prints the same user public key.
#[test]
fn a_new_identity_is_made_once() {
    let scratch = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let out = Command::new(tilo_test_env::package_binary!("tilo"))
            .args(args)
            .env("TILO_IDENTITY_DIR", scratch.path())
            .output_locked()
            .unwrap();
        assert!(out.status.success(), "tilo {args:?}: {out:?}");
        (
            String::from_utf8(out.stdout).unwrap().trim().to_owned(),
            String::from_utf8(out.stderr).unwrap(),
        )
    };
    let (key, said) = run(&["login", "--new-identity", "--name", "Ada"]);
    assert_eq!(key.len(), 64, "{key}");
    assert!(said.contains("Made Ada's Tilo identity"), "{said}");
    assert!(scratch.path().join("user-identity/privkey.bin").exists());
    let (again, said) = run(&["login", "--new-identity", "--name", "Someone else"]);
    assert_eq!(again, key);
    assert!(said.contains("already holds Ada's"), "{said}");
}

/// What `tilo mcp` may hold of its own while it waits for its agent: it is a thin bridge.
const MCP_IDLE_KB: u64 = 6 * 1024;

/// `tilo mcp` answers an agent's first calls and then idles small: a bridge between the agent
/// and the person's workspace holds a few megabytes of its own (its private, anonymous memory;
/// the program's own pages are shared and the kernel takes them back), however many cores the
/// computer has, rather than a runtime's worth of threads and their heaps.
#[cfg(target_os = "linux")]
#[test]
fn tilo_mcp_idles_small() {
    use std::io::{BufRead as _, BufReader, Write as _};
    let home = tempfile::tempdir().unwrap();
    let mut mcp = Command::new(tilo_test_env::package_binary!("tilo"))
        .arg("mcp")
        .env("HOME", home.path())
        .env("XDG_STATE_HOME", home.path().join("state"))
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_RUNTIME_DIR", home.path().join("run"))
        .env_remove("TILO_DEVICE_DIR")
        .env_remove(tilo_paths::login::AGENT_SOCKET_ENV)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn_locked()
        .unwrap();
    let mut input = mcp.stdin.take().unwrap();
    let mut output = BufReader::new(mcp.stdout.take().unwrap());
    for (id, request) in [
        (1, r#""method":"initialize","params":{}"#),
        (2, r#""method":"tools/list""#),
        (3, r#""method":"tools/call","params":{"name":"tilo_ls"}"#),
    ] {
        writeln!(input, r#"{{"jsonrpc":"2.0","id":{id},{request}}}"#).unwrap();
        let mut answer = String::new();
        output.read_line(&mut answer).unwrap();
        assert!(answer.contains(&format!(r#""id":{id}"#)), "{answer}");
    }
    std::thread::sleep(std::time::Duration::from_secs(2));
    let status = std::fs::read_to_string(format!("/proc/{}/status", mcp.id())).unwrap();
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap()
    };
    let (own, program, threads) = (field("RssAnon:"), field("RssFile:"), field("Threads:"));
    drop(input);
    let _ = mcp.wait();
    assert!(
        own < MCP_IDLE_KB,
        "tilo mcp holds {own} kB of its own (and {program} kB of its program) with {threads} \
         threads, past {MCP_IDLE_KB} kB"
    );
}
