//! The tier-1 commands, run over files in memory, against what GNU
//! coreutils, grep, diff and `tree` print for the same input (in the C
//! locale, with times in UTC unless a test says otherwise).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ush_shell::MemoryHost;
use ush_shell::commands::{Ctx, lookup};
use ush_shell::host::{DirEntry, Host, Metadata, Program, WriteMode};
use ush_shell::io::{Input, Output, Std, Stdio, pipe};

struct Ran {
    status: i32,
    out: String,
    err: String,
}

/// Run `name args` in `/w` with `stdin`, its standard output captured.
fn run_on(
    host: &dyn Host,
    env: &[(String, String)],
    stdin: &[u8],
    name: &str,
    args: &[&str],
) -> Ran {
    let (stdout, out) = Output::buffer();
    let (stderr, err) = Output::buffer();
    let io = Stdio {
        stdin: Input::bytes(stdin.to_vec()),
        stdout,
        stderr,
    };
    let ctx = Ctx {
        host,
        cwd: Path::new("/w"),
        io: &io,
        env,
        name,
        dispatch: &ush_shell::commands::NoDispatch,
        render: ush_shell::commands::Render::Text,
        run: None,
    };
    let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
    let command = lookup(name).unwrap();
    let status = command(&ctx, &args);
    Ran {
        status,
        out: String::from_utf8_lossy(&out.take()).into_owned(),
        err: String::from_utf8_lossy(&err.take()).into_owned(),
    }
}

fn run(host: &MemoryHost, name: &str, args: &[&str]) -> Ran {
    run_on(host, &[], b"", name, args)
}

fn run_in(stdin: &str, name: &str, args: &[&str]) -> Ran {
    run_on(&MemoryHost::new(), &[], stdin.as_bytes(), name, args)
}

/// Standard output, after checking the command succeeded and said nothing
/// on standard error.
fn ok(r: Ran) -> String {
    assert_eq!((r.status, r.err.as_str()), (0, ""), "output: {}", r.out);
    r.out
}

const GREEK: &str =
    "alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\neta\ntheta\niota\nkappa\nlambda\nmu\n";
const A_TXT: &str = "one\ntwo\n\n\n\tthree\nfour\n";

fn files() -> MemoryHost {
    let host = MemoryHost::new();
    host.file("/w/a.txt", A_TXT);
    host.file("/w/greek.txt", GREEK);
    host
}

/// A [`MemoryHost`] that knows user and group names, an hour east of UTC,
/// and a clock near the files' times, for `ls -l` and `stat`.
struct Named(MemoryHost);

impl Host for Named {
    fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        self.0.metadata(path)
    }
    fn symlink_metadata(&self, path: &Path) -> io::Result<Metadata> {
        self.0.symlink_metadata(path)
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        self.0.read_dir(path)
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn io::Read + Send>> {
        self.0.open_read(path)
    }
    fn open_write(
        &self,
        path: &Path,
        mode: WriteMode,
        run: Option<&ush_shell::Run>,
    ) -> io::Result<Box<dyn io::Write + Send>> {
        self.0.open_write(path, mode, run)
    }
    fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        self.0.read_link(path)
    }
    fn file_action(&self, call: &ush_shell::FileCall<'_>) -> i32 {
        self.0.file_action(call)
    }
    fn program(
        &self,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        io: &Stdio,
    ) -> Program {
        self.0.program(argv, env, cwd, io)
    }
    fn now(&self) -> i64 {
        86_400
    }
    fn utc_offset(&self, _at: i64) -> i32 {
        3600
    }
    fn user_name(&self, uid: u32) -> Option<String> {
        (uid == 1000).then(|| "xaver".into())
    }
    fn group_name(&self, gid: u32) -> Option<String> {
        (gid == 1000).then(|| "staff".into())
    }
}

// cat

#[test]
fn cat_concatenates_and_reads_stdin_for_dash() {
    let host = files();
    let r = run_on(&host, &[], b"in\n", "cat", &["a.txt", "-"]);
    assert_eq!(ok(r), format!("{A_TXT}in\n"));
}

#[test]
fn cat_shows_all_and_numbers_like_gnu() {
    let host = files();
    assert_eq!(
        ok(run(&host, "cat", &["-A", "a.txt"])),
        "one$\ntwo$\n$\n$\n^Ithree$\nfour$\n"
    );
    assert_eq!(
        ok(run(&host, "cat", &["-n", "a.txt"])),
        "     1\tone\n     2\ttwo\n     3\t\n     4\t\n     5\t\tthree\n     6\tfour\n"
    );
    assert_eq!(
        ok(run(&host, "cat", &["-b", "a.txt"])),
        "     1\tone\n     2\ttwo\n\n\n     3\t\tthree\n     4\tfour\n"
    );
    assert_eq!(
        ok(run(&host, "cat", &["-sn", "a.txt"])),
        "     1\tone\n     2\ttwo\n     3\t\n     4\t\tthree\n     5\tfour\n"
    );
    let r = run_on(&host, &[], b"x\x01\x7f\xc3\xa9\n", "cat", &["-v"]);
    assert_eq!(ok(r), "x^A^?M-CM-)\n");
}

#[test]
fn cat_reports_a_missing_file_and_goes_on() {
    let host = files();
    let r = run(&host, "cat", &["nosuch", "a.txt"]);
    assert_eq!(r.status, 1);
    assert_eq!(r.err, "cat: nosuch: No such file or directory\n");
    assert_eq!(r.out, A_TXT);
}

/// A standard output whose reader has gone away, as `cmd | head -1` leaves
/// it once `head` is done.
fn broken_stdout() -> Output {
    let (reader, writer) = pipe();
    drop(reader);
    Output::new(writer)
}

fn run_broken(host: &MemoryHost, stdin: Input, name: &str, args: &[&str]) -> Ran {
    let (stderr, err) = Output::buffer();
    let io = Stdio {
        stdin,
        stdout: broken_stdout(),
        stderr,
    };
    let ctx = Ctx {
        host,
        cwd: Path::new("/w"),
        io: &io,
        env: &[],
        name,
        dispatch: &ush_shell::commands::NoDispatch,
        render: ush_shell::commands::Render::Text,
        run: None,
    };
    let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
    let status = lookup(name).unwrap()(&ctx, &args);
    Ran {
        status,
        out: String::new(),
        err: String::from_utf8_lossy(&err.take()).into_owned(),
    }
}

#[test]
fn cat_and_yes_stop_quietly_on_a_broken_pipe() {
    let host = files();
    for args in [&["a.txt"][..], &["-n", "a.txt"]] {
        let r = run_broken(&host, Input::null(), "cat", args);
        assert_eq!((r.status, r.err.as_str()), (141, ""));
    }
    let r = run_broken(&host, Input::null(), "yes", &[]);
    assert_eq!((r.status, r.err.as_str()), (141, ""));
}

#[test]
fn yes_feeds_head_through_a_pipe_until_head_is_done() {
    let host = MemoryHost::new();
    let (reader, writer) = pipe();
    let producer = {
        let host = host.clone();
        std::thread::spawn(move || {
            let io = Stdio {
                stdin: Input::null(),
                stdout: Output::new(writer),
                stderr: Output::null(),
            };
            let ctx = Ctx {
                host: &host,
                cwd: Path::new("/"),
                io: &io,
                env: &[],
                name: "yes",
                dispatch: &ush_shell::commands::NoDispatch,
                render: ush_shell::commands::Render::Text,
                run: None,
            };
            lookup("yes").unwrap()(&ctx, &["ok".to_string()])
        })
    };
    let (stdout, out) = Output::buffer();
    let io = Stdio {
        stdin: Input::new(reader),
        stdout,
        stderr: Output::null(),
    };
    let ctx = Ctx {
        host: &host,
        cwd: Path::new("/"),
        io: &io,
        env: &[],
        name: "head",
        dispatch: &ush_shell::commands::NoDispatch,
        render: ush_shell::commands::Render::Text,
        run: None,
    };
    assert_eq!(lookup("head").unwrap()(&ctx, &["-n".into(), "3".into()]), 0);
    drop(io);
    assert_eq!(out.take(), b"ok\nok\nok\n");
    assert_eq!(producer.join().unwrap(), 141);
}

// head and tail

