//! The `tilolang` command at its edge: what it prints when asked for help, which half of it a
//! command reaches for a package and for a Rust app, and how each of its commands refuses
//! arguments it cannot read, before it builds, runs or opens anything.

use std::path::Path;
use std::process::Command;

use tilo_test_env::SpawnUnderEnvLock as _;

/// `tilolang` given `args`, run to its end.
fn tilolang(args: &[&str]) -> std::process::Output {
    tilolang_in(Path::new(env!("CARGO_MANIFEST_DIR")), args)
}

/// `tilolang` given `args` in the working directory `dir`, run to its end.
fn tilolang_in(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tilolang"))
        .args(args)
        .current_dir(dir)
        .output_locked()
        .expect("the command runs")
}

/// A scratch directory holding a one-file package.
fn package_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a scratch directory");
    std::fs::write(
        dir.path().join("app.tl"),
        "app t.cmd \"Command\"\ntype C {\n    n: Int\n}\nroot C\ndefault view v for C\nview v(c: C) {\n    text(\"{c.n}\")\n}\n",
    )
    .expect("source");
    dir
}

tilo_test_env::scenario! {
    fn the_command_refuses_what_it_cannot_read_and_prints_its_usage;

    phase a_command_refuses_arguments_it_cannot_read_before_it_does_anything;
    // Each command reads all of its arguments before it does anything: one it cannot read ends the
    // run with what was wrong with it as the first line on stderr, exit code 1 and nothing on stdout.
    // Run in a package, `build`, `run` and `dev` are the package's.
    {
        let package = package_dir();
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
                &[
                    "dev",
                    "--session",
                    "session.json",
                    "--runtime",
                    "tilolang-process",
                ],
                "A session runs the runtime it baked",
            ),
            (
                &["dev", "--runtime", "tilolang-process", "--build-runtime"],
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
            let output = tilolang_in(package.path(), args);
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

    phase asked_for_help_or_given_nothing_the_command_lists_every_command;
    // Asked for help, or given nothing, the command lists every command of both halves on stdout
    // and succeeds: the ones a package and a Rust app share, a package's own and a Rust app's own.
    {
        let rows: [&[&str]; 4] = [&["--help"], &["-h"], &["help"], &[]];
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
                stdout.contains("usage: tilolang [--json] <COMMAND> [ARGS]..."),
                "{args:?} prints the usage: {stdout}"
            );
            for command in [
                "build",
                "package",
                "run",
                "dev",
                "check",
                "describe",
                "profile",
                "repl",
                "new",
                "add",
                "test",
                "surface",
                "keys",
                "publish",
            ] {
                assert!(
                    stdout.contains(&format!("\n  {command} ")),
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

    phase a_shared_command_asked_for_help_tells_both_kinds_of_app;
    // `build --help` (and `help build`) prints what a package's build takes, then a Rust app's.
    {
        for args in [&["build", "--help"][..], &["help", "build"]] {
            let output = tilolang(args);
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert_eq!(output.status.code(), Some(0), "{args:?}: {stdout}");
            let package = stdout
                .find("tilolang build|package [DIR]")
                .expect("the package's build");
            let rust = stdout.find("--target").expect("a Rust app's build");
            assert!(package < rust, "the package's first: {stdout}");
        }
    }

    phase a_shared_command_goes_by_the_directory_it_is_given;
    // `check` on a package is the compiler's; on a directory with no .tl files it is a Rust app's,
    // which answers --json with one document naming the command.
    {
        let package = package_dir();
        let checked = tilolang_in(package.path(), &["check", "--write-ids"]);
        assert!(
            checked.status.success(),
            "the package checks: {}",
            String::from_utf8_lossy(&checked.stderr)
        );
        let empty = tempfile::tempdir().expect("a scratch directory");
        let rust = tilolang_in(empty.path(), &["check", "--json"]);
        assert!(!rust.status.success(), "no Rust app there");
        let document: serde_json::Value =
            serde_json::from_slice(&rust.stdout).expect("one JSON document");
        assert_eq!(document["command"], "check", "{document}");
        assert_eq!(document["ok"], false, "{document}");
    }
}

/// The compiler's own commands are the tool's: `check`, `fmt` and `ids` reach the compiler's code
/// through `tilolang`, which is the one binary a person runs.
#[test]
fn the_compiler_commands_run_from_the_one_tool() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let dir = scratch.path().join("t");
    std::fs::create_dir(&dir).expect("package directory");
    let source = "app t.lock \"Lock\"\ntype C {\n    n: Int\n}\nroot C\ndefault view v for C\nview v(c: C) {\n  text(\"{c.n}\")\n}\n";
    let file = dir.join("app.tl");
    std::fs::write(&file, source).expect("source");
    let path = dir.to_string_lossy().into_owned();

    let help = tilolang(&["check", "--help"]);
    assert_eq!(help.status.code(), Some(0), "check --help succeeds");
    let usage = String::from_utf8_lossy(&help.stdout);
    assert!(
        usage.contains("tilolang check|fmt|ids"),
        "the compiler commands' usage printed: {usage}"
    );

    let check = tilolang(&["check", &path, "--write-ids"]);
    assert!(
        check.status.success(),
        "check writes the lock: {}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(dir.join("app.ids").is_file(), "the ids lock is written");

    let formatted = tilolang(&["fmt", &path]);
    assert!(
        formatted.status.success(),
        "fmt rewrites the package: {}",
        String::from_utf8_lossy(&formatted.stderr)
    );
    let canonical = std::fs::read_to_string(&file).expect("canonical source");
    assert_ne!(canonical, source, "the two-space body was rewritten");
    assert_eq!(
        tilolang(&["fmt", &path, "--check"]).status.code(),
        Some(0),
        "the canonical form passes --check"
    );

    std::fs::write(&file, source).expect("source");
    let stale = tilolang(&["fmt", &path, "--check"]);
    assert_eq!(
        stale.status.code(),
        Some(1),
        "a hand-wrapped file fails --check"
    );
    assert!(
        String::from_utf8_lossy(&stale.stderr).contains("not in canonical form"),
        "{}",
        String::from_utf8_lossy(&stale.stderr)
    );
    assert!(tilolang(&["fmt", &path]).status.success());
    assert_eq!(
        std::fs::read_to_string(&file).expect("canonical source"),
        canonical,
        "fmt writes the canonical text"
    );

    let lock = tilolang(&["ids", &path]);
    assert!(
        lock.status.success(),
        "ids prints the lock: {}",
        String::from_utf8_lossy(&lock.stderr)
    );
    let printed = String::from_utf8_lossy(&lock.stdout);
    assert!(printed.contains("C.n = "), "the lock printed: {printed}");

    let renamed = tilolang(&["ids", &path, "--rename", "C.n", "C.m"]);
    assert!(
        renamed.status.success(),
        "ids renames: {}",
        String::from_utf8_lossy(&renamed.stderr)
    );
    let written = std::fs::read_to_string(dir.join("app.ids")).expect("lock");
    assert!(
        written.contains("C.m = "),
        "the rename reached the lock: {written}"
    );
}
