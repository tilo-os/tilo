//! What a sandboxed app can actually reach — proven by reading, or by
//! failing to.
//!
//! These tests start the real thing under `bwrap`, through the same
//! `run_with_captured` path the launcher uses, and assert on what a
//! syscall answered inside the sandbox. **They deliberately do not
//! assert on the argv** — a bind-list check can pass while the app
//! still opens the file; the argv is the launcher's intention, not the
//! boundary.
//!
//! Not `#[ignore]`d, unlike other live-bwrap tests here: bwrap is how a
//! qOS app runs at all, so a machine that can't run these can't run the
//! product, and a boundary whose proof is opt-in is a boundary nobody
//! checks.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use launcher::appdir;
use launcher::manifest::{Entrypoint, Fs, MinimalManifest, Net, Sandbox};
use launcher::run::{RunError, RunOptions, run_with_captured};

const PACKAGE: &str = "dev.qos.roomtest";

/// Build the fixture with `rustc` directly, outside the workspace, so
/// a single-shot build doesn't dirty the workspace target dir.
fn build_fixture(out_dir: &Path) -> PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/roomtest/src/main.rs");
    let bin = out_dir.join("bin").join("roomtest");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    let status = std::process::Command::new("rustc")
        .arg("--edition=2021")
        .arg("-O")
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .status()
        .expect("invoke rustc");
    assert!(
        status.success(),
        "rustc failed to build the roomtest fixture"
    );
    bin
}

fn manifest() -> MinimalManifest {
    MinimalManifest {
        package_id: PACKAGE.to_string(),
        name: None,
        category: None,
        icon: None,
        version: "0.0.0".to_string(),
        // Confinement is a property of every package the launcher runs, but
        // this fixture is the kind a user opens: a writable directory is
        // expected at all.
        kind: "app".to_string(),
        runtime: None,
        entrypoint: Entrypoint {
            bin: "bin/roomtest".to_string(),
            r#type: launcher::manifest::EntrypointType::Gui,
            gui_bin: None,
            cli_bin: None,
        },
        sandbox: Sandbox {
            net: Net {
                mode: "off".to_string(),
                ..Net::default()
            },
            fs: Fs::default(),
            services: BTreeMap::new(),
            portals: BTreeMap::new(),
            // Inherit the shipped syscall-filter default rather than naming one
            // here, or a confinement fixture pinning its own profile stops
            // tracking what an installed app actually gets.
            ..Sandbox::default()
        },
        cli_commands: Vec::new(),
        opens_ext: Vec::new(),
        opens_mime: Vec::new(),
        opens_schemes: Vec::new(),
    }
}

fn bwrap() -> PathBuf {
    let p = PathBuf::from("/usr/bin/bwrap");
    assert!(
        p.exists(),
        "bwrap is not installed. It is how a qOS app runs, so this is a \
         broken environment rather than a reason to skip the test."
    );
    p
}

/// Run the fixture in `room` with `argv`, returning its stdout.
fn run_in_room(payload: &Path, room: Option<&str>, argv: &[&str]) -> String {
    let opts = RunOptions {
        no_services: true,
        bwrap_bin: Some(bwrap()),
        landlock_enforce: false,
        workspace_id: room.map(str::to_string),
        mode: launcher::run::LaunchMode::Cli {
            command: "roomtest".to_string(),
            passthrough_argv: argv.iter().map(std::string::ToString::to_string).collect(),
        },
        ..RunOptions::default()
    };
    let (status, out) = run_with_captured(&manifest(), payload, &opts).expect("launch");
    let text = String::from_utf8_lossy(&out).trim().to_string();
    assert!(
        status.success(),
        "fixture exited {status:?} with stdout {text:?}"
    );
    text
}

/// `XDG_DATA_HOME` is process-global and the launcher reads it to
/// decide where the rooms tree lives, so these tests take turns.
struct DataHome {
    _dir: tempfile::TempDir,
    path: PathBuf,
    previous: Option<std::ffi::OsString>,
}

impl DataHome {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_path_buf();
        let previous = std::env::var_os("XDG_DATA_HOME");
        // SAFETY: every caller is in the `process_env` serial group.
        unsafe { std::env::set_var("XDG_DATA_HOME", &path) };
        Self {
            _dir: dir,
            path,
            previous,
        }
    }
}

