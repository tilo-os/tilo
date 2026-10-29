//! The two renderings of `ls`, `grep`, `find` and `diff`: GNU's text for
//! people, unchanged, and the short grouped form for an agent, which keeps
//! every path and line number and says exactly how much it left out.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use ush_shell::{Input, MemoryHost, Output, Shell, Stdio};

const FULL: &str = "(full: rerun with USH_RENDER=text)";

struct Ran {
    out: String,
    err: String,
    status: i32,
}

fn run_on(host: &MemoryHost, script: &str) -> Ran {
    let (stdout, out) = Output::buffer();
    let (stderr, err) = Output::buffer();
    let io = Stdio {
        stdin: Input::null(),
        stdout,
        stderr,
    };
    let env = [("HOME".to_string(), "/home/me".to_string())];
    let mut shell = Shell::new(Arc::new(host.clone()), env, PathBuf::from("/home/me"), io);
    let status = shell.run_to_exit(script);
    drop(shell);
    Ran {
        out: String::from_utf8_lossy(&out.take()).into_owned(),
        err: String::from_utf8_lossy(&err.take()).into_owned(),
        status,
    }
}

/// Run `script` for an agent.
fn agent(host: &MemoryHost, script: &str) -> Ran {
    run_on(host, &format!("USH_RENDER=agent\n{script}"))
}

fn project() -> MemoryHost {
    let host = MemoryHost::new();
    host.file("/home/me/p/a.rs", "foo\nbar\nfoo again\n")
        .file("/home/me/p/b.md", "nothing\n")
        .file("/home/me/p/two words.txt", "x\n")
        .file("/home/me/p/sub/x.rs", "bar\nfoo\n");
    host
}

/// A directory of `n` files, `f000.txt` on, each with `n` lines matching
/// `hit`.
fn many(n: usize) -> MemoryHost {
    let host = MemoryHost::new();
    let mut text = String::new();
    for i in 0..n {
        let _ = writeln!(text, "hit {i}");
    }
    for i in 0..n {
        host.file(&format!("/home/me/big/f{i:03}.txt"), text.clone());
    }
    host
}

// ls

#[test]
fn ls_for_an_agent_is_a_line_per_directory() {
    let host = project();
    let r = agent(&host, "ls p");
    assert_eq!(
        (r.out.as_str(), r.status),
        ("p/: a.rs b.md sub/ 'two words.txt'\n", 0),
        "{}",
        r.err
    );
    let r = agent(&host, "ls -R p");
    assert_eq!(
        r.out, "p/: a.rs b.md sub/ 'two words.txt'\np/sub/: x.rs\n",
        "{}",
        r.err
    );
    let r = agent(&host, "ls p/a.rs p/sub");
    assert_eq!(r.out, "p/a.rs\np/sub/: x.rs\n", "{}", r.err);
    let r = agent(&host, "ls -l p/sub");
    assert_eq!(
        r.out, "p/sub/:\n  -rw-r--r-- 8 1970-01-01 00:00 x.rs\n",
        "{}",
        r.err
    );
}

#[test]
fn ls_for_an_agent_says_exactly_how_much_it_left_out() {
    let host = many(250);
    let r = agent(&host, "ls big");
    assert_eq!(r.status, 0, "{}", r.err);
    let lines: Vec<&str> = r.out.lines().collect();
    assert_eq!(lines.len(), 2, "{}", r.out);
    let first = lines.first().copied().unwrap_or_default();
    assert!(first.starts_with("big/: f000.txt f001.txt "), "{first}");
    assert!(first.ends_with(" f199.txt +50 more"), "{first}");
    assert_eq!(
        lines.get(1).copied(),
        Some(format!("… 50 more names {FULL}").as_str())
    );
}

#[test]
fn ls_text_is_unchanged() {
    let host = project();
    let r = run_on(&host, "ls p");
    assert_eq!(r.out, "a.rs\nb.md\nsub\ntwo words.txt\n");
    let r = run_on(&host, "USH_RENDER=text ls -R p");
    assert_eq!(
        r.out,
        "p:\na.rs\nb.md\nsub\ntwo words.txt\n\np/sub:\nx.rs\n"
    );
}

// grep

#[test]
fn grep_for_an_agent_groups_matches_by_file_with_line_numbers() {
    let host = project();
    let r = agent(&host, "grep -r foo p");
    assert_eq!(
        (r.out.as_str(), r.status),
        (
            "p/a.rs\n  1: foo\n  3: foo again\np/sub/x.rs\n  2: foo\n",
            0
        ),
        "{}",
        r.err
    );
    // One file: still named, so the path survives.
    let r = agent(&host, "grep foo p/a.rs");
    assert_eq!(r.out, "p/a.rs\n  1: foo\n  3: foo again\n", "{}", r.err);
    let r = agent(&host, "grep -C1 again p/a.rs");
    assert_eq!(r.out, "p/a.rs\n  2- bar\n  3: foo again\n", "{}", r.err);
    let r = agent(&host, "grep -rl foo p");
    assert_eq!(r.out, "p/: a.rs\np/sub/: x.rs\n", "{}", r.err);
    let r = agent(&host, "grep -c foo p/a.rs p/b.md");
    assert_eq!(r.out, "p/a.rs: 2\np/b.md: 0\n", "{}", r.err);
    let r = agent(&host, "printf 'a\\nfoo\\n' | grep foo");
    assert_eq!(r.out, "2: foo\n", "{}", r.err);
}

