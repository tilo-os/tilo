//! The single gate for developer-only environment overrides.
//!
//! Three environment variables let a running compositor be re-pointed at
//! something other than its production defaults: `TILO_LOCK_TEST_PASSWORD`
//! (replaces PAM with a fixed string), `TILO_UNLOCK_HELPER` (replaces the
//! binary that decides whether an unlock succeeded), and
//! `TILO_TEST_CONTROL` (a unix socket that injects input and drives the
//! shell as the user). Each is load-bearing for the test harnesses, and
//! each is a complete bypass of the lock screen — and the compositor's
//! environment is not a trust boundary a user controls carefully (on a
//! systemd desktop it is seeded from `~/.config/environment.d/*.conf`,
//! writable by anything already running as the user).
//!
//! So the answer is not "default them off", it's that a shipped build must
//! not contain the code that reads them at all. [`DEV_ENV_OVERRIDES`] is
//! true for `cargo build`/`cargo test` (`debug_assertions`) and for a build
//! that explicitly asks for the non-default `dev-overrides` cargo feature,
//! and false for the plain `--release` binaries the OS image and host RPM
//! install — setting the variables against those has no effect whatsoever.
//!
//! `dev-overrides` exists so dev VM images can run *release* binaries
//! (realistic performance) that a harness can still drive. It must never
//! ship: a `dev-overrides` binary accepts a fixed lock-screen password and
//! opens a socket that drives the session as the user. The repository
//! policy suite (`tools/policy/check_dev_overrides_contract.py`) fails if a
//! production image build asks for it.
//!
//! Consumers must not re-derive the gate with their own `cfg` attribute;
//! they read this constant and take the variable's value as a parameter,
//! so the disabled branch is compiled and unit-tested in both states.

/// Whether this build honours the developer environment overrides described
/// in the module docs.
///
/// `true` in debug builds (the whole test suite, `cargo run`, the `shots`
/// harness, `scripts/headless-examples/`) and in builds with the
/// `dev-overrides` feature; `false` in ordinary release builds.
///
/// This is the only place the gate is derived from build configuration.
pub const DEV_ENV_OVERRIDES: bool = cfg!(any(debug_assertions, feature = "dev-overrides"));

/// Whether this binary is a release build that nonetheless honours the
/// developer overrides — the configuration dev VM images ship and production
/// images must never contain. Used to make such a binary identify itself.
pub const DEV_OVERRIDES_RELEASE_BUILD: bool = DEV_ENV_OVERRIDES && !cfg!(debug_assertions);

/// The environment variables the compositor reads only when
/// [`DEV_ENV_OVERRIDES`] holds, and ignores entirely otherwise.
///
/// The app/runtime overlay variables the launcher forwarding also gates
/// (`TILO_APPS_SYSTEM_DIR` and friends) are not listed: the launcher child
/// inherits the compositor's environment, so calling them "ignored" would
/// be wrong.
///
/// Only the names are ever reported — never the values, since one of them
/// is a password.
pub const DEV_OVERRIDE_VARS: &[&str] = &[
    "TILO_TEST_CONTROL",
    "TILO_TEST_STATE_DUMP",
    "TILO_LOCK_TEST_PASSWORD",
    "TILO_UNLOCK_HELPER",
    "TILO_LAUNCHER_BIN",
    "TILO_TEST_FRAME_STALL_MS",
    "TILO_TEST_PRESENT_DEVICE_DELAY_MS",
    "TILO_APP_SPAWNER_TREE",
];

/// The developer variables that are set (non-empty) but will be ignored
/// because `dev_overrides` is off. Pure, so both build states are testable
/// from one test run.
pub fn ignored_override_vars(
    dev_overrides: bool,
    lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Vec<&'static str> {
    if dev_overrides {
        return Vec::new();
    }
    DEV_OVERRIDE_VARS
        .iter()
        .copied()
        .filter(|name| lookup(name).is_some_and(|value| !value.is_empty()))
        .collect()
}

/// The warning for [`ignored_override_vars`]: names the variables and how to
/// get a build that honours them. Contains no values.
pub fn ignored_override_message(names: &[&str]) -> Option<String> {
    if names.is_empty() {
        return None;
    }
    Some(format!(
        "ignoring {}: this build lacks the `dev-overrides` cargo feature, so developer \
         environment overrides have no effect. Use a debug build, rebuild with \
         `--features dev-overrides`, or run on a tilo-dev dev image.",
        names.join(", ")
    ))
}

