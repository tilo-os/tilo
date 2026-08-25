//! What a sandboxed app's environment actually contains.
//!
//! `plan/security.md` and `docs/app-model/sandbox.md` both describe the
//! app's environment as a closed set — a list of variables the launcher
//! puts there, beside the binds that make each of them mean something.
//! `bwrap` does not do that by default: it hands the child whatever the
//! parent was carrying, which here is the compositor's own environment.
//!
//! These tests start the real thing under `bwrap`, through the same
//! `run_with_captured` path the launcher uses, and read the answer out of
//! the process's own `environ` after `exec`. **They deliberately do not
//! assert on the argv.** An argv containing `--clearenv` is the launcher's
//! intention; a `--setenv` spliced ahead of it, or a caller that built its
//! argv another way, leaves the intention intact and the environment full.
//! The environment the process has is the boundary.
//!
//! Not `#[ignore]`d, for the reason `room_confinement.rs` gives: bwrap is
//! how a qOS app runs at all, so a machine that cannot run these cannot run
//! the product, and a boundary whose proof is opt-in is a boundary nobody
//! checks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use launcher::manifest::{Entrypoint, Fs, MinimalManifest, Net, Sandbox};
use launcher::run::{RunOptions, run_with_captured};

const PACKAGE: &str = "dev.qos.envtest";

/// A variable name no part of qOS sets, so its presence inside the sandbox
/// can only mean one thing: it walked in from the parent.
const MARKER: &str = "QOS_ENV_LEAK_MARKER";

/// Stand-ins for the compositor's own wiring. Real names, chosen because
/// each is something the session genuinely sets and an app genuinely must
/// not be handed: the privileged clipboard socket, the local collaboration
/// broker, and the user's identity file.
const SESSION_PRIVATE: &[(&str, &str)] = &[
    ("QOS_WAYLAND_PRIVILEGED", "wayland-priv-9"),
    ("QUICK_COLLAB_SOCK", "/run/user/1000/qos-collab.sock"),
    ("QOS_USER_IDENTITY", "/home/somebody/.config/qos/identity/primary.json"),
];

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
    assert!(status.success(), "rustc failed to build the fixture");
    bin
}