#[test]
fn head_counts_lines_and_bytes_with_headers() {
    let host = files();
    assert_eq!(
        ok(run(&host, "head", &["-n", "3", "a.txt", "greek.txt"])),
        "==> a.txt <==\none\ntwo\n\n\n==> greek.txt <==\nalpha\nbeta\ngamma\n"
    );
    assert_eq!(
        ok(run(&host, "head", &["-2", "greek.txt"])),
        "alpha\nbeta\n"
    );
    assert_eq!(ok(run(&host, "head", &["-c", "5", "greek.txt"])), "alpha");
    assert_eq!(
        ok(run(&host, "head", &["-n", "-10", "greek.txt"])),
        "alpha\nbeta\n"
    );
    assert_eq!(
        ok(run(&host, "head", &["-qn1", "a.txt", "greek.txt"])),
        "one\nalpha\n"
    );
    assert_eq!(
        ok(run_in("x\ny\n", "head", &["-v", "-n1"])),
        "==> standard input <==\nx\n"
    );
}

#[test]
fn head_leaves_the_rest_of_stdin_for_the_next_reader() {
    let host = MemoryHost::new();
    let (stdout, out) = Output::buffer();
    let io = Stdio {
        stdin: Input::bytes(b"1\n2\n3\n".to_vec()),
        stdout,
        stderr: Output::null(),
    };
    let ctx = Ctx {
        host: &host,
        cwd: Path::new("/"),
        io: &io,
        env: &[],
        name: "head",
        dispatch: &ush_shell::commands::NoDispatch,
        render: ush_shell::commands::Render::Text,
        run: None,
    };
    assert_eq!(lookup("head").unwrap()(&ctx, &["-n1".into()]), 0);
    assert_eq!(out.take(), b"1\n");
    assert_eq!(io.stdin.read_all().unwrap(), b"2\n3\n");
}

#[test]
fn tail_counts_from_the_end_or_from_a_line() {
    let host = files();
    assert_eq!(
        ok(run(&host, "tail", &["-n", "2", "greek.txt"])),
        "lambda\nmu\n"
    );
    assert_eq!(
        ok(run(&host, "tail", &["-n", "+11", "greek.txt"])),
        "lambda\nmu\n"
    );
    assert_eq!(ok(run(&host, "tail", &["-c", "4", "greek.txt"])), "\nmu\n");
    assert_eq!(
        ok(run(&host, "tail", &["-3", "greek.txt"])),
        "kappa\nlambda\nmu\n"
    );
    assert_eq!(
        ok(run(&host, "tail", &["-c", "+3", "greek.txt"])).len(),
        GREEK.len() - 2
    );
    assert_eq!(
        ok(run(&host, "tail", &["-n1", "a.txt", "greek.txt"])),
        "==> a.txt <==\nfour\n\n==> greek.txt <==\nmu\n"
    );
    assert_eq!(ok(run_in("a\nb", "tail", &["-n1"])), "b");
}

#[test]
fn tail_refuses_to_follow() {
    let host = files();
    let r = run(&host, "tail", &["-f", "greek.txt"]);
    assert_eq!(r.status, 2);
    assert_eq!(r.err, "tail: -f is not supported\n");
    assert_eq!(run(&host, "tail", &["--follow", "greek.txt"]).status, 2);
}

// wc

#[test]
fn wc_aligns_columns_like_gnu() {
    let host = files();
    assert_eq!(
        ok(run(&host, "wc", &["a.txt", "greek.txt"])),
        " 6  4 22 a.txt\n12 12 67 greek.txt\n18 16 89 total\n"
    );
    assert_eq!(ok(run(&host, "wc", &["a.txt"])), " 6  4 22 a.txt\n");
    assert_eq!(ok(run(&host, "wc", &["-l", "greek.txt"])), "12 greek.txt\n");
    assert_eq!(
        ok(run_in("hello world\nfoo\n", "wc", &[])),
        "      2       3      16\n"
    );
    assert_eq!(ok(run_in("hello world\nfoo\n", "wc", &["-l"])), "2\n");
    assert_eq!(
        ok(run_in("a b\n", "wc", &["-w", "-l"])),
        "      1       2\n"
    );
    assert_eq!(ok(run_in("héllo wörld\n", "wc", &["-m"])), "12\n");
    assert_eq!(ok(run_in("héllo wörld\n", "wc", &["-c"])), "14\n");
}

// grep

#[test]
fn grep_numbers_counts_and_lists() {
    let host = files();
    assert_eq!(
        ok(run(&host, "grep", &["-n", "eta", "greek.txt"])),
        "2:beta\n6:zeta\n7:eta\n8:theta\n"
    );
    assert_eq!(
        ok(run(&host, "grep", &["-c", "a", "greek.txt", "a.txt"])),
        "greek.txt:10\na.txt:0\n"
    );
    assert_eq!(ok(run(&host, "grep", &["-vc", "a", "greek.txt"])), "2\n");
    assert_eq!(
        ok(run(&host, "grep", &["-l", "alpha", "greek.txt", "a.txt"])),
        "greek.txt\n"
    );
    assert_eq!(
        ok(run(&host, "grep", &["-L", "alpha", "greek.txt", "a.txt"])),
        "a.txt\n"
    );
    assert_eq!(
        ok(run(&host, "grep", &["-o", "e[a-z]", "greek.txt"]))
            .lines()
            .take(5)
            .collect::<Vec<_>>(),
        ["et", "el", "ep", "et", "et"]
    );
    assert_eq!(
        ok(run_in("x\n", "grep", &["-H", "x"])),
        "(standard input):x\n"
    );
    assert_eq!(
        ok(run_in("abc\n", "grep", &["-n", "-e", "a", "-e", "c", "-o"])),
        "1:a\n1:c\n"
    );
}

#[test]
fn grep_status_says_match_no_match_or_trouble() {
    let host = files();
    assert_eq!(run(&host, "grep", &["-q", "alpha", "greek.txt"]).status, 0);
    let r = run(&host, "grep", &["nothing-here", "greek.txt"]);
    assert_eq!((r.status, r.out.as_str()), (1, ""));
    let r = run(&host, "grep", &["x", "nosuch"]);
    assert_eq!(
        (r.status, r.err.as_str()),
        (2, "grep: nosuch: No such file or directory\n")
    );
    let r = run(&host, "grep", &["-s", "x", "nosuch"]);
    assert_eq!((r.status, r.err.as_str()), (2, ""));
    // A match elsewhere does not hide the trouble, except under -q.
    assert_eq!(
        run(&host, "grep", &["alpha", "greek.txt", "nosuch"]).status,
        2
    );
    assert_eq!(
        run(&host, "grep", &["-qs", "alpha", "greek.txt", "nosuch"]).status,
        0
    );
}

#[test]
fn grep_context_groups_are_set_apart() {
    let host = files();
    assert_eq!(
        ok(run(
            &host,
            "grep",
            &["-C1", "-e", "beta", "-e", "kappa", "greek.txt"]
        )),
        "alpha\nbeta\ngamma\n--\niota\nkappa\nlambda\n"
    );
    assert_eq!(
        ok(run_in("1\n2\nm\n3\n4\n5\n6\nm\n7\n", "grep", &["-A1", "m"])),
        "m\n3\n--\nm\n7\n"
    );
    assert_eq!(
        ok(run_in("m\nm\nx\n", "grep", &["-B1", "-A1", "m"])),
        "m\nm\nx\n"
    );
    assert_eq!(
        ok(run_in("1\nm\n2\n", "grep", &["-n", "-1", "m"])),
        "1-1\n2:m\n3-2\n"
    );
    assert_eq!(
        ok(run(&host, "grep", &["-m2", "-A1", "a", "greek.txt"])),
        "alpha\nbeta\ngamma\n"
    );
}

