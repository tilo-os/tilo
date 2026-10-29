//! The shell end to end on a host with files in memory: pipes between
//! Tilo's own commands, the file commands as actions, what it refuses by
//! name, and the marks an interactive session writes for Terminal.

use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine as _;
use ush_shell::{Input, MemoryHost, Output, Session, Shell, Stdio};

struct Run {
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

fn run_on(host: &MemoryHost, script: &str) -> Run {
    let (mut shell, out, err) = shell_on(host, "");
    let status = shell.run_to_exit(script);
    drop(shell);
    Run {
        out: String::from_utf8_lossy(&out.take()).into_owned(),
        err: String::from_utf8_lossy(&err.take()).into_owned(),
        status,
    }
}

fn home() -> MemoryHost {
    let host = MemoryHost::new();
    host.dir("/home/me");
    host
}

fn run(script: &str) -> Run {
    run_on(&home(), script)
}

#[test]
fn tier_one_commands_pipe_into_each_other_in_memory() {
    let host = home();
    host.file("/home/me/notes/a.md", "one\ntwo\n")
        .file("/home/me/notes/b.md", "three\n")
        .file("/home/me/notes/c.txt", "four\n");
    let r = run_on(&host, "ls notes | wc -l");
    assert_eq!((r.out.trim(), r.status), ("3", 0), "{}", r.err);
    let r = run_on(&host, "cat notes/*.md | grep -c o");
    assert_eq!(r.out.trim(), "2", "{}", r.err);
    let r = run_on(&host, "printf '3\\n1\\n2\\n' | sort | head -n 1");
    assert_eq!(r.out, "1\n", "{}", r.err);
}

#[test]
fn a_pipe_stops_its_writer_when_the_reader_is_done() {
    // `yes` never ends on its own; `head` leaving must end it.
    let r = run("yes | head -n 3");
    assert_eq!(r.out, "y\ny\ny\n", "{}", r.err);
    assert_eq!(r.status, 0);
    let r = run("seq 1 200000 | tail -n 1");
    assert_eq!(r.out, "200000\n", "{}", r.err);
}

#[test]
fn file_commands_are_the_environments_actions() {
    let host = home();
    let r = run_on(
        &host,
        "mkdir -p src/new && touch src/new/x.rs && ls src/new",
    );
    assert_eq!((r.out.as_str(), r.status), ("x.rs\n", 0), "{}", r.err);
    assert_eq!(
        host.actions(),
        vec![
            vec!["mkdir".to_string(), "-p".into(), "src/new".into()],
            vec!["touch".to_string(), "src/new/x.rs".into()],
        ]
    );
}

#[test]
fn redirections_write_through_the_host() {
    let host = home();
    let r = run_on(&host, "echo hi > a.txt; echo there >> a.txt; cat < a.txt");
    assert_eq!(r.out, "hi\nthere\n", "{}", r.err);
    assert_eq!(host.contents("/home/me/a.txt").unwrap(), b"hi\nthere\n");
    let r = run_on(&host, "set -C; echo again > a.txt");
    assert_ne!(r.status, 0, "noclobber refuses to replace a file");
}

#[test]
fn a_command_that_is_not_tier_one_says_which_tier_it_needs() {
    let r = run("cargo build");
    assert_eq!(r.status, 127);
    assert!(r.err.contains("cargo"), "{}", r.err);
    assert!(
        r.err.contains("tier 2") && r.err.contains("tier 3"),
        "{}",
        r.err
    );
}

#[test]
fn unsupported_features_are_refused_by_name() {
    for (script, name) in [
        ("cat <(echo hi)", "process substitution"),
        ("coproc cat", "coproc"),
        ("time true", "time"),
        ("trap 'echo x' ERR", "ERR"),
        ("shopt -s extglob", "extglob"),
    ] {
        let r = run(&format!("{script}\necho went on"));
        assert_eq!(r.status, 2, "{script}: {}", r.err);
        assert!(
            r.err.contains(name) && r.err.contains("not supported"),
            "{script}: {}",
            r.err
        );
        assert!(
            !r.out.contains("went on"),
            "{script}: a script stops at what it cannot do"
        );
    }
}

#[test]
fn scripts_read_files_loop_and_capture() {
    let host = home();
    host.file("/home/me/list", "a 1\nb 2\nc 3\n");
    let script = r#"
total=0
while read -r name n; do
  total=$((total + n))
  names="$names$name"
done < list
echo "$names $total"
lines=$(wc -l < list)
echo "lines: $lines"
f() { local x=inner; echo "$x $1"; }
x=outer
f arg
echo "$x"
( x=sub; cd /; pwd )
echo "$x $(pwd)"
"#;
    let r = run_on(&host, script);
    assert_eq!(
        r.out, "abc 6\nlines: 3\ninner arg\nouter\n/\nouter /home/me\n",
        "{}",
        r.err
    );
}

#[test]
fn errexit_and_traps() {
    let r = run(
        "trap 'echo bye' EXIT; set -e; if false; then :; fi; false || true; false; echo not here",
    );
    assert_eq!(r.out, "bye\n");
    assert_eq!(r.status, 1);
}

#[test]
fn here_documents_feed_tier_one_commands() {
    let r = run("name=world\ncat <<EOF | tr a-z A-Z\nhello $name\nEOF\n");
    assert_eq!(r.out, "HELLO WORLD\n", "{}", r.err);
}

/// The marks an interactive session writes, as `docs/app-model/ush.md`
/// ("Command runs") describes them.
#[test]
fn an_interactive_session_marks_each_run_for_terminal() {
    let host = home();
    host.file("/home/me/a", "").file("/home/me/b", "");
    let (mut shell, out, _err) = shell_on(&host, "ls | wc -l\n\nexit\n");
    let key = "k3y0fth1ss3ss10n";
    let status = shell.interactive(&Session {
        runs_key: Some(key.into()),
    });
    drop(shell);
    assert_eq!(status, 0);
    let out = String::from_utf8_lossy(&out.take()).into_owned();
    let b64 = base64::engine::general_purpose::STANDARD;
    let started = format!(
        "\x1b]133;C;k={key};cmd={};cwd={};t=1\x07",
        b64.encode("ls | wc -l"),
        b64.encode("/home/me")
    );
    let ended = format!("\x1b]133;D;0;k={key};t=1\x07");
    let prompt = format!("\x1b]133;A;k={key}\x07");
    let at_start = out.find(&started).expect("the run's start is marked");
    let at_end = out.find(&ended).expect("the run's end is marked");
    let printed = out.get(at_start + started.len()..at_end).unwrap();
    assert_eq!(
        printed, "2\n",
        "what the run printed sits between its marks"
    );
    assert!(out.starts_with(&prompt), "{out:?}");
    // The empty line and `exit` run nothing Terminal records.
    assert_eq!(out.matches("133;C;").count(), 1, "{out:?}");
    assert!(!out.contains(";t=2"), "{out:?}");
}

#[test]
fn an_unfinished_line_asks_for_more() {
    assert!(ush_shell::incomplete("if true; then\n"));
    assert!(ush_shell::incomplete("echo 'open\n"));
    assert!(ush_shell::incomplete("cat <<EOF\nline\n"));
    assert!(!ush_shell::incomplete("echo done\n"));
    let host = home();
    let (mut shell, out, _) = shell_on(&host, "for i in 1 2\ndo echo $i\ndone\n");
    shell.interactive(&Session::default());
    drop(shell);
    assert_eq!(String::from_utf8_lossy(&out.take()), "1\n2\n");
}
