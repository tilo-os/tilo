//! The shell at its prompt: the line editor's keys, multi-line input,
//! recall of the person's history (from the host, and from Terminal on the
//! session's terminal), Tab completion, jobs, and the agent rendering of a
//! run an agent asked for.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use ush_shell::jobs::{Job, Signal};
use ush_shell::{
    DirEntry, FileCall, Host, Input, MemoryHost, Metadata, Output, Program, Session, Shell, Stdio,
    WriteMode,
};

const KEY: &str = "0123456789abcdef0123456789abcdef";

fn b64(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text)
}

fn unb64(text: &str) -> String {
    String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .unwrap(),
    )
    .unwrap()
}

fn home() -> MemoryHost {
    let host = MemoryHost::new().with_terminal(80);
    host.dir("/home/me");
    host
}

struct Ran {
    out: String,
    err: String,
}

impl Ran {
    /// The command lines Terminal was told ran, in order.
    fn runs(&self) -> Vec<String> {
        marked(&self.out, "C")
            .into_iter()
            .filter(|fields| !fields.iter().any(|(k, _)| k == "j"))
            .filter_map(|fields| {
                fields
                    .iter()
                    .find(|(k, _)| k == "cmd")
                    .map(|(_, v)| unb64(v))
            })
            .collect()
    }

    /// What was printed, marks taken out.
    fn printed(&self) -> String {
        let mut pieces = self.out.split("\x1b]133;");
        let mut out = pieces.next().unwrap_or_default().to_owned();
        for piece in pieces {
            out.push_str(piece.split_once('\x07').map_or("", |(_, rest)| rest));
        }
        out
    }
}

/// The marks of kind `kind` in `out`, each as its fields.
fn marked(out: &str, kind: &str) -> Vec<Vec<(String, String)>> {
    let start = format!("{kind};");
    out.split("\x1b]133;")
        .skip(1)
        .filter_map(|piece| piece.split_once('\x07'))
        .filter_map(|(body, _)| body.strip_prefix(&start))
        .map(|body| {
            body.split(';')
                .map(|f| match f.split_once('=') {
                    Some((k, v)) => (k.to_owned(), v.to_owned()),
                    None => (String::new(), f.to_owned()),
                })
                .collect()
        })
        .collect()
}

/// An interactive session on `host` reading `keys` until they run out.
fn typed_on(host: &MemoryHost, keys: &str) -> Ran {
    let (stdout, out) = Output::buffer();
    let (stderr, err) = Output::buffer();
    let io = Stdio {
        stdin: Input::bytes(keys.as_bytes().to_vec()),
        stdout,
        stderr,
    };
    let env = [
        ("HOME".to_string(), "/home/me".to_string()),
        ("PS1".to_string(), "$ ".to_string()),
    ];
    let mut shell = Shell::new(Arc::new(host.clone()), env, PathBuf::from("/home/me"), io);
    shell.interactive(&Session {
        runs_key: Some(KEY.into()),
    });
    drop(shell);
    Ran {
        out: String::from_utf8_lossy(&out.take()).into_owned(),
        err: String::from_utf8_lossy(&err.take()).into_owned(),
    }
}

#[test]
fn emacs_keys_edit_the_line_before_it_runs() {
    let host = home();
    let ran = typed_on(
        &host,
        concat!(
            "cho hi\x01e\x05 there\r",                 // Ctrl+A, Ctrl+E
            "echo one two\x17three\r",                 // Ctrl+W
            "echo abc\x1b[D\x1b[D\x0b\r",              // Left, Left, Ctrl+K
            "echo x y\x15echo kept\r",                 // Ctrl+U
            "echo first second\x1bb\x1bb\x1bdthird\r", // Alt+B, Alt+B, Alt+D
            "world\x1b[Hecho hello \x1b[F!\r",         // Home, End
            "echo ab\x08\x7fz\r",                      // Backspace twice
            "echo gone\x1f\x1f\x1f\x1f\x1fne\r",       // Ctrl+_ undoes the typing
        ),
    );
    assert_eq!(
        ran.runs(),
        [
            "echo hi there",
            "echo one three",
            "echo a",
            "echo kept",
            "echo third second",
            "echo hello world!",
            "echo z",
            "ne",
        ],
        "{:?}",
        ran.err
    );
    assert!(ran.printed().contains("hi there"), "{:?}", ran.printed());
    // Switched on and off once to see there is a terminal, then for each
    // of the eight lines and the end of the input.
    assert_eq!(
        host.editing(),
        (false, 20),
        "the terminal goes back after each line"
    );
}