/// Log, once per process, what this build does with the developer overrides.
///
/// A build that ignores them warns about any that are set anyway, so a
/// harness that sets `TILO_TEST_CONTROL` and waits for a socket learns why
/// it never appears. A release `dev-overrides` build announces itself as a
/// binary that must never ship.
///
/// Written to stderr rather than through `tracing`, so it is visible whatever
/// log filter the session runs with.
pub fn report_build_mode() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if DEV_OVERRIDES_RELEASE_BUILD {
            eprintln!(
                "tilo compositor: dev-overrides build: developer environment overrides \
                 (test-control socket, test lock password, unlock helper, launcher binary, \
                 app overlays) are honoured; this binary is for dev images only and must \
                 never ship"
            );
        }
        let ignored = ignored_override_vars(DEV_ENV_OVERRIDES, |name| std::env::var_os(name));
        if let Some(message) = ignored_override_message(&ignored) {
            eprintln!("tilo compositor: warning: {message}");
        }
    });
}

/// Byte string stamped into a compositor built with `--features dev-session`.
///
/// The host session package's *developer* entry
/// (`/usr/libexec/tilo-session-dev`, from `dist/`) will log you into a
/// compositor of your choosing instead of the installed one. That is a real
/// capability — "run this program as my login session" — so before it execs
/// anything it checks the target carries this marker. A build the developer
/// did not deliberately ask for cannot be logged into, and the entry is not a
/// generic run-anything-at-login primitive that some other program could
/// repurpose for persistence.
///
/// Deliberately searched for with a plain substring match over the file, so
/// the check needs no parsing and no execution of the untrusted binary.
pub const DEV_SESSION_MARKER: &str = "tilo-app-session-build-marker-v1";

/// The marker itself, present only when the non-default `dev-session`
/// feature is on **and** [`DEV_ENV_OVERRIDES`] holds (a debug build, or an
/// explicit `dev-overrides` build). Both conditions, so that
/// `--release --features dev-session` still produces a binary the developer
/// session entry refuses — a shipped artefact never carries it.
///
/// The `cfg` repeats the [`DEV_ENV_OVERRIDES`] predicate because a static
/// cannot be conditionally compiled on a constant.
///
/// `#[used]` keeps the linker from discarding a static nothing reads.
#[cfg(all(
    feature = "dev-session",
    any(debug_assertions, feature = "dev-overrides")
))]
#[used]
pub static DEV_SESSION_BUILD_MARKER: [u8; 32] = *b"tilo-app-session-build-marker-v1";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_override_gate_tracks_the_build_profile_and_reports_ignored_vars() {
        // Phase `overrides_track_the_build_profile`.
        // The test suite is a debug build, so the gate must be open here —
        // otherwise every harness that sets TILO_TEST_CONTROL or
        // TILO_LOCK_TEST_PASSWORD would silently stop working.
        //
        // Asserting against the cfg rather than `true` keeps this honest if
        // someone ever runs `cargo test --release`: the constant has to
        // follow the profile and the feature, not a hard-coded expectation.
        assert_eq!(
            DEV_ENV_OVERRIDES,
            cfg!(debug_assertions) || cfg!(feature = "dev-overrides")
        );
        assert_eq!(
            DEV_OVERRIDES_RELEASE_BUILD,
            !cfg!(debug_assertions) && cfg!(feature = "dev-overrides")
        );
        {
            let env = |name: &str| match name {
                "TILO_TEST_CONTROL" => Some("/run/user/1000/ctl.sock".into()),
                "TILO_LOCK_TEST_PASSWORD" => Some("hunter2".into()),
                "TILO_UNLOCK_HELPER" => Some("".into()),
                _ => None,
            };
            assert!(ignored_override_vars(true, env).is_empty());
            let ignored = ignored_override_vars(false, env);
            assert_eq!(ignored, ["TILO_TEST_CONTROL", "TILO_LOCK_TEST_PASSWORD"]);

            let message = ignored_override_message(&ignored).expect("a warning");
            assert!(message.contains("TILO_TEST_CONTROL"));
            assert!(message.contains("--features dev-overrides"));
            assert!(message.contains("tilo-dev"));
            // Names only: the password and the socket path never reach the log.
            assert!(!message.contains("hunter2"));
            assert!(!message.contains("ctl.sock"));
            assert_eq!(ignored_override_message(&[]), None);
        }
        // Phase `the_marker_static_matches_the_string_the_packaging_greps_for`.
        // The dev session entry searches the binary for DEV_SESSION_MARKER.
        // If the static and the constant ever drift, the entry silently
        // refuses every build — so tie them together here.
        assert_eq!(DEV_SESSION_MARKER.len(), 32);
        #[cfg(all(
            feature = "dev-session",
            any(debug_assertions, feature = "dev-overrides")
        ))]
        assert_eq!(
            DEV_SESSION_BUILD_MARKER.as_slice(),
            DEV_SESSION_MARKER.as_bytes()
        );
    }

    #[test]
    #[cfg(not(any(debug_assertions, feature = "dev-overrides")))]
    fn release_builds_close_the_gate() {
        // The property that actually matters, stated where it can fail.
        // Only compiled into a plain `--release` test run; in a debug run the
        // first test is the one with something to say.
        assert!(
            !DEV_ENV_OVERRIDES,
            "release build still honours dev env overrides"
        );
    }
}