impl Drop for DataHome {
    fn drop(&mut self) {
        // SAFETY: every caller is in the `process_env` serial group.
        unsafe {
            match self.previous.take() {
                Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }
    }
}

/// The claim in `plan/rooms.md` — "an app opened in a room starts
/// empty" — as observable behaviour rather than a design statement.
///
/// One app, two rooms: it writes a record in room A, and the same app
/// in room B cannot read it. Both halves matter — without the read
/// succeeding in room A, "not found in B" would be equally consistent
/// with a fixture that never wrote anything.
#[test]
#[serial_test::serial(process_env)]
fn an_app_cannot_read_across_rooms() {
    let data = DataHome::new();
    let payload = tempfile::tempdir().unwrap();
    build_fixture(payload.path());

    let wrote = run_in_room(
        payload.path(),
        Some("room-a"),
        &["write", "record.txt", "room-A-secret"],
    );
    assert!(
        wrote.starts_with("WROTE "),
        "the app must be able to write in its own directory: {wrote}"
    );

    // The bytes really landed, on the host, in room A's directory.
    let host_file = data
        .path
        .join("qos-rooms/room-a")
        .join(PACKAGE)
        .join("record.txt");
    assert_eq!(
        std::fs::read_to_string(&host_file).expect("record on the host"),
        "room-A-secret",
        "the write must have reached room A's directory at {}",
        host_file.display()
    );

    // Same app, same in-sandbox path, room B.
    let from_b = run_in_room(
        payload.path(),
        Some("room-b"),
        &["read", "/run/qos/app/record.txt"],
    );
    assert_eq!(
        from_b, "ERR /run/qos/app/record.txt ENOENT",
        "an app in room B must not find a record it wrote in room A"
    );

    // Nor by naming room A's host path — the sandbox has no such path.
    let by_host_path = run_in_room(
        payload.path(),
        Some("room-b"),
        &["read", &host_file.display().to_string()],
    );
    assert!(
        by_host_path.starts_with("ERR "),
        "naming the host path must not reach it either: {by_host_path}"
    );

    // The room-B directory is genuinely empty of the record, rather than
    // holding a copy the app happened not to open.
    let listing = run_in_room(payload.path(), Some("room-b"), &["readdir", "/run/qos/app"]);
    assert!(
        !listing.contains("record.txt"),
        "room B's directory must not contain room A's record: {listing}"
    );

    // Proof the fixture can read what it wrote: back in room A, it can.
    let from_a = run_in_room(
        payload.path(),
        Some("room-a"),
        &["read", "/run/qos/app/record.txt"],
    );
    assert_eq!(
        from_a, "OK /run/qos/app/record.txt room-A-secret",
        "room A must still see its own record — otherwise the failure in \
         room B proves nothing about rooms"
    );
}

/// The personal room is a room, not a bypass — an app launched into a
/// workspace must not reach it either.
#[test]
#[serial_test::serial(process_env)]
fn a_workspace_cannot_read_the_personal_room() {
    let _data = DataHome::new();
    let payload = tempfile::tempdir().unwrap();
    build_fixture(payload.path());

    run_in_room(payload.path(), None, &["write", "diary.txt", "personal"]);
    let from_workspace = run_in_room(
        payload.path(),
        Some("some-workspace"),
        &["read", "/run/qos/app/diary.txt"],
    );
    assert_eq!(
        from_workspace, "ERR /run/qos/app/diary.txt ENOENT",
        "the personal room must be as unreachable from a workspace as any \
         other room"
    );
}

/// Everything the app writes with no thought about where — `$HOME`,
/// `$XDG_DATA_HOME`, `$XDG_CONFIG_HOME` — has to land in the directory
/// it was given, or the confinement above is true of one path and
/// false of the paths apps actually use.
#[test]
#[serial_test::serial(process_env)]
fn home_and_the_xdg_variables_all_point_into_the_one_directory() {
    let data = DataHome::new();
    let payload = tempfile::tempdir().unwrap();
    build_fixture(payload.path());

    for var in [
        "HOME",
        "XDG_DATA_HOME",
        "XDG_CONFIG_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "QOS_APP_DIR",
    ] {
        let line = run_in_room(payload.path(), Some("room-a"), &["env", var]);
        let value = line.split_once('=').expect("ENV k=v").1;
        assert!(
            value.starts_with(appdir::SANDBOX_APP_DIR),
            "{var} is {value:?}, outside {}",
            appdir::SANDBOX_APP_DIR
        );
    }

    // `cli::standalone_state_dir` is `$XDG_DATA_HOME/<package id>`. An
    // app using it must end up in the room's directory on the host.
    let wrote = run_in_room(
        payload.path(),
        Some("room-a"),
        &[
            "write",
            &format!(".local/share/{PACKAGE}/state.bin"),
            "app-state",
        ],
    );
    assert!(wrote.starts_with("WROTE "), "{wrote}");
    let host = data
        .path
        .join("qos-rooms/room-a")
        .join(PACKAGE)
        .join(".local/share")
        .join(PACKAGE)
        .join("state.bin");
    assert_eq!(
        std::fs::read_to_string(&host).expect("state on the host"),
        "app-state",
        "standalone_state_dir must resolve inside the room at {}",
        host.display()
    );
}

/// The read profile is a ceiling, not a floor: a path outside it isn't
/// in the namespace at all, and a path inside it can't be written.
#[test]
#[serial_test::serial(process_env)]
fn the_read_profile_is_all_there_is_and_it_is_read_only() {
    let _data = DataHome::new();
    let payload = tempfile::tempdir().unwrap();
    build_fixture(payload.path());

    // Outside the profile: the user's own files, the shadow file, the
    // collaborative mount, the host's programs. `/home` and the invoking
    // user's home are named explicitly since the cross-room tests run with
    // `XDG_DATA_HOME` in a tempdir under `/tmp` (which the sandbox covers
    // with a tmpfs) — this closes the reading that would otherwise only
    // prove `/tmp` is a tmpfs.
    let real_home = std::env::var("HOME").expect("HOME is set for the test process");
    let outside_the_profile: Vec<String> = [
        "/etc/shadow".to_string(),
        "/etc/passwd".to_string(),
        "/var/workspace".to_string(),
        "/usr/bin/env".to_string(),
        "/root".to_string(),
        "/home".to_string(),
        real_home.clone(),
        format!("{real_home}/.ssh"),
        format!("{real_home}/.local/share"),
    ]
    .into();
    for outside in &outside_the_profile {
        let line = run_in_room(payload.path(), Some("room-a"), &["read", outside]);
        assert!(
            line.starts_with("ERR "),
            "{outside} must not be readable without a capability: {line}"
        );
    }

    // Inside the profile: readable.
    let read = run_in_room(
        payload.path(),
        Some("room-a"),
        &["readdir", "/usr/share/fonts"],
    );
    assert!(
        read.starts_with("OK "),
        "the profile must actually deliver fonts: {read}"
    );

    // And not writable: an app that could rewrite `/etc/ld.so.cache` would
    // choose what every later launch loads, and one that could drop a font
    // file could do it for every other app on the machine. Asserted by
    // attempting the write, not by reading the bind flag back out of an argv.
    for target in [
        "/usr/share/fonts/qos-confinement-probe",
        "/etc/ld.so.cache",
        "/usr/lib64/qos-confinement-probe.so",
    ] {
        let line = run_in_room(
            payload.path(),
            Some("room-a"),
            &["writeabs", target, "owned"],
        );
        assert!(
            line.starts_with("ERR "),
            "{target} must not be writable: {line}"
        );
        assert!(
            !std::path::Path::new(target)
                .metadata()
                .is_ok_and(|m| m.len() == 5),
            "{target} was actually modified on the host"
        );
    }

    // The app's own directory is writable, so the failures above are the
    // read-only mount, not a fixture that can't write at all.
    let ok = run_in_room(
        payload.path(),
        Some("room-a"),
        &["writeabs", "/run/qos/app/probe", "owned"],
    );
    assert!(
        ok.starts_with("WROTE "),
        "the app's own directory must be writable: {ok}"
    );
}

/// One app's directory is not another's: two packages in one room are
/// two directories, so a package can't read the state of the app whose
/// id it happens to know.
#[test]
#[serial_test::serial(process_env)]
fn one_app_cannot_read_another_apps_directory() {
    let data = DataHome::new();
    let payload = tempfile::tempdir().unwrap();
    build_fixture(payload.path());

    // Stand in for another package's secrets, in the same room.
    let victim = data
        .path
        .join("qos-rooms/room-a")
        .join("dev.qos.totp")
        .join("seeds.bin");
    std::fs::create_dir_all(victim.parent().unwrap()).unwrap();
    std::fs::write(&victim, b"totp-seed").unwrap();

    for attempt in [
        victim.display().to_string(),
        "/run/qos/app/../dev.qos.totp/seeds.bin".to_string(),
        "/run/qos/../qos/app/../dev.qos.totp/seeds.bin".to_string(),
    ] {
        let line = run_in_room(payload.path(), Some("room-a"), &["read", &attempt]);
        assert!(
            line.starts_with("ERR "),
            "another package's state must be unreachable via {attempt}: {line}"
        );
    }

    // The seed is still there — the reads failed, they did not delete it.
    assert_eq!(std::fs::read(&victim).unwrap(), b"totp-seed");
}

/// The launch-time half of the refusal, on the real `run` path: a
/// manifest naming a path is refused before bwrap is spawned, and
/// nothing it named is created.
#[test]
#[serial_test::serial(process_env)]
fn a_manifest_that_names_a_path_is_refused_at_launch() {
    let data = DataHome::new();
    let payload = tempfile::tempdir().unwrap();
    build_fixture(payload.path());

    let victim = data.path.join("dev.qos.totp");
    for (field, entry) in [
        ("write", "~"),
        ("write", "/"),
        ("write", "~/.ssh"),
        ("write", "~/../../etc"),
        ("write", &victim.display().to_string() as &str),
        ("read", "/etc/shadow"),
    ] {
        let mut m = manifest();
        match field {
            "write" => m.sandbox.fs.write.push(entry.to_string()),
            _ => m.sandbox.fs.read.push(entry.to_string()),
        }
        let opts = RunOptions {
            no_services: true,
            bwrap_bin: Some(bwrap()),
            ..RunOptions::default()
        };
        let err = run_with_captured(&m, payload.path(), &opts)
            .err()
            .unwrap_or_else(|| panic!("{field} = {entry:?} must be refused"));
        assert!(
            matches!(err, RunError::FsPolicy(_)),
            "{field} = {entry:?} produced {err} rather than a policy refusal"
        );
        let text = err.to_string();
        assert!(
            text.contains(PACKAGE) && text.contains(entry),
            "the refusal must name the package and the entry: {text}"
        );
    }
    assert!(
        !victim.exists(),
        "a refused launch must not have created the path it named"
    );
}