#[test]
fn ctrl_c_drops_the_line_and_ctrl_d_ends_the_session() {
    let ran = typed_on(&home(), "echo never\x03echo after\r\x04");
    assert_eq!(ran.runs(), ["echo after"]);
    assert!(ran.err.contains("^C"));
}

#[test]
fn open_quotes_backslashes_blocks_and_pastes_take_more_lines() {
    let ran = typed_on(
        &home(),
        concat!(
            "echo 'a\rb'\r",
            "echo one \\\rtwo\r",
            "if true\rthen echo yes\rfi\r",
            "\x1b[200~echo p1\r\necho p2\x1b[201~\r",
        ),
    );
    assert_eq!(
        ran.runs(),
        [
            "echo 'a\nb'",
            "echo one \\\ntwo",
            "if true\nthen echo yes\nfi",
            "echo p1\necho p2"
        ]
    );
    let printed = ran.printed();
    for line in ["a\r\nb", "one two", "yes", "p1\r\np2"] {
        assert!(
            printed
                .replace("\r\n", "\n")
                .contains(&line.replace("\r\n", "\n")),
            "{line:?} in {printed:?}"
        );
    }
    assert!(ran.err.contains("> "), "the continuation prompt");
}

#[test]
fn up_recalls_the_persons_history_newest_first_after_this_shells_own() {
    let host = home().with_history(&["echo oldest", "echo older"]);
    let ran = typed_on(
        &host,
        concat!(
            "\x1b[A\r",             // Up: the newest of the person's
            "\x1b[A\x1b[A\x1b[A\r", // this shell's line, then the person's
            "\x1b[A\x1b[A\x1b[B\r", // Down goes back one
        ),
    );
    assert_eq!(
        ran.runs(),
        ["echo older", "echo oldest", "echo oldest"],
        "{:?}",
        ran.err
    );
}

#[test]
fn ctrl_r_finds_a_line_by_what_it_holds() {
    let host = home().with_history(&["echo apple", "echo banana", "echo apricot"]);
    let ran = typed_on(&host, "\x12ap\r\x12ap\x12\r\x12zzz\x07\r");
    assert_eq!(ran.runs(), ["echo apricot", "echo apple"]);
    assert!(ran.err.contains("(reverse-i-search)'ap': "));
}

#[test]
fn history_lists_the_persons_lines_numbered_oldest_first() {
    let host = home().with_history(&["echo one", "echo two"]);
    let ran = typed_on(&host, "history\r");
    assert!(
        ran.printed()
            .contains("    1  echo one\n    2  echo two\n    3  history\n"),
        "{:?}",
        ran.printed()
    );
    let ran = typed_on(&host, "history -c\r");
    assert!(ran.err.contains("only reads it"), "{}", ran.err);
}

/// The input side of a session whose terminal answers the editor's history
/// questions the way Terminal does.
struct Keys(Receiver<Vec<u8>>, VecDeque<u8>);

impl Read for Keys {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.1.is_empty() {
            match self.0.recv() {
                Ok(bytes) => self.1.extend(bytes),
                Err(_) => return Ok(0),
            }
        }
        let mut count = 0_usize;
        for slot in buf.iter_mut() {
            let Some(byte) = self.1.pop_front() else {
                break;
            };
            *slot = byte;
            count = count.saturating_add(1);
        }
        Ok(count)
    }
}

/// The screen side: it answers each history question from `history`
/// (newest first) on the input, two lines at a time, as Terminal pages.
struct Answering {
    seen: Arc<Mutex<Vec<u8>>>,
    history: Vec<String>,
    keys: Sender<Vec<u8>>,
    asked: Arc<Mutex<Vec<String>>>,
}