#[test]
fn grep_reads_basic_and_extended_patterns() {
    assert_eq!(
        ok(run_in("a+b\naab\nab\n", "grep", &["a\\+b"])),
        "aab\nab\n"
    );
    assert_eq!(ok(run_in("a+b\naab\n", "grep", &["a+b"])), "a+b\n");
    assert_eq!(
        ok(run_in("ab\nabab\n", "grep", &["-x", "\\(ab\\)\\{2\\}"])),
        "abab\n"
    );
    assert_eq!(
        ok(run_in("ab\nabab\n", "grep", &["-xE", "(ab){2}"])),
        "abab\n"
    );
    assert_eq!(ok(run_in("x{1}\n", "grep", &["-E", "x{1"])), "x{1}\n");
    assert_eq!(ok(run_in("*star\nx\n", "grep", &["*star"])), "*star\n");
    assert_eq!(ok(run_in("a.c\nabc\n", "grep", &["-F", "a.c"])), "a.c\n");
    assert_eq!(
        ok(run_in("Hello\nhello\n", "grep", &["-i", "HELLO"])),
        "Hello\nhello\n"
    );
    assert_eq!(ok(run_in("a1\nb\n", "grep", &["[[:digit:]]"])), "a1\n");
    assert_eq!(ok(run_in("a]b\nab\n", "grep", &["[]]"])), "a]b\n");
    assert_eq!(ok(run_in("cost $5\n", "grep", &["$5"])), "cost $5\n");
    assert_eq!(
        ok(run_in(
            "foo bar\nfoobar\nbar_foo\nfoo\n",
            "grep",
            &["-w", "foo"]
        )),
        "foo bar\nfoo\n"
    );
    assert_eq!(
        ok(run_in("one\ntwo\n", "grep", &["one\\|two"])),
        "one\ntwo\n"
    );
    let r = run_in("aa\n", "grep", &["\\(a\\)\\1"]);
    assert_eq!(r.status, 2);
    assert!(r.err.contains("back-references"), "{}", r.err);
}

#[test]
fn grep_searches_directories_recursively() {
    let host = MemoryHost::new();
    host.file("/w/d/x.txt", "hello\n");
    host.file("/w/d/sub/y.md", "hello\n");
    host.file("/w/d/.hid/z.txt", "hello\n");
    assert_eq!(
        ok(run(&host, "grep", &["-r", "hello", "d"])),
        "d/.hid/z.txt:hello\nd/sub/y.md:hello\nd/x.txt:hello\n"
    );
    assert_eq!(
        ok(run(&host, "grep", &["-r", "hello"])),
        "d/.hid/z.txt:hello\nd/sub/y.md:hello\nd/x.txt:hello\n"
    );
    assert_eq!(
        ok(run(&host, "grep", &["-r", "--include=*.txt", "hello", "d"])),
        "d/.hid/z.txt:hello\nd/x.txt:hello\n"
    );
    assert_eq!(
        ok(run(
            &host,
            "grep",
            &["-r", "--exclude-dir=sub", "--exclude", "z*", "hello", "d"]
        )),
        "d/x.txt:hello\n"
    );
    assert_eq!(ok(run(&host, "grep", &["-rc", "hello", "d/x.txt"])), "1\n");
    assert_eq!(
        ok(run(&host, "grep", &["-rh", "hello", "d/sub"])),
        "hello\n"
    );
    let r = run(&host, "grep", &["hello", "d"]);
    assert_eq!((r.status, r.err.as_str()), (2, "grep: d: Is a directory\n"));
}

#[test]
fn grep_says_a_binary_file_matches() {
    let host = MemoryHost::new();
    host.file("/w/bin", b"a\0b\n".to_vec());
    let r = run(&host, "grep", &["a", "bin"]);
    assert_eq!(
        (r.status, r.out.as_str(), r.err.as_str()),
        (0, "", "grep: bin: binary file matches\n")
    );
    assert_eq!(ok(run(&host, "grep", &["-c", "a", "bin"])), "1\n");
    assert_eq!(ok(run(&host, "grep", &["-a", "b", "bin"])), "a\0b\n");
}

// diff

const LEFT: &str = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
const RIGHT: &str = "a\nB\nc\nd\ne\nf\ng\nh\nI\nj\nk\n";

fn diff_files() -> MemoryHost {
    let host = MemoryHost::new();
    host.file("/w/l", LEFT);
    host.file("/w/r", RIGHT);
    host
}

#[test]
fn diff_normal_format() {
    let host = diff_files();
    let r = run(&host, "diff", &["l", "r"]);
    assert_eq!(r.status, 1);
    assert_eq!(
        r.out,
        "2c2\n< b\n---\n> B\n9c9\n< i\n---\n> I\n10a11\n> k\n"
    );
    host.file("/w/d1", "1\n2\n3\n");
    host.file("/w/d2", "1\n3\n");
    assert_eq!(run(&host, "diff", &["d1", "d2"]).out, "2d1\n< 2\n");
    assert_eq!(run(&host, "diff", &["d2", "d1"]).out, "1a2\n> 2\n");
    host.file("/w/e", "");
    assert_eq!(run(&host, "diff", &["e", "d2"]).out, "0a1,2\n> 1\n> 3\n");
    assert_eq!(run(&host, "diff", &["d2", "e"]).out, "1,2d0\n< 1\n< 3\n");
}

#[test]
fn diff_unified_format_merges_near_hunks() {
    let host = diff_files();
    let r = run(&host, "diff", &["-u", "l", "r"]);
    assert_eq!(r.status, 1);
    assert_eq!(
        r.out,
        "--- l\t1970-01-01 00:00:01.000000000 +0000\n\
         +++ r\t1970-01-01 00:00:02.000000000 +0000\n\
         @@ -1,10 +1,11 @@\n a\n-b\n+B\n c\n d\n e\n f\n g\n h\n-i\n+I\n j\n+k\n"
    );
    let r = run(&host, "diff", &["-U1", "l", "r"]);
    let body: Vec<&str> = r.out.lines().skip(2).collect();
    assert_eq!(
        body,
        [
            "@@ -1,3 +1,3 @@",
            " a",
            "-b",
            "+B",
            " c",
            "@@ -8,3 +8,4 @@",
            " h",
            "-i",
            "+I",
            " j",
            "+k"
        ]
    );
    host.file("/w/e", "");
    host.file("/w/n", "x\nz\n");
    let r = run(&host, "diff", &["-u", "e", "n"]);
    assert_eq!(
        r.out.lines().skip(2).collect::<Vec<_>>(),
        ["@@ -0,0 +1,2 @@", "+x", "+z"]
    );
}

#[test]
fn diff_marks_a_missing_newline() {
    let host = MemoryHost::new();
    host.file("/w/n1", "x\ny");
    host.file("/w/n2", "x\nz\n");
    assert_eq!(
        run(&host, "diff", &["n1", "n2"]).out,
        "2c2\n< y\n\\ No newline at end of file\n---\n> z\n"
    );
    let r = run(&host, "diff", &["-u", "n1", "n2"]);
    assert_eq!(
        r.out.lines().skip(2).collect::<Vec<_>>(),
        [
            "@@ -1,2 +1,2 @@",
            " x",
            "-y",
            "\\ No newline at end of file",
            "+z"
        ]
    );
}

#[test]
fn diff_brief_identical_and_trouble() {
    let host = diff_files();
    let r = run(&host, "diff", &["-q", "l", "r"]);
    assert_eq!((r.status, r.out.as_str()), (1, "Files l and r differ\n"));
    let r = run(&host, "diff", &["-s", "l", "l"]);
    assert_eq!(
        (r.status, r.out.as_str()),
        (0, "Files l and l are identical\n")
    );
    assert_eq!(run(&host, "diff", &["l", "l"]).status, 0);
    let r = run(&host, "diff", &["l", "nosuch"]);
    assert_eq!(
        (r.status, r.err.as_str()),
        (2, "diff: nosuch: No such file or directory\n")
    );
    host.file("/w/bin", b"a\0".to_vec());
    let r = run(&host, "diff", &["bin", "l"]);
    assert_eq!(
        (r.status, r.out.as_str()),
        (1, "Binary files bin and l differ\n")
    );
    assert_eq!(run(&host, "diff", &["-i", "l", "l"]).status, 0);
    host.file("/w/up", "A\nb  c\n");
    host.file("/w/low", "a\nb c\n");
    assert_eq!(run(&host, "diff", &["-ib", "up", "low"]).status, 0);
    assert_eq!(run(&host, "diff", &["-r", "l", "r"]).status, 2);
}

#[test]
fn diff_finds_a_shortest_script_on_shuffled_lines() {
    let host = MemoryHost::new();
    host.file("/w/x", "a\nb\nc\na\nb\nb\na\n");
    host.file("/w/y", "c\nb\na\nb\na\nc\n");
    let r = run(&host, "diff", &["x", "y"]);
    let changed = r
        .out
        .lines()
        .filter(|l| l.starts_with('<') || l.starts_with('>'))
        .count();
    // The classic Myers example: five edits, no fewer.
    assert_eq!(changed, 5, "{}", r.out);
}

