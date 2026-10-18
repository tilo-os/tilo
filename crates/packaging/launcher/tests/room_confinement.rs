//! Paths a sandboxed app can reach, checked by reading them or observing
//! the access failure.
//!
//! These tests start the app under `bwrap`, through the same
//! `run_with_captured` path the launcher uses, and assert on what a
//! syscall answered inside the sandbox. **They deliberately do not
//! assert on the argv** — a bind-list check can pass while the app
//! still opens the file; the argv is the launcher's intention, not the
//! boundary.
//!
//! Not `#[ignore]`d, unlike other live-bwrap tests here: bwrap is how a
//! Tilo app runs at all, so a machine that cannot run these cannot run the
//! Tilo app. They remain enabled so the boundary is checked in every run.

#![allow(
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use launcher::appdir;
use launcher::manifest::{Entrypoint, Fs, ManifestView, Net, Sandbox};
use launcher::run::{RunError, RunOptions, run_with_captured};

use crate::support::{isolated_options, roomtest_payload};

const PACKAGE: &str = "so.tilo.roomtest";

fn manifest() -> ManifestView {
    ManifestView {
        package_id: PACKAGE.to_string(),
        name: None,
        localized_names: BTreeMap::default(),
        keywords: Vec::new(),
        category: None,
        icon: None,
        version: "0.0.0".to_string(),
        // Confinement is a property of every package the launcher runs, but
        // this fixture is the kind a user opens: a writable directory is
        // expected at all.
        kind: "app".to_string(),
        runtime: None,
        optional_runtimes: Vec::new(),
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
        opens_secondary: false,
        continuity: launcher::manifest::ContinuityContract::default(),
        capabilities: Vec::new(),
    }
}

fn bwrap() -> PathBuf {
    let p = PathBuf::from("/usr/bin/bwrap");
    assert!(
        p.exists(),
        "bwrap is not installed. It is how a Tilo app runs, so this is a \
         broken environment rather than a reason to skip the test."
    );
    p
}

/// Run the fixture in `room` with `argv`, returning its stdout.
fn run_in_room(data: &DataHome, room: Option<&str>, argv: &[&str]) -> String {
    run_in_room_with_landlock(data, room, argv, false)
}

fn run_in_room_with_landlock(
    data: &DataHome,
    room: Option<&str>,
    argv: &[&str],
    landlock_enforce: bool,
) -> String {
    let opts = RunOptions {
        no_services: true,
        bwrap_bin: Some(bwrap()),
        landlock_enforce,
        sandbox_shim_bin: Some(PathBuf::from(env!("CARGO_BIN_EXE_tilo-launcher"))),
        workspace_id: room.map(str::to_string),
        mode: launcher::run::LaunchMode::Cli {
            command: "roomtest".to_string(),
            passthrough_argv: argv.iter().map(std::string::ToString::to_string).collect(),
        },
        ..isolated_options(&data.path)
    };
    let (status, out) = run_with_captured(&manifest(), roomtest_payload(), &opts).expect("launch");
    let text = String::from_utf8_lossy(&out).trim().to_string();
    assert!(
        status.success(),
        "fixture exited {status:?} with stdout {text:?}"
    );
    text
}

/// The host data home a test's launches keep their rooms tree in, handed
/// to the launcher rather than set in the environment, so these tests run
/// side by side.
struct DataHome {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl DataHome {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_path_buf();
        Self { _dir: dir, path }
    }
}