impl Write for Answering {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut seen = self.seen.lock().unwrap();
        seen.extend_from_slice(buf);
        let text = String::from_utf8_lossy(&seen).into_owned();
        drop(seen);
        let start = format!("\x1b]133;H;k={KEY};");
        if let Some((_, after)) = text.split_once(&start)
            && let Some((body, rest)) = after.split_once('\x07')
        {
            *self.seen.lock().unwrap() = rest.as_bytes().to_vec();
            let field = |name: &str| {
                body.split(';')
                    .find_map(|f| f.strip_prefix(&format!("{name}=")).map(str::to_owned))
                    .unwrap()
            };
            let skip: usize = field("s").parse().unwrap();
            let q = unb64(&field("q"));
            self.asked.lock().unwrap().push(format!("{skip}:{q}"));
            let matching: Vec<&String> = self.history.iter().filter(|l| l.contains(&q)).collect();
            let page: Vec<String> = matching.iter().skip(skip).take(2).map(|l| b64(l)).collect();
            let end = skip.saturating_add(page.len()) >= matching.len();
            let answer = format!(
                "\x1b]133;H;k={KEY};s={skip};q={};e={}{}\x07",
                field("q"),
                page.join(","),
                if end { ";end=1" } else { "" }
            );
            let _ = self.keys.send(answer.into_bytes());
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn without_a_history_of_its_own_the_host_lets_terminal_answer() {
    let host = home();
    let (keys_tx, keys_rx) = channel();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let (stdout, out) = Output::buffer();
    let io = Stdio {
        stdin: Input::new(Keys(keys_rx, VecDeque::new())),
        stdout,
        stderr: Output::new(Answering {
            seen: Arc::default(),
            history: ["echo c", "echo b", "echo a"].map(String::from).to_vec(),
            keys: keys_tx.clone(),
            asked: Arc::clone(&asked),
        }),
    };
    let shell_host = host;
    let session = std::thread::spawn(move || {
        let mut shell = Shell::new(
            Arc::new(shell_host),
            [("PS1".to_string(), "$ ".to_string())],
            PathBuf::from("/home/me"),
            io,
        );
        shell.interactive(&Session {
            runs_key: Some(KEY.into()),
        });
    });
    // Up three times, past the first page of two; then Enter at once,
    // before the answer: the line waits for it.
    keys_tx.send(b"\x1b[A\x1b[A\x1b[A\r".to_vec()).unwrap();
    keys_tx.send(b"\x12b\r".to_vec()).unwrap();
    keys_tx.send(b"exit\r".to_vec()).unwrap();
    session.join().unwrap();
    let ran = Ran {
        out: String::from_utf8_lossy(&out.take()).into_owned(),
        err: String::new(),
    };
    assert_eq!(ran.runs(), ["echo a", "echo b"]);
    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked[..2], ["0:".to_owned(), "2:".to_owned()], "{asked:?}");
    assert!(asked.contains(&"0:b".to_owned()), "{asked:?}");
}

#[test]
fn tab_completes_commands_paths_variables_and_lists_on_a_second_tab() {
    let host = home().with_programs(&["cargo"]);
    host.file("/home/me/src/main.rs", "fn main() {}\n")
        .dir("/home/me/src/macros")
        .file("/home/me/my notes.md", "hello\n");
    let ran = typed_on(
        &host,
        concat!(
            "ech\t hi\r",
            "cat src/mai\t\r",
            "cat my\t\r",
            "echo $HO\t\r",
            "carg\t\x15echo \r",
            "grep --line-n\t x src/main.rs\r",
            "ls src/ma\t\t\x15\r",
        ),
    );
    assert_eq!(
        ran.runs(),
        [
            "echo  hi",
            "cat src/main.rs ",
            "cat my\\ notes.md ",
            "echo $HOME ",
            "echo ",
            "grep --line-number  x src/main.rs",
        ],
        "{:?}",
        ran.err
    );
    let printed = ran.printed();
    assert!(printed.contains("fn main() {}"));
    assert!(printed.contains("hello"));
    assert!(printed.contains("/home/me"));
    assert!(
        ran.err.contains("macros/  main.rs"),
        "a second Tab lists: {:?}",
        ran.err
    );
}

/// Run `script` (not interactively) on `host`, answering what it printed.
fn script_on(host: &MemoryHost, script: &str) -> (String, String, i32) {
    let (stdout, out) = Output::buffer();
    let (stderr, err) = Output::buffer();
    let io = Stdio {
        stdin: Input::null(),
        stdout,
        stderr,
    };
    let mut shell = Shell::new(
        Arc::new(host.clone()),
        [("HOME".to_string(), "/home/me".to_string())],
        PathBuf::from("/home/me"),
        io,
    );
    let status = shell.run_to_exit(script);
    drop(shell);
    (
        String::from_utf8_lossy(&out.take()).into_owned(),
        String::from_utf8_lossy(&err.take()).into_owned(),
        status,
    )
}

#[test]
fn background_jobs_run_beside_the_shell_and_wait_waits_for_them() {
    let host = home().with_real_time();
    let (out, err, status) = script_on(
        &host,
        "(sleep 0.3; echo late) &\necho early\njobs\nwait\necho waited $?",
    );
    assert_eq!(status, 0, "{err}");
    assert_eq!(
        out, "early\n[1]+  Running                 ( sleep 0.3; echo late ) &\nlate\nwaited 0\n",
        "{err}"
    );
}

#[test]
fn kill_ends_a_job_and_fg_waits_for_one() {
    let host = home().with_real_time();
    let started = Instant::now();
    let (out, err, _) = script_on(
        &host,
        "sleep 30 &\nkill %1\nwait %1\necho killed $?\nsleep 0.1 &\nfg %1\necho fg $?\necho $!",
    );
    assert!(started.elapsed() < Duration::from_secs(10), "kill ended it");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.first().copied(), Some("killed 130"), "{out} {err}");
    assert_eq!(lines.get(1).copied(), Some("fg 0"), "{out} {err}");
    assert!(err.contains("sleep 0.1"), "fg says which job: {err}");
    assert!(lines.get(2).is_some_and(|pid| pid.parse::<u32>().is_ok()));
}