#[test]
fn grep_for_an_agent_says_exactly_how_much_it_left_out() {
    let host = many(30);
    // 30 files of 30 matching lines: 900 lines, 200 shown.
    let r = agent(&host, "grep -r hit big");
    assert_eq!(r.status, 0, "{}", r.err);
    let shown = r.out.lines().filter(|l| l.starts_with("  ")).count();
    assert_eq!(shown, 200);
    // The 200th line is line 20 of the seventh file; that file loses ten.
    assert!(r.out.contains("big/f006.txt\n"), "{}", r.out);
    assert!(!r.out.contains("big/f007.txt"), "{}", r.out);
    assert!(
        r.out
            .ends_with(&format!("… 700 more lines in 24 files {FULL}\n")),
        "{}",
        r.out
    );
}

#[test]
fn grep_for_an_agent_cuts_a_long_line_around_its_match() {
    let host = MemoryHost::new();
    let line = format!("{}NEEDLE{}\n", "a".repeat(2000), "b".repeat(2000));
    host.file("/home/me/min.js", line);
    let r = agent(&host, "grep -n NEEDLE min.js");
    assert!(
        r.out.starts_with("min.js\n  1: …[1,925 bytes cut]"),
        "{}",
        r.out
    );
    assert!(r.out.contains("NEEDLE"), "{}", r.out);
    assert!(r.out.ends_with(" bytes cut]…\n"), "{}", r.out);
}

#[test]
fn grep_text_is_unchanged() {
    let host = project();
    let r = run_on(&host, "grep -rn foo p");
    assert_eq!(
        r.out,
        "p/a.rs:1:foo\np/a.rs:3:foo again\np/sub/x.rs:2:foo\n"
    );
    let r = run_on(&host, "USH_RENDER=text grep foo p/a.rs");
    assert_eq!(r.out, "foo\nfoo again\n");
}

// find

#[test]
fn find_for_an_agent_groups_names_by_directory() {
    let host = project();
    let r = agent(&host, "find p");
    assert_eq!(
        (r.out.as_str(), r.status),
        ("p/\np/: a.rs b.md sub/ 'two words.txt'\np/sub/: x.rs\n", 0),
        "{}",
        r.err
    );
    let r = agent(&host, "find . -name '*.rs'");
    assert_eq!(r.out, "./p/: a.rs\n./p/sub/: x.rs\n", "{}", r.err);
}

#[test]
fn find_for_an_agent_says_exactly_how_much_it_left_out() {
    let host = many(250);
    let r = agent(&host, "find big -type f");
    assert_eq!(r.status, 0, "{}", r.err);
    assert!(r.out.starts_with("big/: f000.txt "), "{}", r.out);
    assert!(
        r.out
            .ends_with(&format!(" f199.txt +50 more\n… 50 more names {FULL}\n")),
        "{}",
        r.out
    );
}

#[test]
fn find_text_is_unchanged() {
    let host = project();
    let r = run_on(&host, "find p -name '*.rs'");
    assert_eq!(r.out, "p/a.rs\np/sub/x.rs\n");
}

// diff

#[test]
fn diff_for_an_agent_is_its_hunks() {
    let host = MemoryHost::new();
    host.file("/home/me/a.txt", "one\ntwo\nthree\nfour\nfive\n")
        .file("/home/me/b.txt", "one\nTWO\nthree\nfour\nfive\nsix\n");
    let r = agent(&host, "diff a.txt b.txt");
    assert_eq!(
        (r.out.as_str(), r.status),
        (
            "--- a.txt +++ b.txt\n@@ -2 +2 @@\n-two\n+TWO\n@@ -5,0 +6 @@\n+six\n",
            1
        ),
        "{}",
        r.err
    );
    let r = agent(&host, "diff -U1 a.txt b.txt");
    assert_eq!(
        r.out,
        "--- a.txt +++ b.txt\n@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n@@ -5 +5,2 @@\n five\n+six\n",
        "{}",
        r.err
    );
}

#[test]
fn diff_for_an_agent_says_exactly_how_much_it_left_out() {
    let host = MemoryHost::new();
    let (mut a, mut b) = (String::new(), String::new());
    for i in 0..300 {
        let _ = writeln!(a, "line {i}");
        let _ = writeln!(b, "line {i}");
        if i % 2 == 0 {
            let _ = writeln!(b, "added {i}");
            let _ = writeln!(b, "and {i}");
        }
    }
    host.file("/home/me/a.txt", a).file("/home/me/b.txt", b);
    // 150 hunks of two added lines: 300 lines, 200 shown (100 hunks).
    let r = agent(&host, "diff a.txt b.txt");
    assert_eq!(r.status, 1, "{}", r.err);
    assert_eq!(
        r.out.lines().filter(|l| l.starts_with('+')).count(),
        200,
        "{}",
        r.out
    );
    assert_eq!(
        r.out.lines().filter(|l| l.starts_with("@@")).count(),
        100,
        "{}",
        r.out
    );
    assert!(r.out.contains("+added 198\n+and 198\n…"), "{}", r.out);
    assert!(
        r.out
            .ends_with(&format!("… 100 more lines in 50 hunks {FULL}\n")),
        "{}",
        r.out
    );
}

#[test]
fn diff_text_is_unchanged() {
    let host = MemoryHost::new();
    host.file("/home/me/a.txt", "one\ntwo\n")
        .file("/home/me/b.txt", "one\n2\n");
    let r = run_on(&host, "diff a.txt b.txt");
    assert_eq!(r.out, "2c2\n< two\n---\n> 2\n");
    let r = run_on(&host, "USH_RENDER=text diff -q a.txt b.txt");
    assert_eq!(r.out, "Files a.txt and b.txt differ\n");
}
