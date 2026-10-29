//! Commands that run commands, end to end through the shell on files in
//! memory: `find -exec`/`-ok`/`-delete`, `xargs`, `env`, `timeout`, `nice`
//! and `nohup` all hand their command back to the shell, so it resolves as
//! any utility does and its file actions belong to the same run.

use std::path::PathBuf;
use std::sync::Arc;

use ush_shell::{Host, Input, MemoryHost, Output, Shell, Stdio};

struct Ran {
    out: String,
    err: String,
    status: i32,
}

fn shell_on(
    host: &MemoryHost,
    stdin: &str,
) -> (Shell, ush_shell::io::Captured, ush_shell::io::Captured) {
    let (stdout, out) = Output::buffer();
    let (stderr, err) = Output::buffer();
    let io = Stdio {
        stdin: Input::bytes(stdin.as_bytes().to_vec()),
        stdout,
        stderr,
    };
    let env = [
        ("HOME".to_string(), "/home/me".to_string()),
        ("PATH".to_string(), "/bin".to_string()),
    ];
    let shell = Shell::new(Arc::new(host.clone()), env, PathBuf::from("/home/me"), io);
    (shell, out, err)
}

fn finish(
    shell: Shell,
    status: i32,
    out: &ush_shell::io::Captured,
    err: &ush_shell::io::Captured,
) -> Ran {
    drop(shell);
    Ran {
        out: String::from_utf8_lossy(&out.take()).into_owned(),
        err: String::from_utf8_lossy(&err.take()).into_owned(),
        status,
    }
}

fn run_with(host: &MemoryHost, stdin: &str, script: &str) -> Ran {
    let (mut shell, out, err) = shell_on(host, stdin);
    let status = shell.run_to_exit(script);
    finish(shell, status, &out, &err)
}

fn run_on(host: &MemoryHost, script: &str) -> Ran {
    run_with(host, "", script)
}

fn sources() -> MemoryHost {
    let host = MemoryHost::new();
    host.file("/home/me/src/a.rs", "foo\n")
        .file("/home/me/src/b.rs", "bar\n")
        .file("/home/me/src/c.rs", "foo bar\n")
        .file("/home/me/src/notes.md", "foo\n");
    host
}

// find -exec, -execdir, -ok, -delete

#[test]
fn find_exec_runs_a_command_per_name() {
    let host = sources();
    let r = run_on(&host, r"find src -name '*.rs' -exec grep -l foo {} \;");
    assert_eq!(
        (r.out.as_str(), r.status),
        ("src/a.rs\nsrc/c.rs\n", 0),
        "{}",
        r.err
    );
    // `{}` inside an argument is the name too.
    let r = run_on(&host, r"find src -name a.rs -exec echo x{}y \;");
    assert_eq!(r.out, "xsrc/a.rsy\n", "{}", r.err);
}

#[test]
fn find_exec_is_a_test_whose_truth_is_the_status() {
    let host = sources();
    let r = run_on(
        &host,
        r"find src -name '*.rs' -exec grep -q foo {} \; -print",
    );
    assert_eq!(
        (r.out.as_str(), r.status),
        ("src/a.rs\nsrc/c.rs\n", 0),
        "{}",
        r.err
    );
    let r = run_on(
        &host,
        r"find src -name '*.rs' ! -exec grep -q foo {} \; -print",
    );
    assert_eq!(r.out, "src/b.rs\n", "{}", r.err);
    // A failing `;` command does not change find's status.
    let r = run_on(&host, r"find src -exec false \;");
    assert_eq!(r.status, 0, "{}", r.err);
}

#[test]
fn find_exec_plus_runs_grep_once_with_every_name() {
    let host = sources();
    // grep names the file only when it was given more than one: one run.
    let r = run_on(&host, "find src -name '*.rs' -exec grep foo {} +");
    assert_eq!(
        (r.out.as_str(), r.status),
        ("src/a.rs:foo\nsrc/c.rs:foo bar\n", 0),
        "{}",
        r.err
    );
    let r = run_on(&host, "find src -name '*.rs' -exec grep -l foo {} +");
    assert_eq!(r.out, "src/a.rs\nsrc/c.rs\n", "{}", r.err);
    let r = run_on(&host, "find src -name '*.rs' -exec echo {} +");
    assert_eq!(r.out, "src/a.rs src/b.rs src/c.rs\n", "{}", r.err);
    // A batch that fails makes find's status 1, as GNU's.
    let r = run_on(&host, "find src -name '*.rs' -exec grep -q nothing {} +");
    assert_eq!(r.status, 1, "{}", r.err);
    // `+` that does not follow `{}` is an argument.
    let r = run_on(&host, r"find src -name a.rs -exec echo + {} \;");
    assert_eq!(r.out, "+ src/a.rs\n", "{}", r.err);
    let r = run_on(&host, "find src -exec echo {} {} +");
    assert_eq!(r.status, 2);
    assert!(r.err.contains("only one instance of {}"), "{}", r.err);
}