/// A host whose one program, `serve`, runs until the shell ends it, and
/// which records the signals the shell's job control sent it.
#[derive(Clone)]
struct Guest {
    files: MemoryHost,
    signals: Arc<Mutex<Vec<(Signal, bool)>>>,
}

impl Host for Guest {
    fn metadata(&self, path: &Path) -> std::io::Result<Metadata> {
        self.files.metadata(path)
    }
    fn read_dir(&self, path: &Path) -> std::io::Result<Vec<DirEntry>> {
        self.files.read_dir(path)
    }
    fn open_read(&self, path: &Path) -> std::io::Result<Box<dyn Read + Send>> {
        self.files.open_read(path)
    }
    fn open_write(
        &self,
        path: &Path,
        mode: WriteMode,
        run: Option<&ush_shell::host::Run>,
    ) -> std::io::Result<Box<dyn Write + Send>> {
        self.files.open_write(path, mode, run)
    }
    fn file_action(&self, call: &FileCall<'_>) -> i32 {
        self.files.file_action(call)
    }
    fn program(&self, _: &[String], _: &[(String, String)], _: &Path, _: &Stdio) -> Program {
        Program::NotFound
    }
    fn program_in_job(
        &self,
        argv: &[String],
        _: &[(String, String)],
        _: &Path,
        io: &Stdio,
        job: &Arc<Job>,
    ) -> Program {
        if argv.first().map(String::as_str) != Some("serve") {
            return Program::NotFound;
        }
        let (tx, rx) = channel();
        let signals = Arc::clone(&self.signals);
        let seen = Arc::clone(job);
        let tx = Mutex::new(tx);
        let _attached = job.attach(move |signal| {
            signals.lock().unwrap().push((signal, seen.foreground()));
            let _ = tx.lock().unwrap().send(signal);
        });
        self.files.file("/home/me/serving", "");
        let _ = io.stdout.write_str("serving\n");
        loop {
            match rx.recv() {
                Ok(Signal::Terminate | Signal::Kill | Signal::Interrupt) | Err(_) => {
                    return Program::Exited(143);
                }
                // A stop the shell took back before it was seen is no stop.
                Ok(Signal::Stop) => match rx.try_recv() {
                    Ok(Signal::Continue) => {}
                    Ok(Signal::Terminate | Signal::Kill | Signal::Interrupt) => {
                        return Program::Exited(143);
                    }
                    _ => job.stopped(),
                },
                Ok(Signal::Continue) => {}
            }
        }
    }
}