#[test]
fn rooms_and_apps_cannot_read_each_others_directories() {
    // Phase `an_app_cannot_read_across_rooms`.
    // The claim in `docs/architecture/rooms.md` — "an app opened in a room starts
    // empty" — as observable behaviour rather than a design statement.
    //
    // One app, two rooms: it writes a record in room A, and the same app
    // in room B cannot read it. Both halves matter — without the read
    // succeeding in room A, "not found in B" would be equally consistent
    // with a fixture that never wrote anything.
    {
        let data = DataHome::new();

        let wrote = run_in_room(
            &data,
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
            .join("tilo-rooms/room-a")
            .join(PACKAGE)
            .join("record.txt");
        assert_eq!(
            std::fs::read_to_string(&host_file).expect("record on the host"),
            "room-A-secret",
            "the write must have reached room A's directory at {}",
            host_file.display()
        );

        // Same app, same in-sandbox path, room B.
        let from_b = run_in_room(&data, Some("room-b"), &["read", "/run/tilo/app/record.txt"]);
        assert_eq!(
            from_b, "ERR /run/tilo/app/record.txt ENOENT",
            "an app in room B must not find a record it wrote in room A"
        );

        // Nor by naming room A's host path — the sandbox has no such path.
        let by_host_path = run_in_room(
            &data,
            Some("room-b"),
            &["read", &host_file.display().to_string()],
        );
        assert!(
            by_host_path.starts_with("ERR "),
            "naming the host path must not reach it either: {by_host_path}"
        );

        // The room-B directory is genuinely empty of the record, rather than
        // holding a copy the app happened not to open.
        let listing = run_in_room(&data, Some("room-b"), &["readdir", "/run/tilo/app"]);
        assert!(
            !listing.contains("record.txt"),
            "room B's directory must not contain room A's record: {listing}"
        );

        // Confirm that the fixture can read what it wrote back in room A.
        let from_a = run_in_room(&data, Some("room-a"), &["read", "/run/tilo/app/record.txt"]);
        assert_eq!(
            from_a, "OK /run/tilo/app/record.txt room-A-secret",
            "room A must still see its own record — otherwise the failure in \
             room B proves nothing about rooms"
        );
    }
    // Phase `a_workspace_cannot_read_the_personal_room`.
    // The personal room is a room, not a bypass — an app launched into a
    // workspace must not reach it either.
    {
        let data = DataHome::new();

        run_in_room(&data, None, &["write", "diary.txt", "personal"]);
        let from_workspace = run_in_room(
            &data,
            Some("some-workspace"),
            &["read", "/run/tilo/app/diary.txt"],
        );
        assert_eq!(
            from_workspace, "ERR /run/tilo/app/diary.txt ENOENT",
            "the personal room must be as unreachable from a workspace as any \
             other room"
        );
    }
    // Phase `one_app_cannot_read_another_apps_directory`.
    // One app's directory is not another's: two packages in one room are
    // two directories, so a package can't read the state of the app whose
    // id it happens to know.
    {
        let data = DataHome::new();

        // Stand in for another package's secrets, in the same room.
        let victim = data
            .path
            .join("tilo-rooms/room-a")
            .join("so.tilo.totp")
            .join("seeds.bin");
        std::fs::create_dir_all(victim.parent().unwrap()).unwrap();
        std::fs::write(&victim, b"totp-seed").unwrap();

        for attempt in [
            victim.display().to_string(),
            "/run/tilo/app/../so.tilo.totp/seeds.bin".to_string(),
            "/run/tilo/../tilo/app/../so.tilo.totp/seeds.bin".to_string(),
        ] {
            let line = run_in_room(&data, Some("room-a"), &["read", &attempt]);
            assert!(
                line.starts_with("ERR "),
                "another package's state must be unreachable via {attempt}: {line}"
            );
        }

        // The seed is still there — the reads failed, they did not delete it.
        assert_eq!(std::fs::read(&victim).unwrap(), b"totp-seed");
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "confinement phases assert production filesystem access in one test"
)]
fn the_sandbox_filesystem_is_the_app_directory_plus_the_read_only_profile() {
    // Phase `home_and_the_xdg_variables_all_point_into_the_one_directory`.
    // Everything the app writes with no thought about where — `$HOME`,
    // `$XDG_DATA_HOME`, `$XDG_CONFIG_HOME` — has to land in the directory
    // it was given, or the confinement above applies to one path and
    // not to the paths apps actually use.
    {
        let data = DataHome::new();

        for var in [
            "HOME",
            "XDG_DATA_HOME",
            "XDG_CONFIG_HOME",
            "XDG_STATE_HOME",
            "XDG_CACHE_HOME",
            "TILO_APP_DIR",
        ] {
            let line = run_in_room(&data, Some("room-a"), &["env", var]);
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
            &data,
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
            .join("tilo-rooms/room-a")
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
    // Phase `the_read_profile_is_all_there_is_and_it_is_read_only`.
    // The read profile is a ceiling, not a floor: a path outside it isn't
    // in the namespace at all, and a path inside it can't be written.
    {
        let data = DataHome::new();

        // Outside the profile: the user's own files, the shadow file, the
        // collaborative mount, the host's programs. `/home` and the invoking
        // user's home are named explicitly since these tests keep their data
        // home in a tempdir under `/tmp` (which the sandbox covers with a tmpfs) — this closes the reading that would otherwise only
        // prove `/tmp` is a tmpfs.
        let real_home = std::env::var("HOME").expect("HOME is set for the test process");
        let outside_the_profile: Vec<String> = [
            "/etc/shadow".to_string(),
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
            let line = run_in_room(&data, Some("room-a"), &["read", outside]);
            assert!(
                line.starts_with("ERR "),
                "{outside} must not be readable without a capability: {line}"
            );
        }

        // The account file is generated sandbox data, not a bind of the
        // host database. Require the complete single-record contents, then
        // attempt a real write and check both the sandbox and host bytes.
        let host_accounts = std::fs::read("/etc/passwd").expect("host account database");
        let (uid, gid) = (
            rustix::process::geteuid().as_raw(),
            rustix::process::getegid().as_raw(),
        );
        let expected_account = format!(
            "OK /etc/passwd app:x:{uid}:{gid}:Sandbox application:/run/tilo/app:/sbin/nologin"
        );
        let account = run_in_room(&data, Some("room-a"), &["read", "/etc/passwd"]);
        assert_eq!(
            account, expected_account,
            "only the synthetic account is visible"
        );
        let write = run_in_room(&data, Some("room-a"), &["writeabs", "/etc/passwd", "owned"]);
        assert!(
            write.starts_with("ERR "),
            "the synthetic account is read-only: {write}"
        );
        assert_eq!(
            run_in_room(&data, Some("room-a"), &["read", "/etc/passwd"]),
            expected_account,
            "the denied write must not change sandbox account bytes"
        );
        assert!(
            std::fs::read("/etc/passwd").expect("host account database after launch")
                == host_accounts,
            "sandbox account access must not modify the host database"
        );

        // Inside the profile: readable.
        let read = run_in_room(&data, Some("room-a"), &["readdir", "/usr/share/fonts"]);
        assert!(
            read.starts_with("OK "),
            "the profile must actually deliver fonts: {read}"
        );

        // And not writable: an app that could rewrite `/etc/ld.so.cache` would
        // choose what every later launch loads, and one that could drop a font
        // file could do it for every other app on the machine. Asserted by
        // attempting the write, not by reading the bind flag back out of an argv.
        for target in [
            "/usr/share/fonts/tilo-confinement-probe",
            "/etc/ld.so.cache",
            "/usr/lib64/tilo-confinement-probe.so",
        ] {
            let line = run_in_room(&data, Some("room-a"), &["writeabs", target, "owned"]);
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
            &data,
            Some("room-a"),
            &["writeabs", "/run/tilo/app/probe", "owned"],
        );
        assert!(
            ok.starts_with("WROTE "),
            "the app's own directory must be writable: {ok}"
        );
    }
    // Phase `a_manifest_that_names_a_path_is_refused_at_launch`.
    // The launch-time half of the refusal, on the `run` path: a
    // manifest naming a path is refused before bwrap is spawned, and
    // nothing it named is created.
    {
        let data = DataHome::new();

        let victim = data.path.join("so.tilo.totp");
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
                ..isolated_options(&data.path)
            };
            let err = run_with_captured(&m, roomtest_payload(), &opts)
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
}

#[test]
fn fedora_nss_trust_module_resolves_inside_the_app_sandbox() {
    // NSS loads this module dynamically through Fedora's system alternative,
    // so DT_NEEDED checks cannot observe a broken /etc/alternatives link.
    let library = "/usr/lib64/libnssckbi.so";
    let host = std::fs::read(library).expect("Fedora NSS trust module is required");
    assert_eq!(
        &host[..4],
        b"\x7fELF",
        "the host must supply an actual trust module"
    );
    let data = DataHome::new();
    let observed = run_in_room_with_landlock(&data, Some("room-a"), &["elf", library], true);
    assert_eq!(observed, format!("ELF {library} [127, 69, 76, 70]"));
    let unrelated =
        run_in_room_with_landlock(&data, Some("room-a"), &["read", "/etc/shadow"], true);
    assert!(
        unrelated.starts_with("ERR "),
        "TLS trust must not expose private host configuration: {unrelated}"
    );
}

/// Launch the fixture with `grants` written as the compositor's capability
/// file, Landlock enforced, and return its stdout.
fn run_with_grants(
    data: &DataHome,
    grants: &[(String, collab_types::caps::Access)],
    argv: &[&str],
) -> String {
    use collab_types::caps::{CapOverride, LaunchGrants, Scope, Subject};

    let caps_file = data.path.join("caps.json");
    let sandbox = grants
        .iter()
        .map(|(path, verb)| {
            CapOverride::grant(Subject::app(PACKAGE), Scope::FsPath(path.clone()), *verb)
        })
        .collect();
    let grants = LaunchGrants {
        authors: Vec::new(),
        sandbox,
    };
    std::fs::write(
        &caps_file,
        serde_json::to_vec(&grants).expect("encode grants"),
    )
    .expect("write capability file");
    let opts = RunOptions {
        no_services: true,
        bwrap_bin: Some(bwrap()),
        landlock_enforce: true,
        sandbox_shim_bin: Some(PathBuf::from(env!("CARGO_BIN_EXE_tilo-launcher"))),
        capabilities_path: Some(caps_file),
        mode: launcher::run::LaunchMode::Cli {
            command: "roomtest".to_string(),
            passthrough_argv: argv.iter().map(std::string::ToString::to_string).collect(),
        },
        ..isolated_options(&data.path)
    };
    let (status, out) = run_with_captured(&manifest(), roomtest_payload(), &opts).expect("launch");
    let text = String::from_utf8_lossy(&out).trim().to_string();
    assert!(
        status.success(),
        "fixture exited {status:?} with stdout {text:?}"
    );
    text
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one person's home, probed phase by phase through the production launch"
)]
fn a_home_grant_never_reaches_tilos_private_configuration() {
    use collab_types::caps::Access;

    // Built before the home is replaced: the fixture compiles with `rustc`
    // from the developer's toolchain, which lives under the real `HOME`.
    let _ = roomtest_payload();
    let home = tilo_test_env::TestHome::new().expect("temporary home");
    let store = home.config().join("tilo");
    let secret = store.join("identity/secrets/0123abcd.key");
    let vault_key = store.join("accounts/vault/vault.key");
    let attestors = store.join("trusted-attestors.toml");
    let revoked = store.join("revoked-keys.toml");
    for (path, contents) in [
        (&secret, "root-identity-secret"),
        (&vault_key, "vault-key-bytes"),
        (&attestors, "# trusted attestors"),
        (&revoked, "# revoked keys"),
    ] {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("store dir");
        std::fs::write(path, contents).expect("store file");
    }
    let note = home.home().join("Documents/note.txt");
    std::fs::create_dir_all(note.parent().expect("parent")).expect("documents");
    std::fs::write(&note, "my note").expect("note");
    let home_dir = home.home().display().to_string();
    let path = |p: &std::path::Path| p.display().to_string();

    // Phase `a_writable_home_grant_hides_and_protects_the_store`.
    {
        let data = DataHome::new();
        let grant = [(home_dir, Access::Write)];
        let run = |argv: &[&str]| run_with_grants(&data, &grant, argv);

        // The grant itself works: the person's files read and write.
        assert_eq!(
            run(&["read", &path(&note)]),
            format!("OK {} my note", path(&note))
        );
        let other_config = home.config().join("other-app.toml");
        assert!(
            run(&["writeabs", &path(&other_config), "x = 1"]).starts_with("WROTE "),
            "the rest of ~/.config stays writable under a writable home grant"
        );

        // Tilo's store is an empty directory: no secret, key or trust file.
        assert_eq!(
            run(&["readdir", &path(&store)]),
            format!("OK {}", path(&store)),
            "the private store must read as empty"
        );
        for private in [&secret, &vault_key, &attestors, &revoked] {
            let line = run(&["read", &path(private)]);
            assert!(
                line.starts_with("ERR "),
                "{} must be unreadable under a home grant: {line}",
                private.display()
            );
        }

        // Nor can the app plant a trust root in the store's place: writing it,
        // or moving the real store aside to write one where it was.
        for target in [&attestors, &revoked] {
            let line = run(&["writeabs", &path(target), "planted"]);
            assert!(line.starts_with("ERR "), "{line}");
        }
        let moved_config = home.home().join(".config-moved");
        for (from, to) in [
            (home.config().to_path_buf(), moved_config.clone()),
            (store.clone(), home.home().join("tilo-moved")),
        ] {
            let line = run(&["rename", &path(&from), &path(&to)]);
            assert!(
                line.starts_with("ERR "),
                "{} must not be movable from inside the sandbox: {line}",
                from.display()
            );
        }

        // On the host, every file is where it was, unchanged.
        assert_eq!(
            std::fs::read_to_string(&secret).expect("secret"),
            "root-identity-secret"
        );
        assert_eq!(
            std::fs::read_to_string(&attestors).expect("attestors"),
            "# trusted attestors"
        );
        assert_eq!(
            std::fs::read_to_string(&revoked).expect("revoked"),
            "# revoked keys"
        );
        assert!(!moved_config.exists());
        assert_eq!(
            std::fs::read_to_string(&other_config).expect("other config"),
            "x = 1",
            "the write beside the store reached the host"
        );
    }
    // Phase `a_read_only_config_grant_hides_the_store`.
    {
        let data = DataHome::new();
        let grant = [(path(home.config()), Access::Read)];
        let line = run_with_grants(&data, &grant, &["read", &path(&secret)]);
        assert!(line.starts_with("ERR "), "{line}");
        let listing = run_with_grants(&data, &grant, &["readdir", &path(home.config())]);
        assert!(
            listing.starts_with("OK ") && listing.contains("tilo"),
            "the grant still lists ~/.config: {listing}"
        );
    }
    // Phase `a_grant_naming_the_store_is_refused`.
    {
        let data = DataHome::new();
        for granted in [path(&store), path(&store.join("identity"))] {
            let grant = [(granted.clone(), Access::Write)];
            let line = run_with_grants(&data, &grant, &["read", &path(&secret)]);
            assert!(
                line.starts_with("ERR "),
                "a grant on {granted} must bind nothing: {line}"
            );
        }
    }
    // Phase `a_documents_grant_is_unchanged`.
    {
        let data = DataHome::new();
        let documents = path(note.parent().expect("documents"));
        let grant = [(documents, Access::Write)];
        assert_eq!(
            run_with_grants(&data, &grant, &["read", &path(&note)]),
            format!("OK {} my note", path(&note))
        );
        let line = run_with_grants(&data, &grant, &["read", &path(&secret)]);
        assert!(line.starts_with("ERR "), "{line}");
    }
    drop(home);
}

/// Run the fixture as Settings, the one package the launcher gives parts of
/// the session's configuration, with Landlock enforced as in production.
fn run_as_settings(data: &DataHome, argv: &[&str]) -> String {
    let manifest = ManifestView {
        package_id: "so.tilo.settings".to_string(),
        ..manifest()
    };
    let opts = RunOptions {
        no_services: true,
        bwrap_bin: Some(bwrap()),
        landlock_enforce: true,
        sandbox_shim_bin: Some(PathBuf::from(env!("CARGO_BIN_EXE_tilo-launcher"))),
        mode: launcher::run::LaunchMode::Cli {
            command: "roomtest".to_string(),
            passthrough_argv: argv.iter().map(std::string::ToString::to_string).collect(),
        },
        ..isolated_options(&data.path)
    };
    let (status, out) = run_with_captured(&manifest, roomtest_payload(), &opts).expect("launch");
    let text = String::from_utf8_lossy(&out).trim().to_string();
    assert!(
        status.success(),
        "fixture exited {status:?} with stdout {text:?}"
    );
    text
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one session configuration, probed phase by phase through the production launch"
)]
fn settings_reaches_only_its_own_parts_of_the_session_configuration() {
    // Built before the home is replaced: the fixture compiles with `rustc`
    // from the developer's toolchain, which lives under the real `HOME`.
    let _ = roomtest_payload();
    let home = tilo_test_env::TestHome::new().expect("temporary home");
    let host = home.config().join("tilo");
    let attestors = host.join("trusted-attestors.toml");
    let revoked = host.join("revoked-keys.toml");
    let registries = host.join("registries.toml");
    let record = host.join("records/person.json");
    let state = host.join("state.json");
    let settings = host.join("session/settings.json");
    let accounts = host.join("accounts/store.json");
    let root_secret = host.join("identity/secrets/0123abcd.key");
    let active = host.join("identity/active");
    let identity_record = host.join("identity/primary.json");
    for (path, contents) in [
        (&root_secret, "root secret"),
        (&active, "primary"),
        (&identity_record, r#"{"schema_version":2}"#),
        (&attestors, "# trusted attestors"),
        (&revoked, "# revoked keys"),
        (&registries, "# registries"),
        (&record, r#"{"person":true}"#),
        (&state, r#"{"workspaces":[]}"#),
        (&settings, r#"{"region":{"units":"metric"}}"#),
        (&accounts, r#"{"accounts":[]}"#),
    ] {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("config dir");
        std::fs::write(path, contents).expect("config file");
    }
    let config = appdir::SANDBOX_SESSION_CONFIG;
    let data = DataHome::new();
    let run = |argv: &[&str]| run_as_settings(&data, argv);

    // Phase `settings_is_told_where_its_document_is`.
    assert_eq!(
        run(&["env", "TILO_SESSION_SETTINGS"]),
        format!(
            "ENV TILO_SESSION_SETTINGS={}",
            appdir::SANDBOX_SESSION_SETTINGS
        )
    );

    // Phase `the_trust_lists_registries_and_records_are_not_there`.
    for private in [
        "trusted-attestors.toml",
        "revoked-keys.toml",
        "registries.toml",
        "records/person.json",
    ] {
        let line = run(&["read", &format!("{config}/{private}")]);
        assert!(
            line.starts_with("ERR "),
            "{private} must not reach the Settings sandbox: {line}"
        );
    }
    // A trust list written where the session keeps them lands in the app's
    // own directory, never on the host.
    let planted = run(&[
        "writeabs",
        &format!("{config}/trusted-attestors.toml"),
        "planted",
    ]);
    assert!(planted.starts_with("WROTE "), "{planted}");
    assert_eq!(
        std::fs::read_to_string(&attestors).expect("attestors"),
        "# trusted attestors"
    );

    // Phase `the_identity_store_is_not_there`: the root secret, the active
    // selector and the records are the host's, administered behind its
    // identity actions.
    for private in [
        "identity/secrets/0123abcd.key",
        "identity/active",
        "identity/primary.json",
    ] {
        let line = run(&["read", &format!("{config}/{private}")]);
        assert!(
            line.starts_with("ERR "),
            "{private} must not reach the Settings sandbox: {line}"
        );
    }
    let listing = run(&["readdir", &format!("{config}/identity")]);
    assert!(
        listing.starts_with("ERR "),
        "the identity store must not be listable from Settings: {listing}"
    );

    // Phase `the_workspace_tree_reads_and_refuses_writes`.
    assert_eq!(
        run(&["read", &format!("{config}/state.json")]),
        format!(r#"OK {config}/state.json {{"workspaces":[]}}"#)
    );
    let line = run(&["writeabs", &format!("{config}/state.json"), "{}"]);
    assert!(
        line.starts_with("ERR "),
        "state.json must be read-only: {line}"
    );
    assert_eq!(
        std::fs::read_to_string(&state).expect("state"),
        r#"{"workspaces":[]}"#
    );

    // Phase `the_settings_document_is_replaced_by_rename_and_the_host_sees_it`.
    let session = format!("{config}/session");
    assert_eq!(
        run(&["read", appdir::SANDBOX_SESSION_SETTINGS]),
        format!(
            r#"OK {} {{"region":{{"units":"metric"}}}}"#,
            appdir::SANDBOX_SESSION_SETTINGS
        )
    );
    let temp = format!("{session}/.settings.json.1.tmp");
    let wrote = run(&["writeabs", &temp, r#"{"region":{"units":"imperial"}}"#]);
    assert!(wrote.starts_with("WROTE "), "{wrote}");
    let renamed = run(&["rename", &temp, appdir::SANDBOX_SESSION_SETTINGS]);
    assert!(renamed.starts_with("RENAMED "), "{renamed}");
    assert_eq!(
        std::fs::read_to_string(&settings).expect("settings"),
        r#"{"region":{"units":"imperial"}}"#,
        "the compositor reads the document Settings replaced"
    );
    assert_eq!(
        run(&["readdir", &session]),
        format!("OK {session} settings.json"),
        "the session directory holds the document and nothing else of Tilo's"
    );

    // Phase `the_account_stores_stay_writable`.
    let wrote = run(&[
        "writeabs",
        &format!("{config}/accounts/store.json"),
        r#"{"accounts":[1]}"#,
    ]);
    assert!(wrote.starts_with("WROTE "), "{wrote}");
    assert_eq!(
        std::fs::read_to_string(&accounts).expect("accounts"),
        r#"{"accounts":[1]}"#
    );

    // On the host, nothing outside those parts changed.
    for (path, contents) in [
        (&revoked, "# revoked keys"),
        (&registries, "# registries"),
        (&record, r#"{"person":true}"#),
        (&root_secret, "root secret"),
        (&active, "primary"),
        (&identity_record, r#"{"schema_version":2}"#),
    ] {
        assert_eq!(std::fs::read_to_string(path).expect("unchanged"), contents);
    }
    drop(home);
}

/// An app's cgroup is out of its reach: its own place is the root of its
/// cgroup namespace, the hierarchy (every app's cgroup, named by app, with
/// its counters) is hidden behind an empty read-only directory, and no
/// other cgroupfs can be mounted to reach it. The other half of the
/// boundary, the kernel refusing moves out of that namespace and writes to
/// its own limits, is `app_spawner::a_process_in_an_app_cgroup_cannot_move_or_retune_itself`.
#[test]
fn an_app_can_neither_see_nor_reach_the_cgroup_hierarchy() {
    let data = DataHome::new();
    let run = |argv: &[&str]| run_in_room(&data, Some("room-a"), argv);
    assert_eq!(
        run(&["read", "/proc/self/cgroup"]),
        "OK /proc/self/cgroup 0::/",
        "the app sees its own cgroup as the root of its namespace"
    );
    assert_eq!(
        run(&["readdir", "/sys/fs/cgroup"]),
        "OK /sys/fs/cgroup",
        "no other app's cgroup, and no tree, is visible"
    );
    assert_eq!(
        run(&["writeabs", "/sys/fs/cgroup/cgroup.procs", "0"]),
        "ERR /sys/fs/cgroup/cgroup.procs EROFS"
    );
    assert_eq!(
        run(&["mkdir", "/sys/fs/cgroup/escape"]),
        "ERR /sys/fs/cgroup/escape EROFS"
    );
    // Neither a fresh cgroup2 mount nor a remount of the hidden one.
    assert_eq!(
        run(&["mount", "cgroup2", "/tmp/cgroup"]),
        "ERR /tmp/cgroup EPERM"
    );
    assert_eq!(
        run(&["mount", "cgroup2", "/sys/fs/cgroup"]),
        "ERR /sys/fs/cgroup EPERM"
    );
}