// sort and uniq

const S: &str = "b 2\na 10\nc 1\nB 3\na 2\n";

#[test]
fn sort_orders_like_gnu_in_the_c_locale() {
    assert_eq!(ok(run_in(S, "sort", &[])), "B 3\na 10\na 2\nb 2\nc 1\n");
    assert_eq!(ok(run_in(S, "sort", &["-f"])), "a 10\na 2\nb 2\nB 3\nc 1\n");
    assert_eq!(
        ok(run_in(S, "sort", &["-k2n"])),
        "c 1\na 2\nb 2\nB 3\na 10\n"
    );
    assert_eq!(
        ok(run_in(S, "sort", &["-k2,2nr"])),
        "a 10\nB 3\na 2\nb 2\nc 1\n"
    );
    assert_eq!(
        ok(run_in(S, "sort", &["-u", "-k1,1"])),
        "B 3\na 10\nb 2\nc 1\n"
    );
    assert_eq!(ok(run_in(S, "sort", &["-r"])), "c 1\nb 2\na 2\na 10\nB 3\n");
    assert_eq!(
        ok(run_in("10K\n2M\n1G\n500\n-3K\n", "sort", &["-h"])),
        "-3K\n500\n10K\n2M\n1G\n"
    );
    assert_eq!(
        ok(run_in("3\n-1\n2.5\n10\nx\n", "sort", &["-n"])),
        "-1\nx\n2.5\n3\n10\n"
    );
    assert_eq!(
        ok(run_in("a:3\nb:1\nc:2\n", "sort", &["-t:", "-k2"])),
        "b:1\nc:2\na:3\n"
    );
    assert_eq!(ok(run_in("b\na\na\n", "sort", &["-u"])), "a\nb\n");
}

#[test]
fn sort_checks_order() {
    let r = run_in("a\nb\na\n", "sort", &["-c"]);
    assert_eq!((r.status, r.err.as_str()), (1, "sort: -:3: disorder: a\n"));
    assert_eq!(run_in("a\nb\n", "sort", &["-c"]).status, 0);
}

#[test]
fn uniq_counts_and_filters() {
    assert_eq!(
        ok(run_in("a\na\nb\nA\nc\nc\nc\n", "uniq", &["-c"])),
        "      2 a\n      1 b\n      1 A\n      3 c\n"
    );
    assert_eq!(ok(run_in("a\na\nb\nA\nc\n", "uniq", &["-d"])), "a\n");
    assert_eq!(ok(run_in("a\na\nb\nA\nc\n", "uniq", &["-u"])), "b\nA\nc\n");
    assert_eq!(
        ok(run_in("a\nA\nb\n", "uniq", &["-ci"])),
        "      2 a\n      1 b\n"
    );
}

// cut and tr

#[test]
fn cut_fields_and_bytes() {
    assert_eq!(
        ok(run_in("a:b:c:d\nnodelim\n", "cut", &["-d:", "-f2,4"])),
        "b:d\nnodelim\n"
    );
    assert_eq!(
        ok(run_in("a:b:c:d\nnodelim\n", "cut", &["-d:", "-f2-", "-s"])),
        "b:c:d\n"
    );
    assert_eq!(ok(run_in("abcdef\n", "cut", &["-c2-4,6"])), "bcdf\n");
    assert_eq!(
        ok(run_in(
            "abcdef\n",
            "cut",
            &["-b", "-2", "--output-delimiter=,"]
        )),
        "ab\n"
    );
    assert_eq!(
        ok(run_in(
            "abcdef\n",
            "cut",
            &["-c1,3-4", "--output-delimiter=,"]
        )),
        "a,cd\n"
    );
    assert_eq!(
        ok(run_in("a:b:c\n", "cut", &["-d:", "-f1", "--complement"])),
        "b:c\n"
    );
    assert_eq!(ok(run_in("a\tb\n", "cut", &["-f2"])), "b\n");
    let r = run_in("x\n", "cut", &["-f0"]);
    assert_eq!(r.status, 2);
    assert_eq!(run_in("x\n", "cut", &[]).status, 2);
}

#[test]
fn tr_translates_deletes_and_squeezes() {
    assert_eq!(
        ok(run_in("Hello World\n", "tr", &["a-z", "A-Z"])),
        "HELLO WORLD\n"
    );
    assert_eq!(
        ok(run_in("Hello World\n", "tr", &["[:lower:]", "[:upper:]"])),
        "HELLO WORLD\n"
    );
    assert_eq!(ok(run_in("aabbccdd\n", "tr", &["-s", "a-c"])), "abcdd\n");
    assert_eq!(ok(run_in("hello 123\n", "tr", &["-d", "0-9"])), "hello \n");
    assert_eq!(
        ok(run_in("hello 123\n", "tr", &["-cd", "a-z\\n"])),
        "hello\n"
    );
    assert_eq!(ok(run_in("aabbcc\n", "tr", &["-s", "abc", "xyz"])), "xyz\n");
    assert_eq!(ok(run_in("abcd\n", "tr", &["abcd", "xy"])), "xyyy\n");
    assert_eq!(ok(run_in("abc\n", "tr", &["-c", "a", "z"])), "azzz");
    assert_eq!(ok(run_in("a-b\n", "tr", &["a\\-", "xy"])), "xyb\n");
    assert_eq!(ok(run_in("abc\n", "tr", &["a-c", "[x*]"])), "xxx\n");
    assert_eq!(run_in("x", "tr", &["z-a", "x"]).status, 2);
}

// seq, nl, rev, tee

#[test]
fn seq_counts_with_steps_widths_and_fractions() {
    assert_eq!(ok(run_in("", "seq", &["3"])), "1\n2\n3\n");
    assert_eq!(ok(run_in("", "seq", &["2", "4"])), "2\n3\n4\n");
    assert_eq!(
        ok(run_in("", "seq", &["-s", " ", "1", "2", "7"])),
        "1 3 5 7\n"
    );
    assert_eq!(
        ok(run_in("", "seq", &["-w", "8", "11"])),
        "08\n09\n10\n11\n"
    );
    assert_eq!(ok(run_in("", "seq", &["-s,", "1", "5"])), "1,2,3,4,5\n");
    assert_eq!(
        ok(run_in("", "seq", &["0.5", "0.5", "2"])),
        "0.5\n1.0\n1.5\n2.0\n"
    );
    assert_eq!(ok(run_in("", "seq", &["5", "1"])), "");
    assert_eq!(ok(run_in("", "seq", &["3", "-1", "1"])), "3\n2\n1\n");
    assert_eq!(ok(run_in("", "seq", &["-w", "-1", "1"])), "-1\n00\n01\n");
    assert_eq!(
        ok(run_in("", "seq", &["1", "0.3", "2"])),
        "1.0\n1.3\n1.6\n1.9\n"
    );
    assert_eq!(run_in("", "seq", &["1", "0", "2"]).status, 2);
    assert_eq!(run_in("", "seq", &["-f", "%g", "2"]).status, 2);
}

#[test]
fn nl_numbers_non_empty_lines() {
    let host = files();
    assert_eq!(
        ok(run(&host, "nl", &["a.txt"])),
        "     1\tone\n     2\ttwo\n       \n       \n     3\t\tthree\n     4\tfour\n"
    );
    assert_eq!(
        ok(run_in("a\n\nb\n", "nl", &["-ba", "-w2", "-s", ": "])),
        " 1: a\n 2: \n 3: b\n"
    );
}

#[test]
fn rev_reverses_characters() {
    assert_eq!(ok(run_in("abc\nhéllo\nend", "rev", &[])), "cba\nolléh\ndne");
}

#[test]
fn tee_copies_to_files_and_stdout() {
    let host = MemoryHost::new();
    host.dir("/w");
    host.file("/w/old", "x\n");
    let r = run_on(&host, &[], b"data\n", "tee", &["new", "-"]);
    assert_eq!(ok(r), "data\n");
    assert_eq!(host.contents("/w/new").unwrap(), b"data\n");
    let r = run_on(&host, &[], b"more\n", "tee", &["-a", "old"]);
    assert_eq!(ok(r), "more\n");
    assert_eq!(host.contents("/w/old").unwrap(), b"x\nmore\n");
    let r = run_on(&host, &[], b"z\n", "tee", &["/nodir/f"]);
    assert_eq!((r.status, r.out.as_str()), (1, "z\n"));
}

