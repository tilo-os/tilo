//! Every old `tilo-<name>` of a VM-wide component still works: run as a link
//! to `tilo`, it runs its program from Tilo's program directory with the
//! arguments it was given, `--help` included, and adds nothing of its own. A
//! session component's names link to their programs directly instead, so
//! nothing of `tilo` runs at all.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard; the memory it reads adds what the \
              kernel reports"
)]

use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use tilo_test_env::SpawnUnderEnvLock as _;

#[path = "hosted_journey.rs"]
mod hosted_journey;
#[path = "machine_tests.rs"]
mod machine_tests;
mod ticket_action;

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
    assert!(moved.len() >= 10, "{moved:?}");
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
/// holds, in the store a Tilo session on this account reads, and keeps it: a
/// second time says so and prints the same user public key. The session finds
/// that person there and verifies them as its own.
#[test]
fn a_new_identity_is_made_once_where_the_session_reads_it() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config");
    let root = home.path().join("identity-root");
    let run = |args: &[&str]| {
        let out = Command::new(tilo_test_env::package_binary!("tilo"))
            .args(args)
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", &config)
            .env("TILO_IDENTITY_ROOT", &root)
            .env("TILO_IDENTITY_FILE_SECRETS", "1")
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
    let (again, said) = run(&["login", "--new-identity", "--name", "Someone else"]);
    assert_eq!(again, key);
    assert!(said.contains("already holds Ada's"), "{said}");

    let identity_dir = config.join("tilo/identity");
    let active = tilo_identity::store::resolve_active_identity(&identity_dir)
        .expect("the session's identity store holds the person");
    let metadata = tilo_identity::store::load_metadata_from_path(&active).unwrap();
    assert_eq!(hex::encode(metadata.group_public_key), key);
    assert_eq!(metadata.display_name, "Ada");
    assert!(metadata.single_device, "this computer holds the root");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(tilo_identity::store::FILE_SECRETS_ENV, "1");
    tilo_identity::provisioning::verify_collaboration_identity(
        &identity_dir,
        &tilo_device_identity::IdentityPaths::under(&root),
        tilo_clock::unix_now_ns(),
    )
    .expect("the session verifies the person and this device as its own");
}

#[test]
fn hosted_public_key_reuses_only_the_existing_device_key() {
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().canonicalize().unwrap();
    let identity = home.join("identity");
    let key_file = tilo_device_identity::IdentityPaths::under(&identity).device_privkey;
    let public_key = |existing: bool| {
        Command::new(tilo_test_env::package_binary!("tilo"))
            .args(["device", "public-key"])
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_DATA_HOME", home.join("data"))
            .env("XDG_RUNTIME_DIR", home.join("run"))
            .env_remove(tilo_api::client::DESKTOP_ENV)
            .env("TILO_IDENTITY_ROOT", &identity)
            .env(tilo_identity::provisioning::FILE_SECRETS_ENV, "1")
            .env("TILO_HOSTED_EXISTING_KEY", if existing { "1" } else { "0" })
            .output_locked()
            .unwrap()
    };
    let missing = || {
        let output = public_key(true);
        assert!(!output.status.success(), "{output:?}");
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("device key"));
        assert!(!key_file.exists());
    };
    missing();

    let first = public_key(false);
    assert!(first.status.success(), "{first:?}");
    assert_eq!(
        hex::decode(String::from_utf8_lossy(&first.stdout).trim())
            .unwrap()
            .len(),
        32
    );
    let private = std::fs::read(&key_file).unwrap();
    let retry = public_key(true);
    assert!(retry.status.success(), "{retry:?}");
    assert_eq!(retry.stdout, first.stdout);
    assert!(std::fs::read(&key_file).unwrap() == private);

    std::fs::remove_file(&key_file).unwrap();
    missing();
}

/// What `tilo mcp` may hold of its own while it waits for its agent: it is a thin bridge. What
/// counts is what it allocates — its heaps, its stacks and the buffers it reads and writes — not
/// the pages its program's statics take to be relocated as it loads: those are the program's, paid
/// by every process that runs it, and keeping the toolkit and the media stack out of them is
/// `tools/policy/check_crate_graph.py`'s job.
const MCP_IDLE_KB: u64 = 7 * 1024;

/// How far the process's own memory may move between two readings half a second apart and still
/// be called idle: an allocator does not hand pages back at once, and startup's last allocations
/// go by then.
const IDLE_DRIFT_KB: u64 = 16;

/// Whether `line` heads a mapping of `/proc/<pid>/smaps`.
fn mapping(line: &str) -> bool {
    line.as_bytes().first().is_some_and(u8::is_ascii_hexdigit) && line.contains('-')
}

/// What `pid` holds, in kB: of its own, of its program, and in threads.
///
/// Its own is its private anonymous memory less the pages of its own executable that the loader
/// wrote to relocate its statics: a program's statics are the program's, paid by every process
/// that runs it and grown by any dependency, while what the bridge holds is what it allocates.
fn holding(pid: u32) -> (u64, u64, u64) {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or_else(|| panic!("{name} in {status}"))
    };
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).unwrap();
    let smaps = std::fs::read_to_string(format!("/proc/{pid}/smaps")).unwrap();
    let mut relocated = 0u64;
    let mut of_exe = false;
    for line in smaps.lines() {
        if mapping(line) {
            of_exe = line
                .split_whitespace()
                .next_back()
                .is_some_and(|path| std::path::Path::new(path) == exe.as_path());
        } else if of_exe && let Some(rest) = line.strip_prefix("Private_Dirty:") {
            relocated += rest
                .trim()
                .trim_end_matches("kB")
                .trim()
                .parse::<u64>()
                .unwrap_or(0);
        }
    }
    (
        field("RssAnon:").saturating_sub(relocated),
        field("RssFile:"),
        field("Threads:"),
    )
}

/// [`holding`] once the process idles: read until two readings half a second apart move its own
/// memory by no more than [`IDLE_DRIFT_KB`]. A quiet machine reaches that in the first half
/// second; a loaded one finishes the process's startup later, and a fixed wait would read it
/// mid-startup.
fn idle_holding(pid: u32) -> (u64, u64, u64) {
    let deadline = tilo_test_env::hang_deadline();
    let mut last = holding(pid);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let now = holding(pid);
        if now.0.abs_diff(last.0) <= IDLE_DRIFT_KB {
            return now;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "tilo mcp never idled: it held {} kB of its own, then {} kB",
            last.0,
            now.0
        );
        last = now;
    }
}

/// `tilo mcp` answers an agent's first calls and then idles small: a bridge between the agent
/// and the person's workspace holds a few megabytes of its own — what it allocates, not the pages
/// its program's statics are relocated in — however many cores the computer has, rather than a
/// runtime's worth of threads and their heaps.
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
        .env_remove("TILO_IDENTITY_ROOT")
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
    let (own, program, threads) = idle_holding(mcp.id());
    drop(input);
    let _ = mcp.wait();
    assert!(
        own < MCP_IDLE_KB,
        "tilo mcp holds {own} kB of its own (and {program} kB of its program) with {threads} \
         threads, past {MCP_IDLE_KB} kB"
    );
}
