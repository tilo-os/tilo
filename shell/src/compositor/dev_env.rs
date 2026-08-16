//! The single gate for developer-only environment overrides.
//!
//! Three environment variables let a running compositor be re-pointed at
//! something other than its production defaults:
//!
//! * `QOS_LOCK_TEST_PASSWORD` — replaces PAM with a fixed string, so *that*
//!   string unlocks the session.
//! * `QOS_UNLOCK_HELPER` — replaces the binary that decides whether an unlock
//!   attempt succeeded.
//! * `QOS_TEST_CONTROL` — binds a unix socket that injects synthetic input,
//!   reads back state, and drives the shell as if it were the user.
//!
//! Each of them is load-bearing for the test harnesses, and each of them is a
//! complete bypass of the lock screen. The compositor's environment is not a
//! trust boundary a user controls carefully: on a systemd desktop it is
//! seeded from `~/.config/environment.d/*.conf`, which is writable by
//! anything already running as the user. A shipped binary that honours these
//! variables therefore hands "unlock this machine" to any code that can drop
//! a file in the user's home — including while the screen is locked and the
//! owner is away from it.
//!
//! So the answer is not "default them off". It is that a shipped build must
//! not contain the code that reads them at all. [`DEV_ENV_OVERRIDES`] is
//! `cfg!(debug_assertions)`: true for `cargo build`/`cargo test`, where every
//! harness in this repo lives, and false for the `--release` binaries that
//! the OS image and the host RPM install. The release build does not consult
//! the variables, so setting them has no effect whatsoever — there is no flag
//! to flip back on, and nothing to misconfigure.
//!
//! Consumers must not re-derive the gate with their own `cfg` attribute; they
//! read this constant, and take the variable's value as a parameter, so the
//! disabled branch is compiled and unit-tested in both states rather than
//! only in whichever build the test suite happens to run under.

/// Whether this build honours the developer environment overrides described
/// in the module docs.
///
/// `true` in debug builds (the whole test suite, `cargo run`, the `shots`
/// harness, `scripts/headless-examples/`), `false` in release builds.
pub const DEV_ENV_OVERRIDES: bool = cfg!(debug_assertions);

/// Byte string stamped into a compositor built with `--features dev-session`.
///
/// The host session package's *developer* entry
/// (`/usr/libexec/qos-session-dev`, from `dist/`) will log you into a
/// compositor of your choosing instead of the installed one. That is a real
/// capability — "run this program as my login session" — so before it execs
/// anything it checks the target carries this marker. A build the developer
/// did not deliberately ask for cannot be logged into, and the entry is not a
/// generic run-anything-at-login primitive that some other program could
/// repurpose for persistence.
///
/// Deliberately searched for with a plain substring match over the file, so
/// the check needs no parsing and no execution of the untrusted binary.
pub const DEV_SESSION_MARKER: &str = "qos-dev-session-build-marker-v1";

/// The marker itself, present only when the non-default `dev-session`
/// feature is on **and** this is a debug build. Both conditions, so that
/// `--release --features dev-session` still produces a binary the developer
/// session entry refuses — a shipped artefact never carries it.
///
/// `#[used]` keeps the linker from discarding a static nothing reads.
#[cfg(all(feature = "dev-session", debug_assertions))]
#[used]
pub static DEV_SESSION_BUILD_MARKER: [u8; 31] = *b"qos-dev-session-build-marker-v1";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_track_the_build_profile() {
        // The test suite is a debug build, so the gate must be open here —
        // otherwise every harness that sets QOS_TEST_CONTROL or
        // QOS_LOCK_TEST_PASSWORD would silently stop working.
        //
        // Asserting `cfg!(debug_assertions)` rather than `true` keeps this
        // honest if someone ever runs `cargo test --release`: the constant
        // has to follow the profile, not a hard-coded expectation.
        assert_eq!(DEV_ENV_OVERRIDES, cfg!(debug_assertions));
    }

    #[test]
    fn the_marker_static_matches_the_string_the_packaging_greps_for() {
        // The dev session entry searches the binary for DEV_SESSION_MARKER.
        // If the static and the constant ever drift, the entry silently
        // refuses every build — so tie them together here.
        assert_eq!(DEV_SESSION_MARKER.len(), 31);
        #[cfg(all(feature = "dev-session", debug_assertions))]
        assert_eq!(
            DEV_SESSION_BUILD_MARKER.as_slice(),
            DEV_SESSION_MARKER.as_bytes()
        );
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn release_builds_close_the_gate() {
        // The property that actually matters, stated where it can fail.
        // Only compiled into a `--release` test run; in a debug run the
        // preceding test is the one with something to say.
        assert!(
            !DEV_ENV_OVERRIDES,
            "release build still honours dev env overrides"
        );
    }
}