#[test]
fn find_exec_without_its_terminator_is_refused() {
    let r = run_on(&sources(), "find src -exec echo {}");
    assert_eq!(r.status, 2);
    assert_eq!(r.err, "find: missing argument to `-exec'\n");
}

#[test]
fn find_execdir_runs_in_the_names_directory() {
    let host = sources();
    let r = run_on(&host, r"find src -name a.rs -execdir pwd \;");
    assert_eq!(r.out, "/home/me/src\n", "{}", r.err);
    let r = run_on(&host, r"find src -name a.rs -execdir echo {} \;");
    assert_eq!(r.out, "./a.rs\n", "{}", r.err);
    let r = run_on(&host, "find src -name '*.rs' -execdir echo {} +");
    assert_eq!(r.out, "./a.rs ./b.rs ./c.rs\n", "{}", r.err);
}

#[test]
fn find_ok_asks_and_runs_on_yes() {
    let host = sources();
    let r = run_with(&host, "y\nn\n", r"find src -name '*.rs' -ok echo hit {} \;");
    assert_eq!(r.out, "hit src/a.rs\n", "{}", r.err);
    assert_eq!(
        r.err,
        "< echo ... src/a.rs > ? < echo ... src/b.rs > ? < echo ... src/c.rs > ? "
    );
    let r = run_with(&host, "y\n", r"find src -name b.rs -okdir echo {} \;");
    assert_eq!(r.out, "./b.rs\n", "{}", r.err);
}

#[test]
fn find_delete_asks_for_the_environments_remove_depth_first() {
    let host = MemoryHost::new();
    host.file("/home/me/tmp/a.log", "x")
        .file("/home/me/tmp/keep.txt", "x")
        .file("/home/me/tmp/sub/b.log", "x");
    let r = run_on(&host, "find tmp -name '*.log' -delete");
    assert_eq!((r.out.as_str(), r.status), ("", 0), "{}", r.err);
    assert_eq!(
        host.actions(),
        vec![
            vec!["rm".to_string(), "tmp/a.log".to_string()],
            vec!["rm".to_string(), "tmp/sub/b.log".to_string()],
        ]
    );
    assert!(!host.exists("/home/me/tmp/a.log"));
    assert!(host.exists("/home/me/tmp/keep.txt"));
    let r = run_on(&host, "find tmp/sub -delete");
    assert_eq!(r.status, 0, "{}", r.err);
    let last = host.actions().last().cloned();
    assert_eq!(last, Some(vec!["rmdir".to_string(), "tmp/sub".to_string()]));
    assert!(!host.exists("/home/me/tmp/sub"));
}

#[test]
fn a_dispatched_file_action_carries_the_run() {
    let host = sources();
    let (mut shell, out, err) = shell_on(&host, "");
    shell.set_run(Some(ush_shell::Run {
        id: "run-7".into(),
        label: "cleanup".into(),
    }));
    let status = shell.run_to_exit(
        "find src -name '*.md' -delete; echo new.txt | xargs touch; env X=1 touch other.txt; \
         find src -name a.rs -exec rm {} \\;",
    );
    let r = finish(shell, status, &out, &err);
    assert_eq!(r.status, 0, "{}", r.err);
    assert_eq!(host.actions().len(), 4, "{:?}", host.actions());
    assert_eq!(host.action_runs(), vec!["run-7"; 4]);
    assert!(host.exists("/home/me/new.txt") && host.exists("/home/me/other.txt"));
}

// xargs