// env, printenv, sleep, yes

fn env() -> Vec<(String, String)> {
    vec![
        ("HOME".into(), "/home/x".into()),
        ("LANG".into(), "C".into()),
    ]
}

#[test]
fn printenv_and_env_print_the_environment() {
    let host = MemoryHost::new();
    let vars = env();
    assert_eq!(
        ok(run_on(&host, &vars, b"", "printenv", &[])),
        "HOME=/home/x\nLANG=C\n"
    );
    assert_eq!(ok(run_on(&host, &vars, b"", "printenv", &["LANG"])), "C\n");
    let r = run_on(&host, &vars, b"", "printenv", &["NOPE", "HOME"]);
    assert_eq!((r.status, r.out.as_str()), (1, "/home/x\n"));
    assert_eq!(
        ok(run_on(&host, &vars, b"", "env", &[])),
        "HOME=/home/x\nLANG=C\n"
    );
    assert_eq!(
        ok(run_on(&host, &vars, b"", "env", &["-u", "HOME", "X=1"])),
        "LANG=C\nX=1\n"
    );
    assert_eq!(
        ok(run_on(&host, &vars, b"", "env", &["-i", "A=b"])),
        "A=b\n"
    );
    // A command goes back through dispatch: with none, it is unknown.
    let r = run_on(&host, &vars, b"", "env", &["ls"]);
    assert_eq!((r.status, r.err.as_str()), (127, "ls: command not found\n"));
}

/// A host whose clock moves only when the shell sleeps on it, and that can
/// be told to interrupt after some sleeping.
#[derive(Clone)]
struct Clock {
    files: MemoryHost,
    slept: Arc<AtomicU64>,
    interrupt_after: u64,
    interrupted: Arc<AtomicBool>,
}

