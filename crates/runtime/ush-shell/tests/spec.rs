//! POSIX and bash conformance: cases from the Oils project's shell spec
//! tests, run against this shell.
//!
//! The cases under `tests/spec/` are a chosen subset of
//! <https://github.com/oils-for-unix/oils/tree/master/spec> (Apache-2.0),
//! in their own format: `#### name`, the script, then what it prints and
//! the status it ends with, as bash gives them. Each runs in a fresh shell on
//! a host with files in memory, where `argv.py` prints its arguments as a
//! Python list, as the suite's own helper does.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ush_shell::{Host, MemoryHost, Output, Program, Shell, Stdio};

/// The memory host, plus the suite's helper programs.
struct SpecHost(MemoryHost);

impl Host for SpecHost {
    fn metadata(&self, path: &Path) -> std::io::Result<ush_shell::Metadata> {
        self.0.metadata(path)
    }
    fn symlink_metadata(&self, path: &Path) -> std::io::Result<ush_shell::Metadata> {
        self.0.symlink_metadata(path)
    }
    fn read_dir(&self, path: &Path) -> std::io::Result<Vec<ush_shell::DirEntry>> {
        self.0.read_dir(path)
    }
    fn open_read(&self, path: &Path) -> std::io::Result<Box<dyn std::io::Read + Send>> {
        self.0.open_read(path)
    }
    fn open_write(
        &self,
        path: &Path,
        mode: ush_shell::WriteMode,
        run: Option<&ush_shell::Run>,
    ) -> std::io::Result<Box<dyn std::io::Write + Send>> {
        self.0.open_write(path, mode, run)
    }
    fn file_action(&self, call: &ush_shell::FileCall<'_>) -> i32 {
        self.0.file_action(call)
    }
    fn program(&self, argv: &[String], env: &[(String, String)], _: &Path, io: &Stdio) -> Program {
        match argv.first().map(String::as_str) {
            Some("argv.py") => {
                let items: Vec<String> = argv.iter().skip(1).map(|a| py_repr(a)).collect();
                let _ = io.stdout.write_str(&format!("[{}]\n", items.join(", ")));
                Program::Exited(0)
            }
            Some("printenv.py") => {
                for name in argv.iter().skip(1) {
                    let value = env.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
                    let _ = io
                        .stdout
                        .write_str(&format!("{}\n", value.unwrap_or("None")));
                }
                Program::Exited(0)
            }
            _ => Program::NotFound,
        }
    }
    fn now(&self) -> i64 {
        self.0.now()
    }
    fn home_of(&self, name: &str) -> Option<PathBuf> {
        (name == "root").then(|| PathBuf::from("/root"))
    }
}

/// A string as Python 2's `repr` writes it, which is what the suite's
/// expected output holds.
fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for b in s.bytes() {
        match b {
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\t' => out.push_str("\\t"),
            b'\r' => out.push_str("\\r"),
            b if b == quote as u8 => {
                out.push('\\');
                out.push(quote);
            }
            0x20..=0x7e => out.push(char::from(b)),
            other => {
                let _ = write!(out, "\\x{other:02x}");
            }
        }
    }
    out.push(quote);
    out
}

#[derive(Debug, Default)]
struct Case {
    file: String,
    name: String,
    code: String,
    stdout: Option<String>,
    status: i32,
}

/// Read a spec file: each case's script and what bash gives for it.
fn parse(file: &str, text: &str) -> Vec<Case> {
    let mut cases = Vec::new();
    let mut lines = text.lines();
    let mut current: Option<Case> = None;
    while let Some(line) = lines.next() {
        if let Some(name) = line.strip_prefix("#### ") {
            cases.extend(current.take());
            current = Some(Case {
                file: file.into(),
                name: name.trim().into(),
                ..Case::default()
            });
            continue;
        }
        let Some(case) = current.as_mut() else {
            continue;
        };
        let Some(directive) = line.strip_prefix("## ") else {
            if case.stdout.is_none() {
                case.code.push_str(line);
                case.code.push('\n');
            }
            continue;
        };
        if let Some(value) = directive.strip_prefix("stdout: ") {
            case.stdout = Some(format!("{value}\n"));
        } else if let Some(value) = directive.strip_prefix("stdout-json: ") {
            case.stdout = Some(json_string(value));
        } else if directive == "STDOUT:" {
            let mut block = String::new();
            for l in lines.by_ref() {
                if l == "## END" {
                    break;
                }
                block.push_str(l);
                block.push('\n');
            }
            case.stdout = Some(block);
        } else if let Some(value) = directive.strip_prefix("status: ") {
            case.status = value.trim().parse().unwrap_or(0);
        }
    }
    cases.extend(current);
    cases
}

/// A JSON string literal's value.
fn json_string(raw: &str) -> String {
    let inner = raw.trim().trim_start_matches('"').trim_end_matches('"');
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                out.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

fn run(case: &Case) -> (String, i32) {
    let host = MemoryHost::new();
    host.dir("/tmp/_tmp").dir("/home/spec");
    let (stdout, captured) = Output::buffer();
    let io = Stdio {
        stdin: ush_shell::Input::null(),
        stdout,
        stderr: Output::null(),
    };
    let env = [
        ("HOME".to_string(), "/home/spec".to_string()),
        ("TMP".to_string(), "/tmp".to_string()),
        ("SH".to_string(), "ush".to_string()),
    ];
    let mut shell = Shell::new(Arc::new(SpecHost(host)), env, PathBuf::from("/tmp"), io);
    let status = shell.run_to_exit(&case.code);
    drop(shell);
    (
        String::from_utf8_lossy(&captured.take()).into_owned(),
        status,
    )
}

/// Run a case on a thread of its own, so a case that never ends is a
/// failure with its name rather than a test run that never ends.
fn run_bounded(case: &Case) -> Option<(String, i32)> {
    let (tx, rx) = std::sync::mpsc::channel();
    let code = Case {
        file: case.file.clone(),
        name: case.name.clone(),
        code: case.code.clone(),
        stdout: None,
        status: 0,
    };
    std::thread::spawn(move || {
        let _ = tx.send(run(&code));
    });
    rx.recv_timeout(std::time::Duration::from_secs(10)).ok()
}

fn spec_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/spec")
}

#[test]
fn the_chosen_spec_cases_all_pass() {
    let mut failures = Vec::new();
    let mut ran = 0_usize;
    let mut files: Vec<_> = std::fs::read_dir(spec_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "sh"))
        .collect();
    files.sort();
    for path in files {
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(&path).unwrap();
        for case in parse(&file, &text) {
            ran += 1;
            let Some((stdout, status)) = run_bounded(&case) else {
                failures.push(format!(
                    "{} / {}: did not finish\n--- script\n{}",
                    case.file, case.name, case.code
                ));
                continue;
            };
            let expected = case.stdout.clone().unwrap_or_default();
            if stdout != expected || status != case.status {
                failures.push(format!(
                    "{} / {}\n--- script\n{}--- expected (status {})\n{}--- got (status {status})\n{}",
                    case.file, case.name, case.code, case.status, expected, stdout
                ));
            }
        }
    }
    eprintln!("spec: {ran} cases ran, {} failed", failures.len());
    assert!(ran > 100, "only {ran} spec cases were found");
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