#[test]
fn xargs_builds_command_lines_from_its_input() {
    let host = sources();
    let r = run_on(&host, "printf 'a b\\nc\\n' | xargs");
    assert_eq!(r.out, "a b c\n", "{}", r.err);
    let r = run_on(&host, "printf 'a b\\nc\\n' | xargs echo x");
    assert_eq!(r.out, "x a b c\n", "{}", r.err);
    let r = run_on(&host, "printf 'a b\\nc\\n' | xargs -n 2 echo");
    assert_eq!(r.out, "a b\nc\n", "{}", r.err);
    let r = run_on(&host, "printf 'a b\\nc d e\\n' | xargs -L 1 echo");
    assert_eq!(r.out, "a b\nc d e\n", "{}", r.err);
    // Quotes and backslashes, as GNU reads them.
    let r = run_on(
        &host,
        r#"printf '%s\n' "'a b' \"c d\" e\\ f" | xargs -n 1 echo"#,
    );
    assert_eq!(r.out, "a b\nc d\ne f\n", "{}", r.err);
    let r = run_on(&host, "printf 'a b\\0c\\0' | xargs -0 -n 1 echo");
    assert_eq!(r.out, "a b\nc\n", "{}", r.err);
    let r = run_on(&host, "printf 'a b,c,' | xargs -d , -n 1 echo");
    assert_eq!(r.out, "a b\nc\n", "{}", r.err);
    let r = run_on(
        &host,
        "printf 'one\\n  two three\\n' | xargs -I {} echo [{}] {}",
    );
    assert_eq!(r.out, "[one] one\n[two three] two three\n", "{}", r.err);
    let r = run_on(&host, "printf 'a\\nb\\n' | xargs -t -n 1 echo");
    assert_eq!(
        (r.out.as_str(), r.err.as_str()),
        ("a\nb\n", "echo a\necho b\n")
    );
}

#[test]
fn xargs_runs_once_on_empty_input_unless_told_not_to() {
    let host = sources();
    let r = run_on(&host, "printf '' | xargs echo");
    assert_eq!((r.out.as_str(), r.status), ("\n", 0));
    let r = run_on(&host, "printf '' | xargs -r echo");
    assert_eq!((r.out.as_str(), r.status), ("", 0));
    let r = run_on(&host, "printf '' | xargs --no-run-if-empty echo");
    assert_eq!(r.out, "");
    let r = run_on(&host, "printf '' | xargs -I {} echo {}");
    assert_eq!(r.out, "");
}

#[test]
fn xargs_exit_statuses_are_gnus() {
    let host = sources();
    let r = run_on(&host, "printf 'a\\nb\\n' | xargs -n 1 false");
    assert_eq!(r.status, 123, "{}", r.err);
    let r = run_on(&host, "printf 'src/a.rs src/b.rs\\n' | xargs grep -q foo");
    assert_eq!(r.status, 0, "{}", r.err);
    let r = run_on(&host, "printf 'src/b.rs\\n' | xargs grep -q foo");
    assert_eq!(r.status, 123, "{}", r.err);
    let r = run_on(&host, "printf 'a\\nb\\n' | xargs -n 1 nosuchcommand");
    assert_eq!(r.status, 127, "{}", r.err);
    // It stops at the first command it cannot find.
    assert_eq!(r.err.matches("nosuchcommand").count(), 1, "{}", r.err);
    let r = run_on(&host, "printf \"'a\\n\" | xargs echo");
    assert_eq!(r.status, 1);
    assert!(r.err.contains("unmatched single quote"), "{}", r.err);
}

#[test]
fn xargs_in_parallel_gives_the_same_lines() {
    let host = sources();
    let serial = run_on(&host, "seq 1 200 | xargs -n 3 -P 1 echo | sort");
    let parallel = run_on(&host, "seq 1 200 | xargs -n 3 -P 4 echo | sort");
    assert_eq!(serial.status, 0, "{}", serial.err);
    assert_eq!(parallel.status, 0, "{}", parallel.err);
    assert_eq!(serial.out.lines().count(), 67);
    assert_eq!(serial.out, parallel.out);
    // Every line is one command's whole output.
    for line in parallel.out.lines() {
        let n = line.split(' ').count();
        assert!(n == 3 || n == 2, "{line}");
    }
}

