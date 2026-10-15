//! `tilo-unlock-helper` — verify a live session user's password via PAM.
//!
//! The compositor's session-lock screen cannot verify a password itself: a
//! *running* session exposes no greetd socket, and the compositor deliberately
//! links no PAM (keeping its attack surface small). Instead it spawns THIS
//! helper — a tiny, single-purpose process — passes the username on argv and
//! the password on stdin, and reads the exit code:
//!
//! * `0` — the credential authenticated (unlock).
//! * `1` — PAM denied it (wrong password / bad account) — stay locked.
//! * `2` — the check could not run (PAM unavailable/misconfigured) — the
//!   caller treats this as fail-closed: stay locked.
//!
//! The password is read from stdin (never argv, so it never appears in
//! `/proc/*/cmdline` or `ps`). Nothing secret is ever printed. The PAM
//! conversation lives in [`pam`]; this file is the (unit-testable) CLI
//! contract around it.
//!
//! Configuration:
//! * argv`1` — username (falls back to `$USER` when absent/empty).
//! * `TILO_UNLOCK_PAM_SERVICE` — PAM service name (default `login`).
//! * `TILO_PAM_LIB` — libpam soname to dlopen (default `libpam.so.0`).

mod pam;

use std::io::{self, BufRead};

use pam::Outcome;

/// Default PAM service. On the Tilo image this is `os/etc/pam.d/tilo-unlock`
/// (auth + account via the maintained `password-auth`, plus fingerprint /
/// homed factors). If the service file is absent, `pam_start` fails and the
/// helper exits fail-closed — a missing stack never authenticates anyone.
/// Override with `TILO_UNLOCK_PAM_SERVICE` (e.g. `login` on a plain host).
const DEFAULT_SERVICE: &str = "tilo-unlock";
/// Default libpam soname resolved at runtime (see [`pam`]).
const DEFAULT_LIB: &str = "libpam.so.0";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let username = resolve_username(args.get(1).map(String::as_str), std::env::var("USER").ok());
    let service =
        std::env::var("TILO_UNLOCK_PAM_SERVICE").unwrap_or_else(|_| DEFAULT_SERVICE.to_string());
    let lib = std::env::var("TILO_PAM_LIB").unwrap_or_else(|_| DEFAULT_LIB.to_string());

    // Could not even read the credential — fail-closed.
    let Ok(password) = read_password(io::stdin().lock()) else {
        std::process::exit(outcome_to_exit(Outcome::Error));
    };

    let outcome = pam::authenticate(&lib, &service, &username, &password);
    // Do not leak which of user/service/password was at fault; the exit code
    // is the only channel, and the caller maps it to a single non-leaky line.
    std::process::exit(outcome_to_exit(outcome));
}

/// Resolve the username to authenticate: the explicit argv value when
/// non-empty, else `$USER`, else empty (PAM will simply deny an empty user).
fn resolve_username(arg: Option<&str>, env_user: Option<String>) -> String {
    match arg {
        Some(a) if !a.trim().is_empty() => a.to_string(),
        _ => env_user.unwrap_or_default(),
    }
}

/// Read the password as the first line of `reader`, stripping a trailing
/// `\n` and an optional preceding `\r`. Everything after the first newline is
/// ignored. A password legitimately contains no newline, so this is lossless.
fn read_password<R: BufRead>(mut reader: R) -> io::Result<String> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
    Ok(line)
}

/// Map a verify [`Outcome`] to the process exit code that is the helper's
/// entire contract with the compositor.
fn outcome_to_exit(outcome: Outcome) -> i32 {
    match outcome {
        Outcome::Ok => 0,
        Outcome::Denied => 1,
        Outcome::Error => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn read_password_takes_the_first_line_without_its_terminator() {
        let cases: [(&str, &[u8], &str); 6] = [
            ("strips trailing newline", b"hunter2\n", "hunter2"),
            ("strips crlf", b"hunter2\r\n", "hunter2"),
            ("without trailing newline", b"hunter2", "hunter2"),
            ("keeps only first line", b"first\nsecond\n", "first"),
            (
                "preserves internal spaces and symbols",
                b"a b c$%^&\n",
                "a b c$%^&",
            ),
            // Empty stdin is an empty password, not an error.
            ("empty stdin", b"", ""),
        ];
        for (name, input, want) in cases {
            let out = read_password(Cursor::new(input.to_vec())).unwrap();
            assert_eq!(out, want, "case {name}");
        }
    }

    #[test]
    fn resolve_username_prefers_argv_then_env_then_empty() {
        let cases = [
            ("prefers argv", Some("alice"), Some("bob"), "alice"),
            (
                "blank arg falls back to env",
                Some("   "),
                Some("bob"),
                "bob",
            ),
            ("missing arg falls back to env", None, Some("bob"), "bob"),
            ("empty when nothing available", None, None, ""),
        ];
        for (name, arg, env, want) in cases {
            assert_eq!(
                resolve_username(arg, env.map(str::to_string)),
                want,
                "case {name}"
            );
        }
    }

    #[test]
    fn exit_codes_are_stable_contract() {
        assert_eq!(outcome_to_exit(Outcome::Ok), 0);
        assert_eq!(outcome_to_exit(Outcome::Denied), 1);
        assert_eq!(outcome_to_exit(Outcome::Error), 2);
    }
}