#[test]
fn a_program_in_the_guest_is_stopped_continued_and_ended_as_its_job() {
    let guest = Guest {
        files: home(),
        signals: Arc::default(),
    };
    let (stdout, out) = Output::buffer();
    let (stderr, err) = Output::buffer();
    let io = Stdio {
        stdin: Input::null(),
        stdout,
        stderr,
    };
    let mut shell = Shell::new(
        Arc::new(guest.clone()),
        [("HOME".to_string(), "/home/me".to_string())],
        PathBuf::from("/home/me"),
        io,
    );
    let status = shell.run_to_exit(concat!(
        "serve &\n",
        "until test -e serving; do :; done\n",
        "kill -STOP %1\n",
        "jobs\n",
        "kill -CONT %1\n",
        "kill %1\n",
        "wait %1\n",
        "echo ended $?\n",
    ));
    drop(shell);
    let out = String::from_utf8_lossy(&out.take()).into_owned();
    let err = String::from_utf8_lossy(&err.take()).into_owned();
    assert_eq!(status, 0, "{err}");
    assert!(out.contains("[1]+  Stopped                 serve"), "{out}");
    assert!(out.ends_with("ended 143\n"), "{out}");
    let signals = guest.signals.lock().unwrap().clone();
    assert_eq!(
        signals,
        [
            (Signal::Stop, false),
            (Signal::Continue, false),
            (Signal::Terminate, false)
        ]
    );
}

#[test]
fn ctrl_z_stops_the_line_and_fg_brings_it_back() {
    let host = home().with_real_time();
    let suspend = host.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        suspend.suspend();
    });
    let ran = typed_on(&host, "sleep 0.6; echo slept\rjobs\rfg\rjobs\r");
    let printed = ran.printed();
    assert!(
        ran.err
            .contains("[1]+  Stopped                 sleep 0.6; echo slept"),
        "{:?}",
        ran.err
    );
    assert!(
        printed.contains("[1]+  Stopped"),
        "jobs lists it: {printed:?}"
    );
    assert!(
        printed.contains("slept"),
        "fg ran it to its end: {printed:?}"
    );
    let ended: Vec<String> = marked(&ran.out, "D")
        .into_iter()
        .map(|f| f[0].1.clone())
        .collect();
    assert_eq!(ended, ["148", "0", "0", "0"], "{ended:?}");
}

#[test]
fn a_background_job_has_a_run_of_its_own() {
    let host = home();
    host.file("/home/me/a", "x");
    let ran = typed_on(&host, "rm a & wait\recho after\r");
    let started = marked(&ran.out, "C");
    let job: Vec<_> = started
        .iter()
        .filter(|f| f.iter().any(|(k, v)| k == "j" && v == "1"))
        .collect();
    assert_eq!(job.len(), 1, "{started:?}");
    assert!(job[0].iter().any(|(k, v)| k == "cmd" && unb64(v) == "rm a"));
    let ended = marked(&ran.out, "D");
    assert!(ended.iter().any(|f| f.iter().any(|(k, _)| k == "j")));
    let runs = host.action_runs();
    assert_eq!(runs.len(), 1);
    assert!(runs[0].ends_with("-job-1"), "{runs:?}");
    assert_eq!(ran.runs(), ["rm a & wait", "echo after"]);
}

#[test]
fn a_run_an_agent_asked_for_gets_the_agent_rendering() {
    let host = home();
    for i in 0..3 {
        host.file(&format!("/home/me/p/f{i}.txt"), "");
    }
    let asked = |agent: bool| {
        format!(
            "\x1b]133;R;k={KEY};cmd={}{}\x07",
            b64("ls -R p"),
            if agent { ";agent=1" } else { "" }
        )
    };
    let person = typed_on(&host, &asked(false));
    let agent = typed_on(&host, &asked(true));
    assert_eq!(agent.runs(), ["ls -R p"]);
    assert!(
        person.printed().contains("p:\nf0.txt\nf1.txt\nf2.txt"),
        "{:?}",
        person.printed()
    );
    assert!(
        agent.printed().contains("p/: f0.txt f1.txt f2.txt"),
        "{:?}",
        agent.printed()
    );
    // A forged request, without the key, is not run.
    let forged = typed_on(&host, &asked(true).replace(KEY, "forged"));
    assert!(forged.runs().is_empty());
}