#[test]
fn a_pipeline_mixes_dispatching_commands_and_built_ins() {
    let host = MemoryHost::new();
    host.file("/home/me/notes/a.md", "x\nx\n")
        .file("/home/me/notes/b.md", "x\n")
        .file("/home/me/notes/c.md", "y\n")
        .file("/home/me/notes/d.txt", "x\n");
    let r = run_on(
        &host,
        "find . -name '*.md' | xargs grep -c x | sort | head -n 2",
    );
    assert_eq!(
        (r.out.as_str(), r.status),
        ("./notes/a.md:2\n./notes/b.md:1\n", 0),
        "{}",
        r.err
    );
    let r = run_on(
        &host,
        "for f in $(find notes -name '*.md' -exec grep -l x {} +); do echo \"$f\"; done | wc -l",
    );
    assert_eq!(r.out.trim(), "2", "{}", r.err);
}

// env, timeout, nice, nohup

#[test]
fn env_runs_its_command_in_the_changed_environment() {
    let host = sources();
    let r = run_on(&host, "env FOO=bar printenv FOO");
    assert_eq!((r.out.as_str(), r.status), ("bar\n", 0), "{}", r.err);
    let r = run_on(&host, "env -u HOME printenv HOME");
    assert_eq!((r.out.as_str(), r.status), ("", 1), "{}", r.err);
    let r = run_on(&host, "env -i A=b printenv");
    assert_eq!(r.out, "A=b\n", "{}", r.err);
    // The shell's own variables stay as they were.
    let r = run_on(
        &host,
        "env FOO=bar true; echo \"${FOO-unset}\"; printenv HOME",
    );
    assert_eq!(r.out, "unset\n/home/me\n", "{}", r.err);
    // A variable the shell does not export is not in the environment.
    let r = run_on(&host, "Y=2; env printenv Y; echo $?");
    assert_eq!(r.out, "1\n", "{}", r.err);
    let r = run_on(&host, "env -C src ls");
    assert_eq!(r.out, "a.rs\nb.rs\nc.rs\nnotes.md\n", "{}", r.err);
    let r = run_on(&host, "env nosuchcommand");
    assert_eq!(r.status, 127, "{}", r.err);
    // With no command it still prints the environment.
    let r = run_on(&host, "env -i A=1 B=2");
    assert_eq!(r.out, "A=1\nB=2\n", "{}", r.err);
}

#[test]
fn timeout_stops_a_command_when_its_time_is_up() {
    let host = sources();
    let start = host.now_ms();
    let r = run_on(&host, "timeout 1 sleep 5");
    assert_eq!(r.status, 124, "{}", r.err);
    let waited = host.now_ms().saturating_sub(start);
    assert!(
        waited <= 1000,
        "stopped at its deadline, not after: {waited} ms"
    );
    let r = run_on(&host, "timeout 5 sleep 1; echo $?");
    assert_eq!(r.out, "0\n", "{}", r.err);
    let r = run_on(&host, "timeout --preserve-status 1 sleep 5");
    assert_eq!(r.status, 143, "{}", r.err);
    let r = run_on(&host, "timeout -s KILL -k 1 --preserve-status 0.5 sleep 5");
    assert_eq!(r.status, 137, "{}", r.err);
    let r = run_on(&host, "timeout -v 1 sleep 5");
    assert_eq!(r.err, "timeout: sending signal TERM to command 'sleep'\n");
    let r = run_on(&host, "timeout 1 grep -c foo src/a.rs");
    assert_eq!((r.out.as_str(), r.status), ("1\n", 0), "{}", r.err);
    let r = run_on(&host, "timeout -s BOGUS 1 true");
    assert_eq!(r.status, 125, "{}", r.err);
    let r = run_on(&host, "timeout 1 nosuchcommand");
    assert_eq!(r.status, 127, "{}", r.err);
}

#[test]
fn nice_and_nohup_run_their_command() {
    let host = sources();
    let r = run_on(&host, "nice -n 5 echo hi; nice; nice -10 echo there");
    assert_eq!(r.out, "hi\n0\nthere\n", "{}", r.err);
    let r = run_on(&host, "nohup echo hi");
    assert_eq!((r.out.as_str(), r.err.as_str(), r.status), ("hi\n", "", 0));
    assert!(!host.exists("/home/me/nohup.out"));
    let r = run_on(&host, "nohup nosuchcommand");
    assert_eq!(r.status, 127, "{}", r.err);
}