impl Host for Clock {
    fn metadata(&self, path: &Path) -> io::Result<Metadata> {
        self.files.metadata(path)
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        self.files.read_dir(path)
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn io::Read + Send>> {
        self.files.open_read(path)
    }
    fn open_write(
        &self,
        path: &Path,
        mode: WriteMode,
        run: Option<&ush_shell::Run>,
    ) -> io::Result<Box<dyn io::Write + Send>> {
        self.files.open_write(path, mode, run)
    }
    fn file_action(&self, call: &ush_shell::FileCall<'_>) -> i32 {
        self.files.file_action(call)
    }
    fn program(
        &self,
        argv: &[String],
        env: &[(String, String)],
        cwd: &Path,
        io: &Stdio,
    ) -> Program {
        self.files.program(argv, env, cwd, io)
    }
    fn sleep(&self, millis: u64) {
        let total = self
            .slept
            .fetch_add(millis, Ordering::SeqCst)
            .saturating_add(millis);
        if total >= self.interrupt_after {
            self.interrupted.store(true, Ordering::SeqCst);
        }
    }
    fn interrupted(&self) -> bool {
        self.interrupted.load(Ordering::SeqCst)
    }
}

fn clock(interrupt_after: u64) -> Clock {
    Clock {
        files: MemoryHost::new(),
        slept: Arc::new(AtomicU64::new(0)),
        interrupt_after,
        interrupted: Arc::new(AtomicBool::new(false)),
    }
}

#[test]
fn sleep_waits_on_the_host_and_stops_when_interrupted() {
    let host = clock(u64::MAX);
    assert_eq!(run_on(&host, &[], b"", "sleep", &["1.5", "1m"]).status, 0);
    assert_eq!(host.slept.load(Ordering::SeqCst), 61_500);
    let host = clock(200);
    assert_eq!(run_on(&host, &[], b"", "sleep", &["1h"]).status, 130);
    assert_eq!(host.slept.load(Ordering::SeqCst), 200);
    let r = run_on(&clock(0), &[], b"", "sleep", &["soon"]);
    assert_eq!(
        (r.status, r.err.as_str()),
        (2, "sleep: invalid time interval 'soon'\n")
    );
}

#[test]
fn yes_stops_when_interrupted() {
    let host = clock(0);
    host.interrupted.store(true, Ordering::SeqCst);
    assert_eq!(run_on(&host, &[], b"", "yes", &["x"]).status, 130);
}

// ls

fn tree_host() -> MemoryHost {
    let host = MemoryHost::new();
    host.file("/w/d/b.txt", "bee\n");
    host.file("/w/d/a.txt", "hello\n");
    host.file("/w/d/sub/deep.rs", "fn main() {}\n");
    host.file("/w/d/.hidden", "");
    host.symlink("/w/d/link", "a.txt");
    host
}

#[test]
fn ls_lists_one_per_line_off_the_terminal() {
    let host = tree_host();
    assert_eq!(ok(run(&host, "ls", &["d"])), "a.txt\nb.txt\nlink\nsub\n");
    assert_eq!(
        ok(run(&host, "ls", &["-a", "d"])),
        ".\n..\n.hidden\na.txt\nb.txt\nlink\nsub\n"
    );
    assert_eq!(
        ok(run(&host, "ls", &["-A", "d"])),
        ".hidden\na.txt\nb.txt\nlink\nsub\n"
    );
    assert_eq!(
        ok(run(&host, "ls", &["-F", "d"])),
        "a.txt\nb.txt\nlink@\nsub/\n"
    );
    assert_eq!(
        ok(run(&host, "ls", &["-p", "d"])),
        "a.txt\nb.txt\nlink\nsub/\n"
    );
    assert_eq!(
        ok(run(&host, "ls", &["-r", "d"])),
        "sub\nlink\nb.txt\na.txt\n"
    );
    assert_eq!(
        ok(run(&host, "ls", &["-S", "d"])),
        "a.txt\nlink\nb.txt\nsub\n"
    );
    assert_eq!(
        ok(run(&host, "ls", &["-t", "d"])),
        "a.txt\nb.txt\nlink\nsub\n"
    );
    assert_eq!(ok(run(&host, "ls", &["-d", "d"])), "d\n");
    assert_eq!(
        ok(run(&host, "ls", &["d/sub", "d/a.txt"])),
        "d/a.txt\n\nd/sub:\ndeep.rs\n"
    );
    assert_eq!(
        ok(run(&host, "ls", &["-R", "d"])),
        "d:\na.txt\nb.txt\nlink\nsub\n\nd/sub:\ndeep.rs\n"
    );
    let r = run(&host, "ls", &["nosuch", "d/sub"]);
    assert_eq!(r.status, 2);
    assert_eq!(
        r.err,
        "ls: cannot access 'nosuch': No such file or directory\n"
    );
    assert_eq!(r.out, "d/sub:\ndeep.rs\n");
}

#[test]
fn ls_lays_out_columns_on_the_terminal_like_gnu() {
    let host = MemoryHost::new();
    for name in [
        "aaaaaaaaaaaa",
        "bbbbbbbbbb",
        "cccccccccccccccc",
        "dddddd",
        "eeeeeeeeeeeeeee",
        "ffffffffffff",
        "gggggggggggggggggg",
        "hhhhh",
        "iiiiiiiiiiiiiii",
        "jjjjj",
    ] {
        host.file(&format!("/w/c/{name}"), "");
    }
    let columns = |width: &str| {
        let (stdout, out) = Output::buffer();
        let io = Stdio {
            stdin: Input::null(),
            stdout: Output::host(Std::Out, OutputSink(stdout)),
            stderr: Output::null(),
        };
        let env = vec![("COLUMNS".to_string(), width.to_string())];
        let ctx = Ctx {
            host: &host,
            cwd: Path::new("/w"),
            io: &io,
            env: &env,
            name: "ls",
            dispatch: &ush_shell::commands::NoDispatch,
            render: ush_shell::commands::Render::Text,
            run: None,
        };
        assert_eq!(lookup("ls").unwrap()(&ctx, &["c".into()]), 0);
        String::from_utf8(out.take()).unwrap()
    };
    // What GNU ls 9.10 prints for these names (`COLUMNS=60 ls -C`).
    assert_eq!(
        columns("60"),
        "aaaaaaaaaaaa\t  eeeeeeeeeeeeeee     iiiiiiiiiiiiiii\n\
         bbbbbbbbbb\t  ffffffffffff\t      jjjjj\n\
         cccccccccccccccc  gggggggggggggggggg\n\
         dddddd\t\t  hhhhh\n"
    );
    assert_eq!(
        columns("80"),
        "aaaaaaaaaaaa\t  dddddd\t   gggggggggggggggggg  jjjjj\n\
         bbbbbbbbbb\t  eeeeeeeeeeeeeee  hhhhh\n\
         cccccccccccccccc  ffffffffffff\t   iiiiiiiiiiiiiii\n"
    );
}

/// Forwards to a captured [`Output`], so a buffer can stand in for the
/// host's own standard output.
struct OutputSink(Output);

impl io::Write for OutputSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write_all(buf)?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn ls_long_format_like_gnu() {
    let host = Named(tree_host());
    let r = run_on(&host, &[], b"", "ls", &["-l", "d"]);
    assert_eq!(
        ok(r),
        "total 8\n\
         -rw-r--r-- 1 xaver staff 6 Jan  1 01:00 a.txt\n\
         -rw-r--r-- 1 xaver staff 4 Jan  1 01:00 b.txt\n\
         lrwxrwxrwx 1 xaver staff 5 Jan  1 01:00 link -> a.txt\n\
         drwxr-xr-x 1 xaver staff 0 Jan  1 01:00 sub\n"
    );
    let memory = tree_host();
    let r = run(&memory, "ls", &["-ln", "d/a.txt"]);
    assert_eq!(ok(r), "-rw-r--r-- 1 1000 1000 6 Jan  1  1970 d/a.txt\n");
    memory.file("/w/big", vec![b'x'; 1536]);
    let r = run(&memory, "ls", &["-lh", "big"]);
    assert_eq!(ok(r), "-rw-r--r-- 1 1000 1000 1.5K Jan  1  1970 big\n");
}

#[test]
fn ls_refuses_what_it_does_not_do() {
    let host = tree_host();
    let r = run(&host, "ls", &["-i", "d"]);
    assert_eq!((r.status, r.err.as_str()), (2, "ls: -i is not supported\n"));
    assert_eq!(run(&host, "ls", &["--color=always"]).status, 2);
    assert_eq!(
        ok(run(&host, "ls", &["--color=never", "d/sub"])),
        "deep.rs\n"
    );
}

// stat

#[test]
fn stat_formats_and_default_layout() {
    let host = Named(tree_host());
    let r = run_on(
        &host,
        &[],
        b"",
        "stat",
        &[
            "-c",
            "%n %s %F %a %A %u %g %U %G %h %Y",
            "d/a.txt",
            "d",
            "d/link",
        ],
    );
    assert_eq!(
        ok(r),
        "d/a.txt 6 regular file 644 -rw-r--r-- 1000 1000 xaver staff 1 2\n\
         d 0 directory 755 drwxr-xr-x 1000 1000 xaver staff 1 0\n\
         d/link 5 symbolic link 777 lrwxrwxrwx 1000 1000 xaver staff 1 0\n"
    );
    let r = run_on(
        &host,
        &[],
        b"",
        "stat",
        &["--printf", "%n\\t%s\\n", "d/a.txt"],
    );
    assert_eq!(ok(r), "d/a.txt\t6\n");
    let r = run_on(&host, &[], b"", "stat", &["-c", "%y", "d/a.txt"]);
    assert_eq!(ok(r), "1970-01-01 01:00:02.000000000 +0100\n");
    let r = run_on(&host, &[], b"", "stat", &["d/a.txt"]);
    assert_eq!(
        ok(r),
        "  File: d/a.txt\n\
         \x20 Size: 6         \tBlocks: 8          IO Block: 4096   regular file\n\
         Device: 0,1\tInode: 0           Links: 1\n\
         Access: (0644/-rw-r--r--)  Uid: ( 1000/   xaver)   Gid: ( 1000/   staff)\n\
         Access: 1970-01-01 01:00:02.000000000 +0100\n\
         Modify: 1970-01-01 01:00:02.000000000 +0100\n\
         Change: 1970-01-01 01:00:02.000000000 +0100\n\
         \x20Birth: -\n"
    );
    let r = run_on(&host, &[], b"", "stat", &["nosuch"]);
    assert_eq!(
        (r.status, r.err.as_str()),
        (
            1,
            "stat: cannot statx 'nosuch': No such file or directory\n"
        )
    );
}

// find

#[test]
fn find_walks_and_filters() {
    let host = tree_host();
    assert_eq!(
        ok(run(&host, "find", &["d"])),
        "d\nd/.hidden\nd/a.txt\nd/b.txt\nd/link\nd/sub\nd/sub/deep.rs\n"
    );
    assert_eq!(
        ok(run(&host, "find", &["d", "-name", "*.txt"])),
        "d/a.txt\nd/b.txt\n"
    );
    assert_eq!(ok(run(&host, "find", &["d", "-type", "d"])), "d\nd/sub\n");
    assert_eq!(ok(run(&host, "find", &["d", "-type", "l"])), "d/link\n");
    assert_eq!(ok(run(&host, "find", &["d", "-maxdepth", "0"])), "d\n");
    assert_eq!(
        ok(run(&host, "find", &["d", "-mindepth", "2"])),
        "d/sub/deep.rs\n"
    );
    assert_eq!(ok(run(&host, "find", &["d", "-empty"])), "d/.hidden\n");
    assert_eq!(
        ok(run(
            &host,
            "find",
            &[
                "d", "-path", "*/sub", "-prune", "-o", "-type", "f", "-print"
            ]
        )),
        "d/.hidden\nd/a.txt\nd/b.txt\n"
    );
    assert_eq!(
        ok(run(
            &host,
            "find",
            &[
                "d", "(", "-name", "a*", "-o", "-name", "b*", ")", "-a", "!", "-type", "d"
            ]
        )),
        "d/a.txt\nd/b.txt\n"
    );
    assert_eq!(
        ok(run(&host, "find", &["d", "-iname", "A.TXT"])),
        "d/a.txt\n"
    );
    assert_eq!(
        ok(run(&host, "find", &["d", "-size", "+5c", "-type", "f"])),
        "d/a.txt\nd/sub/deep.rs\n"
    );
    assert_eq!(
        ok(run(
            &host,
            "find",
            &["d", "-newer", "d/a.txt", "-type", "f"]
        )),
        "d/.hidden\nd/sub/deep.rs\n"
    );
    assert_eq!(
        ok(run(&host, "find", &["d/sub", "-print0"])),
        "d/sub\0d/sub/deep.rs\0"
    );
    let r = run(&host, "find", &["nosuch"]);
    assert_eq!(
        (r.status, r.err.as_str()),
        (1, "find: 'nosuch': No such file or directory\n")
    );
}

#[test]
fn find_actions_go_back_through_dispatch() {
    // With nowhere to dispatch to, every command is unknown: find says
    // nothing itself, and its status is 1. `-ok` asks, and runs nothing
    // without a yes.
    let host = tree_host();
    for action in ["-exec", "-execdir"] {
        let r = run(&host, "find", &["d", action, "rm", "{}", ";"]);
        assert_eq!(r.status, 1, "{action}");
        assert!(
            r.err.starts_with("rm: command not found\n"),
            "{action}: {}",
            r.err
        );
        assert_eq!(r.out, "");
    }
    for action in ["-ok", "-okdir"] {
        let r = run(&host, "find", &["d", action, "rm", "{}", ";"]);
        assert_eq!(
            (r.status, r.err.as_str(), r.out.as_str()),
            (0, "", ""),
            "{action}"
        );
    }
}

// tree

#[test]
fn tree_draws_like_tree() {
    let host = tree_host();
    assert_eq!(
        ok(run(&host, "tree", &["d"])),
        "d\n├── a.txt\n├── b.txt\n├── link -> a.txt\n└── sub\n    └── deep.rs\n\n1 directory, 4 files\n"
    );
    assert_eq!(
        ok(run(&host, "tree", &["-a", "-L", "1", "d"])),
        "d\n├── .hidden\n├── a.txt\n├── b.txt\n├── link -> a.txt\n└── sub\n\n1 directory, 4 files\n"
    );
    assert_eq!(
        ok(run(&host, "tree", &["-d", "d"])),
        "d\n└── sub\n\n1 directory\n"
    );
    host.file("/w/d/sub/more/x", "");
    assert_eq!(
        ok(run(&host, "tree", &["d/sub"])),
        "d/sub\n├── deep.rs\n└── more\n    └── x\n\n1 directory, 2 files\n"
    );
    host.file("/w/d/zz/y", "");
    let out = ok(run(&host, "tree", &["d"]));
    assert!(
        out.contains("├── sub\n│   ├── deep.rs\n│   └── more\n│       └── x\n└── zz\n    └── y\n"),
        "{out}"
    );
}

// path commands

#[test]
fn basename_and_dirname_like_gnu() {
    assert_eq!(ok(run_in("", "basename", &["/a/b/c.txt", ".txt"])), "c\n");
    assert_eq!(ok(run_in("", "basename", &["/a/b/"])), "b\n");
    assert_eq!(ok(run_in("", "basename", &["-a", "x/y", "z/w"])), "y\nw\n");
    assert_eq!(
        ok(run_in("", "basename", &["-s", ".c", "a.c", "b.c"])),
        "a\nb\n"
    );
    assert_eq!(ok(run_in("", "basename", &["/"])), "/\n");
    assert_eq!(
        ok(run_in("", "dirname", &["/a/b/c", "a/b", "c", "/", "//x"])),
        "/a/b\na\n.\n/\n/\n"
    );
}

#[test]
fn realpath_and_readlink_follow_links_through_the_host() {
    let host = tree_host();
    host.symlink("/w/up", "d/sub");
    assert_eq!(ok(run(&host, "realpath", &["d/link"])), "/w/d/a.txt\n");
    assert_eq!(
        ok(run(&host, "realpath", &["up/deep.rs", "up/new"])),
        "/w/d/sub/deep.rs\n/w/d/sub/new\n"
    );
    assert_eq!(ok(run(&host, "realpath", &["-m", "/a/../b/./c"])), "/b/c\n");
    assert_eq!(ok(run(&host, "realpath", &["-s", "d/link"])), "/w/d/link\n");
    let r = run(&host, "realpath", &["nosuch/x"]);
    assert_eq!(
        (r.status, r.err.as_str()),
        (1, "realpath: nosuch/x: No such file or directory\n")
    );
    assert_eq!(ok(run(&host, "readlink", &["d/link"])), "a.txt\n");
    assert_eq!(
        ok(run(&host, "readlink", &["-f", "up/deep.rs"])),
        "/w/d/sub/deep.rs\n"
    );
    let r = run(&host, "readlink", &["d/a.txt"]);
    assert_eq!((r.status, r.out.as_str(), r.err.as_str()), (1, "", ""));
    assert_eq!(run(&host, "readlink", &["-e", "d/nope"]).status, 1);
}

// Everything refuses what it does not do, by name.

#[test]
fn unsupported_flags_are_refused_by_name() {
    let host = files();
    for (name, flag) in [
        ("cat", "--frobnicate"),
        ("head", "-x"),
        ("wc", "--files0-from=x"),
        ("grep", "-P"),
        ("grep", "-z"),
        ("diff", "-c"),
        ("diff", "-y"),
        ("sort", "-M"),
        ("sort", "-V"),
        ("uniq", "-x"),
        ("cut", "-x"),
        ("tr", "-x"),
        ("seq", "-f"),
        ("tee", "-p"),
        ("nl", "-p"),
        ("stat", "-t"),
        ("tree", "-f"),
        ("rev", "-x"),
        ("printenv", "-x"),
        ("realpath", "--relative-to=/"),
        ("sleep", "-x"),
        ("find", "-L"),
    ] {
        let args: Vec<&str> = match name {
            "find" => vec![flag, "."],
            "tr" => vec![flag, "a", "b"],
            "seq" => vec![flag, "%g", "3"],
            "diff" => vec![flag, "a.txt", "a.txt"],
            _ => vec![flag, "a.txt"],
        };
        let r = run(&host, name, &args);
        assert_eq!(r.status, 2, "{name} {flag}: {}", r.err);
        let spelled = flag.split('=').next().unwrap_or(flag);
        assert!(
            r.err.starts_with(&format!("{name}: {spelled}")) && r.err.contains("not supported"),
            "{name} {flag}: {}",
            r.err
        );
    }
}

#[test]
fn every_command_is_named() {
    let names = ush_shell::commands::names();
    for name in [
        "cat", "head", "tail", "wc", "grep", "diff", "sort", "uniq", "cut", "tr", "seq", "tee",
        "nl", "rev", "printenv", "env", "sleep", "yes", "ls", "stat", "find", "tree", "basename",
        "dirname", "realpath", "readlink",
    ] {
        assert!(names.contains(&name), "{name}");
        assert!(lookup(name).is_some(), "{name}");
    }
    assert!(lookup("cp").is_none());
}

// The options Tab completion offers

/// What `name OPTION` is refused with when the parser does not take it, or
/// takes it without the value it was given.
const REFUSALS: &[&str] = &[
    "invalid option",
    "unrecognized option",
    "is not supported",
    "unknown option",
    "requires an argument",
];

/// The value an option that takes one is given, as words: what follows the
/// option for it to be read.
#[expect(
    clippy::match_same_arms,
    reason = "one arm per command's options, read as they are listed"
)]
fn option_value(name: &str, bare: &str) -> Option<&'static str> {
    Some(match (name, bare) {
        ("basename", "-s" | "--suffix") => ".txt",
        ("find", "-name" | "-iname" | "-path" | "-ipath" | "-wholename" | "-iwholename") => "*.txt",
        ("find", "-type") => "f",
        ("find", "-size") => "+0",
        ("find", "-mtime" | "-mmin") => "+0",
        ("find", "-newer") => "a.txt",
        ("find", "-maxdepth" | "-mindepth") => "1",
        ("find", "-not" | "-and" | "-or") => "-true",
        ("find", "-exec" | "-execdir" | "-ok" | "-okdir") => "echo {} ;",
        ("ls" | "grep" | "diff", "--color") => "never",
        ("stat", "-c" | "--format" | "--printf") => "%n",
        ("tree", "-L") => "1",
        ("cut", "-f" | "--fields" | "-b" | "--bytes" | "-c" | "--characters") => "1",
        ("cut", "-d" | "--delimiter") => ",",
        ("cut", "--output-delimiter") => ":",
        ("diff", "-U") => "1",
        ("diff" | "grep", "--label") => "x",
        ("env", "-u" | "--unset") => "HOME",
        ("env", "-C" | "--chdir") => "/w",
        ("grep", "-m" | "--max-count") => "1",
        ("grep", "-A" | "--after-context" | "-B" | "--before-context" | "-C" | "--context") => "1",
        ("grep", "--include") => "*.txt",
        ("grep", "--exclude") => "*.md",
        ("grep", "--exclude-dir") => "d",
        ("grep", "-e" | "--regexp") => "one",
        ("grep", "-f" | "--file") => "a.txt",
        ("grep", "--binary-files") => "text",
        ("head" | "tail", "-n" | "--lines" | "-c" | "--bytes") => "2",
        ("nl", "-b" | "--body-numbering") => "a",
        ("nl", "-n" | "--number-format") => "ln",
        ("nl", "-w" | "--number-width") => "3",
        ("nl", "-s" | "--number-separator") => ":",
        ("nl", "-v" | "--starting-line-number" | "-i" | "--line-increment") => "1",
        ("seq", "-s" | "--separator") => ",",
        ("sort", "-k" | "--key") => "1",
        ("sort", "-t" | "--field-separator") => ",",
        ("sort", "-o" | "--output") => "out.txt",
        ("uniq", "-f" | "--skip-fields" | "-s" | "--skip-chars" | "-w" | "--check-chars") => "1",
        ("wc", "--total") => "always",
        ("nice", "-n" | "--adjustment") => "1",
        ("timeout", "-s" | "--signal") => "TERM",
        ("timeout", "-k" | "--kill-after") => "1",
        ("xargs", "-d" | "--delimiter") => ",",
        ("xargs", "-n" | "--max-args" | "-L" | "--max-lines" | "-P" | "--max-procs") => "1",
        ("xargs", "-I") => "{}",
        ("xargs", "-s" | "--max-chars") => "100",
        ("xargs", "-E" | "--eof") => "END",
        ("xargs", "-a" | "--arg-file") => "a.txt",
        _ => return None,
    })
}