fn manifest() -> MinimalManifest {
    MinimalManifest {
        package_id: PACKAGE.to_string(),
        name: None,
        category: None,
        icon: None,
        version: "0.0.0".to_string(),
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
            ..Sandbox::default()
        },
        cli_commands: Vec::new(),
        opens_ext: Vec::new(),
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

/// Variables set on *this* process for the duration of one test, and put
/// back afterwards. What the launcher is carrying when it builds the launch
/// is the whole subject here, so the fixture has to be able to plant it.
struct ParentEnv {
    previous: Vec<(String, Option<std::ffi::OsString>)>,
}

impl ParentEnv {
    fn with(vars: &[(&str, &str)]) -> Self {
        let mut previous = Vec::new();
        for (k, v) in vars {
            previous.push(((*k).to_string(), std::env::var_os(k)));
            // SAFETY: every caller is in the `process_env` serial group, and
            // `Drop` restores what was there.
            unsafe { std::env::set_var(k, v) };
        }
        Self { previous }
    }
}

impl Drop for ParentEnv {
    fn drop(&mut self) {
        for (k, previous) in self.previous.drain(..) {
            // SAFETY: as above.
            unsafe {
                match previous {
                    Some(v) => std::env::set_var(&k, v),
                    None => std::env::remove_var(&k),
                }
            }
        }
    }
}

/// Run the fixture and return its stdout.
fn run(payload: &Path, argv: &[&str]) -> String {
    let opts = RunOptions {
        no_services: true,
        bwrap_bin: Some(bwrap()),
        landlock_enforce: false,
        mode: launcher::run::LaunchMode::Cli {
            command: "roomtest".to_string(),
            passthrough_argv: argv.iter().map(|s| s.to_string()).collect(),
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

/// Every variable name the sandboxed process has, from its own `environ`.
fn env_names_inside(payload: &Path) -> Vec<String> {
    let line = run(payload, &["envall"]);
    let names = line
        .strip_prefix("ENVALL ")
        .unwrap_or_else(|| panic!("fixture did not answer envall: {line:?}"));
    names
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// The launcher's environment does not become the app's.
///
/// A marker planted on this process, and three real session variables
/// beside it, must not be in the sandbox. Asserted against the process's
/// own `environ` after `exec` — the argv is not consulted.
#[test]
#[serial_test::serial(process_env)]
fn the_sessions_environment_does_not_reach_a_sandboxed_app() {
    let payload = tempfile::tempdir().unwrap();
    build_fixture(payload.path());

    let mut planted: Vec<(&str, &str)> = vec![(MARKER, "the-parent-was-here")];
    planted.extend_from_slice(SESSION_PRIVATE);
    let _env = ParentEnv::with(&planted);

    // Fixture sanity: without this the test would pass on a launcher that
    // never had the variable to leak in the first place.
    assert_eq!(
        std::env::var(MARKER).as_deref(),
        Ok("the-parent-was-here"),
        "the marker must be set on the launching process, or this test \
         cannot tell a cleared environment from an empty one"
    );

    let inside = env_names_inside(payload.path());

    assert!(
        !inside.iter().any(|k| k == MARKER),
        "{MARKER} was set on the launching process and reached the sandbox; \
         the app inherits the session's environment. Saw: {inside:?}"
    );
    for (key, _) in SESSION_PRIVATE {
        assert!(
            !inside.iter().any(|k| k == key),
            "{key} describes the session's own wiring and must not be in a \
             sandboxed app's environment. Saw: {inside:?}"
        );
    }
}

/// The app's environment is a closed set, not a filtered one.
///
/// Stronger than naming variables that must be absent: every name the
/// process has must be one the launcher put there on purpose. A variable
/// that arrives some other way fails this even though nobody thought to
/// name it — which is the point, since the leak this closes was exactly a
/// variable nobody had thought of.
#[test]
#[serial_test::serial(process_env)]
fn every_variable_in_the_sandbox_is_one_the_launcher_set() {
    let payload = tempfile::tempdir().unwrap();
    build_fixture(payload.path());

    let _env = ParentEnv::with(&[
        ("EDITOR", "vi"),
        ("SSH_AUTH_SOCK", "/run/user/1000/keyring/ssh"),
        ("XDG_SESSION_TYPE", "wayland"),
        (MARKER, "the-parent-was-here"),
    ]);

    // What the launcher announces it gives an app. `PATH` and the
    // locale/terminal allowlist come from `push_clean_environment`; the rest
    // are set beside the bind that makes each one reachable.
    let mut allowed: Vec<String> = vec![
        "PATH".to_string(),
        // `--clearenv` keeps `PWD`, which bwrap documents and which is a
        // path inside the sandbox rather than anything of the session's.
        "PWD".to_string(),
        "QOS_PACKAGE_ID".to_string(),
        "QOS_APP_NAME".to_string(),
        "QOS_APP_ICON".to_string(),
        "QOS_APP_DIR".to_string(),
        "QOS_USER_HOME".to_string(),
        "QOS_USER_IDENTITY".to_string(),
        "QOS_WORKSPACE_ID".to_string(),
        "QOS_PARTICIPATION".to_string(),
        "QOS_CLI_COMMAND".to_string(),
        // The egress gate, and the proxy variables that name it. Absent for
        // this fixture, which declares `net = "off"`, but part of the
        // documented set.
        "QOS_NET_GATE".to_string(),
        "HTTPS_PROXY".to_string(),
        "HTTP_PROXY".to_string(),
        "ALL_PROXY".to_string(),
        "https_proxy".to_string(),
        "http_proxy".to_string(),
        "all_proxy".to_string(),
        "NO_PROXY".to_string(),
        "no_proxy".to_string(),
        "HOME".to_string(),
        "XDG_DATA_HOME".to_string(),
        "XDG_STATE_HOME".to_string(),
        "XDG_CONFIG_HOME".to_string(),
        "XDG_CACHE_HOME".to_string(),
        "XDG_RUNTIME_DIR".to_string(),
        "WAYLAND_DISPLAY".to_string(),
        "DBUS_SESSION_BUS_ADDRESS".to_string(),
        "LD_LIBRARY_PATH".to_string(),
        "LD_PRELOAD".to_string(),
        "GLIBC_TUNABLES".to_string(),
    ];
    allowed.extend(launcher::sandbox::INHERITED_ENV.iter().map(|s| s.to_string()));

    let inside = env_names_inside(payload.path());
    assert!(
        !inside.is_empty(),
        "the fixture reported no environment at all, which would make every \
         other assertion here vacuous"
    );

    let unexpected: Vec<&String> = inside
        .iter()
        .filter(|k| {
            // Service-socket descriptors are named per service.
            !allowed.contains(k) && !k.starts_with("QOS_SVC_")
        })
        .collect();
    assert!(
        unexpected.is_empty(),
        "a sandboxed app's environment must hold only what the launcher put \
         there; these arrived some other way: {unexpected:?}"
    );
}

/// The user's language crosses the boundary, because it is the user's and
/// not the session's.
///
/// Clearing the environment without putting the locale back would leave
/// every app in the C locale — and would break the headless rigs under
/// `scripts/headless-examples/`, which read an app's own words back off the
/// screen and set `LANG` on the compositor to choose which words those are.
#[test]
#[serial_test::serial(process_env)]
fn the_users_locale_is_carried_across_explicitly() {
    let payload = tempfile::tempdir().unwrap();
    build_fixture(payload.path());

    let _env = ParentEnv::with(&[("LANG", "en_US.UTF-8"), ("LC_ALL", "en_US.UTF-8")]);

    let lang = run(payload.path(), &["env", "LANG"]);
    assert_eq!(
        lang, "ENV LANG=en_US.UTF-8",
        "the session's locale must reach the app; without it every app \
         answers in the C locale"
    );
}

/// `PATH` inside the sandbox is the sandbox's, not the session's.
///
/// The inherited one named host directories that are not in the namespace,
/// and could name a writable directory of the user's, which would decide
/// which binary the app's own `exec` resolves to.
#[test]
#[serial_test::serial(process_env)]
fn path_is_the_sandboxes_own() {
    let payload = tempfile::tempdir().unwrap();
    build_fixture(payload.path());

    let _env = ParentEnv::with(&[("PATH", "/home/somebody/bin:/usr/bin")]);

    let path = run(payload.path(), &["env", "PATH"]);
    assert_eq!(
        path,
        format!("ENV PATH={}", launcher::sandbox::SANDBOX_PATH),
        "a sandboxed app must not inherit the session's PATH"
    );
}

/// What the session asked its applications to say out loud crosses too.
///
/// An app's widgets are in no registry the compositor can walk, so the lines
/// an app writes about itself are the only account anything outside its
/// window has of what happened inside it — which row a press landed on,
/// where a panel was laid out, whether what was typed was sent. Without
/// `RUST_LOG` a sandboxed app falls back to errors only and every one of
/// those is dropped before it reaches the pipe the session reads, so a press
/// that hit and a press that missed come back identically silent.
#[test]
#[serial_test::serial(process_env)]
fn the_sessions_logging_preference_reaches_the_app() {
    let payload = tempfile::tempdir().unwrap();
    build_fixture(payload.path());

    let _env = ParentEnv::with(&[("RUST_LOG", "info,file_explorer::drag=debug")]);

    let seen = run(payload.path(), &["env", "RUST_LOG"]);
    assert_eq!(
        seen, "ENV RUST_LOG=info,file_explorer::drag=debug",
        "the session's log filter must reach the app; without it every app \
         inside the sandbox is silent below error level"
    );
}
