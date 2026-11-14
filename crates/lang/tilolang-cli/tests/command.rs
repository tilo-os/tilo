//! The `tilolang` command at its edge: what it prints when asked for help, and how each of its
//! commands refuses arguments it cannot read, before it builds, runs or opens anything.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

use std::process::Command;

use tilo_test_env::SpawnUnderEnvLock as _;

/// `tilolang` given `args`, run to its end.
fn tilolang(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tilolang"))
        .args(args)
        .output_locked()
        .expect("the command runs")
}

tilo_test_env::scenario! {
    fn the_command_refuses_what_it_cannot_read_and_prints_its_usage;

    phase a_command_refuses_arguments_it_cannot_read_before_it_does_anything;
    // Each command reads all of its arguments before it does anything: one it cannot read ends the
    // run with what was wrong with it as the first line on stderr, exit code 1 and nothing on stdout.
    {
        let rows: [(&[&str], &str); 12] = [
            (
                &["build", "--no-such-flag"],
                "unknown option --no-such-flag",
            ),
            (&["build", "--out"], "--out needs a value"),
            (
                &["describe", "--no-such-flag"],
                "unknown option --no-such-flag",
            ),
            (&["run", "--no-such-flag"], "unknown option --no-such-flag"),
            (&["dev", "--no-such-flag"], "unknown option --no-such-flag"),
            (
                &["dev", "--runtime-identity", "identity.json"],
                "--runtime-identity signs a session's runtime bundle: give --session too",
            ),
            (
                &["dev", "--runtime", "tilolang-app", "--build-runtime"],
                "--runtime names a binary built by hand, --build-runtime builds one: give one",
            ),
            (&["repl", "--no-such-flag"], "unknown option --no-such-flag"),
            (
                &["repl", "--color", "sometimes"],
                "--color is always, never or auto, not sometimes",
            ),
            (
                &["profile", "--no-such-flag"],
                "profile: no flag --no-such-flag",
            ),
            (
                &["profile", "--seed", "many"],
                "--seed many: not a number of items",
            ),
            (
                &["profile", "--size", "1280xtall"],
                "--size 1280xtall: \"tall\" is not a size",
            ),
        ];
        for (args, said) in rows {
            let output = tilolang(args);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(
                stderr.lines().next(),
                Some(format!("tilolang: {said}").as_str()),
                "{args:?} says what is wrong with it first: {stderr}"
            );
            assert_eq!(
                output.status.code(),
                Some(1),
                "{args:?} is refused: {stderr}"
            );
            assert!(
                output.stdout.is_empty(),
                "{args:?} does nothing: {}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
    }

    phase asked_for_help_or_given_nothing_the_command_prints_its_usage;
    // Asked for help, or given nothing, the command prints its usage on stdout and succeeds: every
    // command it has is named there.
    {
        let rows: [&[&str]; 3] = [&["--help"], &["-h"], &[]];
        for args in rows {
            let output = tilolang(args);
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert_eq!(
                output.status.code(),
                Some(0),
                "{args:?} succeeds: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                stdout.starts_with("usage:\n"),
                "{args:?} prints the usage: {stdout}"
            );
            for command in [
                "build",
                "run",
                "dev",
                "profile",
                "describe",
                "repl",
                "layout-check",
            ] {
                assert!(
                    stdout.contains(&format!("\n  tilolang {command} ")),
                    "{args:?} names `{command}`: {stdout}"
                );
            }
            assert!(
                output.stderr.is_empty(),
                "{args:?} is no error: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