/// The operands `name` is run on after an option, so it has something to
/// read.
#[expect(
    clippy::match_same_arms,
    reason = "one arm per command's options, read as they are listed"
)]
fn option_operands(name: &str, bare: &str) -> &'static [&'static str] {
    match (name, bare) {
        ("cut", "-f" | "--fields" | "-b" | "--bytes" | "-c" | "--characters") => &["a.txt"],
        ("cut", _) => &["-f", "1", "a.txt"],
        ("grep", "-e" | "--regexp" | "-f" | "--file") => &["a.txt"],
        ("grep", _) => &["one", "a.txt"],
        ("diff", _) => &["a.txt", "greek.txt"],
        ("env", "-0" | "--null") => &[],
        ("env" | "nice", _) => &["true"],
        ("timeout", _) => &["1", "true"],
        ("xargs", _) => &["echo", "{}"],
        ("tr", "-d" | "--delete") => &["a"],
        ("tr", _) => &["a", "b"],
        ("seq", _) => &["3"],
        ("tee", _) => &["out.txt"],
        ("dirname", _) => &["/w/a.txt"],
        ("printenv", _) => &["HOME"],
        ("find" | "tree", _) => &[],
        ("ls", _) => &["."],
        _ => &["a.txt"],
    }
}

/// `name OPTION`, with the option's value and operands, as a script in `/w`
/// with every word quoted.
fn option_script(name: &str, spelling: &str) -> String {
    let bare = spelling.trim_end_matches('=');
    let value = option_value(name, bare);
    let mut words: Vec<String> = vec![name.to_string()];
    if name == "find" && spelling != "-P" {
        words.push(".".into());
        if matches!(bare, "-and" | "-or") {
            words.push("-true".into());
        }
    }
    assert!(
        value.is_some() || !spelling.ends_with('='),
        "{name} {spelling}: no value to give it"
    );
    match (spelling.ends_with('='), value) {
        (true, Some(v)) => words.push(format!("{spelling}{v}")),
        (false, Some(v)) => {
            words.push(spelling.to_string());
            words.extend(v.split_whitespace().map(str::to_string));
        }
        (_, None) => words.push(spelling.to_string()),
    }
    if name == "find" && spelling == "-P" {
        words.push(".".into());
    }
    words.extend(option_operands(name, bare).iter().map(|w| (*w).to_string()));
    let quoted: Vec<String> = words.iter().map(|w| format!("'{w}'")).collect();
    format!("cd /w && {}", quoted.join(" "))
}

/// Standard error of `script` run through the shell on fresh files.
fn shell_stderr(script: &str) -> String {
    let host = files();
    host.file("/w/b.txt", "one\nthree\n").dir("/w/d");
    let (stdout, _out) = Output::buffer();
    let (stderr, err) = Output::buffer();
    let io = Stdio {
        stdin: Input::bytes(b"one\ntwo\n".to_vec()),
        stdout,
        stderr,
    };
    let env = [
        ("HOME".to_string(), "/w".to_string()),
        ("PATH".to_string(), "/bin".to_string()),
    ];
    let mut shell = ush_shell::Shell::new(Arc::new(host), env, PathBuf::from("/w"), io);
    shell.run_to_exit(script);
    drop(shell);
    String::from_utf8_lossy(&err.take()).into_owned()
}

fn assert_not_refused(name: &str, spelling: &str, script: &str) {
    let err = shell_stderr(script);
    assert!(
        !REFUSALS.iter().any(|r| err.contains(r)),
        "{name} {spelling}: `{script}` was refused: {err}"
    );
}

#[test]
fn every_option_offered_for_a_command_is_one_it_takes() {
    let mut tried = 0;
    for name in ush_shell::commands::names() {
        if ush_shell::commands::FILE_ACTIONS.contains(&name) {
            continue;
        }
        for opt in ush_shell::commands::options::of(name) {
            assert_not_refused(name, opt.spelling, &option_script(name, opt.spelling));
            tried += 1;
        }
    }
    assert!(tried > 300, "only {tried} options tried");
}

#[test]
fn every_option_offered_for_a_built_in_is_one_it_takes() {
    for (name, spelling, script) in [
        ("alias", "-p", "alias -p"),
        ("cd", "-L", "cd -L /w"),
        ("cd", "-P", "cd -P /w"),
        ("pwd", "-L", "pwd -L"),
        ("pwd", "-P", "pwd -P"),
        ("command", "-v", "command -v ls"),
        ("command", "-V", "command -V ls"),
        ("command", "-p", "command -p true"),
        ("echo", "-n", "echo -n x"),
        ("echo", "-e", "echo -e 'x\\ty'"),
        ("echo", "-E", "echo -E x"),
        ("export", "-n", "X=1; export X; export -n X"),
        ("export", "-p", "export -p"),
        ("mapfile", "-t", "mapfile -t lines"),
        ("mapfile", "-d", "mapfile -d , lines"),
        ("mapfile", "-n", "mapfile -n 1 lines"),
        ("mapfile", "-s", "mapfile -s 1 lines"),
        ("readarray", "-t", "readarray -t lines"),
        ("printf", "-v", "printf -v x %s y"),
        ("read", "-r", "read -r x"),
        ("read", "-p", "read -p '> ' x"),
        ("read", "-a", "read -a xs"),
        ("read", "-d", "read -d , x"),
        ("read", "-n", "read -n 2 x"),
        ("read", "-N", "read -N 2 x"),
        ("read", "-u", "read -u 0 x"),
        ("readonly", "-a", "readonly -a xs"),
        ("readonly", "-A", "readonly -A m"),
        ("readonly", "-p", "readonly -p"),
        ("set", "-o", "set -o pipefail"),
        ("shopt", "-s", "shopt -s nullglob"),
        ("shopt", "-u", "shopt -u nullglob"),
        ("shopt", "-q", "shopt -q nullglob"),
        ("trap", "-p", "trap -p"),
        ("trap", "-l", "trap -l"),
        ("type", "-t", "type -t ls"),
        ("unalias", "-a", "unalias -a"),
        ("unset", "-f", "unset -f f"),
        ("unset", "-v", "unset -v x"),
    ] {
        assert!(
            ush_shell::commands::options::of(name)
                .iter()
                .any(|o| o.spelling == spelling),
            "{name} {spelling} is not offered"
        );
        assert_not_refused(name, spelling, script);
    }
    let every = |name: &str, script: &dyn Fn(&str) -> String| {
        for opt in ush_shell::commands::options::of(name) {
            assert_not_refused(name, opt.spelling, &script(opt.spelling));
        }
    };
    every("set", &|s| {
        if s == "-o" {
            "set -o errexit".into()
        } else {
            format!("set {s}")
        }
    });
    every("declare", &|s| format!("declare {s} x"));
    every("typeset", &|s| format!("typeset {s} x"));
    every("local", &|s| format!("f() {{ local {s} x; }}; f"));
    every("test", &|s| {
        if s.len() == 3 {
            format!("test 1 {s} 1")
        } else {
            format!("test {s} a")
        }
    });
    every("[", &|s| {
        if s.len() == 3 {
            format!("[ 1 {s} 1 ]")
        } else {
            format!("[ {s} a ]")
        }
    });
}

#[test]
fn options_are_offered_for_every_command_that_takes_them() {
    let none = ["append", "nohup", "rev", "sleep", "touch", "yes"];
    for name in ush_shell::commands::names() {
        let opts = ush_shell::commands::options::of(name);
        if none.contains(&name) {
            assert!(opts.is_empty(), "{name} takes no options");
        } else {
            assert!(!opts.is_empty(), "{name} has no options offered");
        }
        let mut seen = std::collections::HashSet::new();
        for opt in opts {
            assert!(opt.spelling.starts_with('-'), "{name} {}", opt.spelling);
            assert!(seen.insert(opt.spelling), "{name} {} twice", opt.spelling);
            assert!(
                !opt.help.is_empty() && !opt.help.ends_with('.'),
                "{name} {}: {:?}",
                opt.spelling,
                opt.help
            );
        }
    }
    for name in [
        "ls", "grep", "find", "head", "tail", "wc", "sort", "cut", "diff", "cat", "xargs",
    ] {
        assert!(!ush_shell::commands::options::of(name).is_empty(), "{name}");
    }
    assert!(ush_shell::commands::options::of("no-such-command").is_empty());
}
