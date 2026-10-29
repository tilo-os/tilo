//! The Tilo-side shell over a real environment record: reads under its
//! grants with no guest, a redirect as its action in History, and programs
//! in a guest started once for the session.
//!
//! The guest here is the program server ([`crate::guest_exec::serve`]) on a
//! thread of this process, not a `bwrap` sandbox, so these run where no
//! guest can; `tests/serve_handoff.rs` runs the same session in a real one.

use std::path::PathBuf;

use ush_shell::{Shell, Std};
use ush_tilo::TiloHost;

use super::*;
use crate::file_actions::tests::{brokered_state, entries_for, undo_from_history};

fn null() -> OwnedFd {
    std::fs::File::open("/dev/null").unwrap().into()
}

/// A guest that is the program server on a thread, counting how often it
/// was started.
fn local_guest() -> Box<Launch> {
    Box::new(|| {
        let (control, served) = UnixStream::pair()?;
        let served = OwnedFd::from(served);
        std::thread::spawn(move || crate::guest_exec::serve(&served));
        Ok(RunningGuest::new(control, Vec::new()))
    })
}

struct Fixture {
    _dir: tempfile::TempDir,
    allowed: PathBuf,
    outside: PathBuf,
    state: Arc<Mutex<BrokerState>>,
    guest: Arc<SessionGuest>,
    shell: Shell,
    out: ush_shell::io::Captured,
    err: ush_shell::io::Captured,
}

/// The Tilo-side shell of the environment `state` brokers, with `env` and
/// starting in `cwd`, its programs in a local guest.
fn session_shell(
    state: &Arc<Mutex<BrokerState>>,
    env: Vec<(String, String)>,
    cwd: PathBuf,
) -> (
    Arc<SessionGuest>,
    Shell,
    ush_shell::io::Captured,
    ush_shell::io::Captured,
) {
    let place = Arc::new(DevicePlace::install().unwrap());
    let guest = Arc::new(SessionGuest::new(
        local_guest(),
        Box::new(|| false),
        SessionStreams {
            stdin: null(),
            stdout: null(),
            stderr: null(),
        },
        Arc::clone(&place),
    ));
    let (stdout, out) = Output::buffer();
    let (stderr, err) = Output::buffer();
    let host = TiloHost::new(
        Arc::new(RecordEnvironment::new(Arc::clone(state))),
        Arc::new(ush_tilo::NativeFiles),
        Arc::clone(&guest) as Arc<dyn Programs>,
        place,
        stderr.clone(),
    );
    let io = Stdio {
        stdin: Input::null(),
        stdout,
        stderr,
    };
    let shell = Shell::new(Arc::new(host), env, cwd, io);
    (guest, shell, out, err)
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let state = Arc::new(Mutex::new(brokered_state(&root)));
    let allowed = root.join("allowed");
    std::fs::create_dir_all(allowed.join("notes")).unwrap();
    std::fs::write(allowed.join("notes/a.md"), "apples\npears\n").unwrap();
    std::fs::write(allowed.join("notes/b.md"), "plums\n").unwrap();
    let outside = root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "the secret\n").unwrap();

    let env = [("PATH".to_owned(), "/usr/bin:/bin".to_owned())];
    let (guest, shell, out, err) = session_shell(&state, env.to_vec(), allowed.clone());
    Fixture {
        _dir: dir,
        allowed,
        outside,
        state,
        guest,
        shell,
        out,
        err,
    }
}

impl Fixture {
    fn run(&mut self, script: &str) -> (i32, String, String) {
        let status = self.shell.run_script(script);
        (
            status,
            String::from_utf8_lossy(&self.out.take()).into_owned(),
            String::from_utf8_lossy(&self.err.take()).into_owned(),
        )
    }
}

#[test]
fn ls_cat_and_grep_read_the_granted_files_with_no_guest_started() {
    let mut f = fixture();
    let (status, out, err) = f.run("ls notes; cat notes/a.md | grep pe; grep -c . notes/*.md");
    assert_eq!(status, 0, "{err}");
    assert_eq!(
        out, "a.md\nb.md\npears\nnotes/a.md:2\nnotes/b.md:1\n",
        "{err}"
    );
    assert_eq!(f.guest.starts(), 0, "Tilo's own commands need no guest");
}

#[test]
fn reading_outside_the_grants_is_refused() {
    let mut f = fixture();
    let (status, out, err) = f.run(&format!(
        "cat {0}/secret.txt; ls {0}; grep -r secret {0}",
        f.outside.display()
    ));
    assert_ne!(status, 0);
    assert!(!out.contains("secret"), "{out}");
    assert!(err.contains("not granted"), "{err}");
    assert_eq!(f.guest.starts(), 0);
}

#[test]
fn a_redirect_is_an_environment_action_in_history_and_undone_from_there() {
    let mut f = fixture();
    let line = Run {
        id: "ush-1-1".into(),
        label: "echo hi > a.txt".into(),
    };
    f.shell.set_run(Some(line.clone()));
    let (status, _, err) = f.run(&line.label);
    f.shell.set_run(None);
    assert_eq!(status, 0, "{err}");
    let written = f.allowed.join("a.txt");
    assert_eq!(std::fs::read_to_string(&written).unwrap(), "hi\n");

    let state = f.state.lock().unwrap();
    let writes = entries_for(&state, "write");
    assert_eq!(writes.len(), 1, "one redirect is one History entry");
    let group = writes[0].group.as_ref().expect("in the command line's run");
    assert_eq!(
        (group.id.as_str(), group.label.as_str()),
        ("ush-1-1", "echo hi > a.txt")
    );
    assert_eq!(
        writes[0].invoker.as_ref().map(|invoker| &invoker.actor),
        Some(&state.person),
        "attributed to the person"
    );
    undo_from_history(&state, &writes[0]);
    assert!(!written.exists(), "undo takes the new file back");
    drop(state);
    assert_eq!(f.guest.starts(), 0, "writing needs no guest either");
}

#[test]
fn a_program_starts_the_guest_once_for_the_session_and_its_output_is_piped_back() {
    let mut f = fixture();
    let (status, out, err) = f.run("uname -s | tr a-z A-Z");
    assert_eq!(status, 0, "{err}");
    assert_eq!(out, "LINUX\n");
    assert_eq!(f.guest.starts(), 1);

    // The same guest serves the next ones: the server is their parent.
    let (status, out, err) = f.run("sh -c 'echo $PPID'; sh -c 'echo $PPID; exit 4'");
    assert_eq!(status, 4, "{err}");
    let parents: Vec<&str> = out.lines().collect();
    assert_eq!(parents.len(), 2, "{out}");
    assert_eq!(parents[0], parents[1], "one guest serves the session");
    assert_eq!(f.guest.starts(), 1, "started once for the session");

    // A program reads what the shell pipes in, and one the guest does not
    // have is not found.
    let (_, out, _) = f.run("printf 'b\\na\\n' | sh -c 'read x; echo got $x'");
    assert_eq!(out, "got b\n");
    let (status, _, err) = f.run("no-such-program-here");
    assert_eq!(status, 127);
    assert!(err.contains("command not found"), "{err}");
}

#[test]
fn a_program_on_the_terminal_gets_the_sessions_own_descriptor() {
    let f = fixture();
    let shared = f.guest.stream(Some(Std::Out)).unwrap();
    assert!(shared.1.is_none(), "the terminal itself, no pipe");
    let piped = f.guest.stream(None).unwrap();
    assert!(
        piped.1.is_some(),
        "anything else is a pipe the shell serves"
    );
}

/// A redirect far past what an action carries inside it is still one
/// action: its content streamed into the environment's content store, one
/// History entry in the command line's run, undone from there.
#[test]
fn a_ten_megabyte_redirect_is_one_undoable_history_entry() {
    let mut f = fixture();
    let line = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde\n";
    let big = line.repeat(10 * 1024 * 1024 / line.len());
    assert!(big.len() >= 10 * 1000 * 1000);
    std::fs::write(f.allowed.join("big.log"), &big).unwrap();
    let copy = f.allowed.join("copy.log");
    std::fs::write(&copy, "what was there\n").unwrap();

    let run = Run {
        id: "ush-1-2".into(),
        label: "cat big.log > copy.log".into(),
    };
    f.shell.set_run(Some(run.clone()));
    let (status, _, err) = f.run(&run.label);
    f.shell.set_run(None);
    assert_eq!(status, 0, "{err}");
    assert!(
        std::fs::read(&copy).unwrap() == big.as_bytes(),
        "the whole of it was written"
    );

    let state = f.state.lock().unwrap();
    let writes = entries_for(&state, "write");
    assert_eq!(writes.len(), 1, "one redirect is one History entry");
    assert_eq!(
        writes[0].group.as_ref().map(|group| group.id.as_str()),
        Some("ush-1-2")
    );
    let named = crate::content::named(&state.environment.current());
    assert_eq!(named.len(), 1, "the action names its content by reference");
    let content = crate::file_actions::content_in(&state.environment);
    assert!(content.store().has(named.first().unwrap()));

    undo_from_history(&state, &writes[0]);
    drop(state);
    assert_eq!(
        std::fs::read_to_string(&copy).unwrap(),
        "what was there\n",
        "undo puts back what the file held"
    );
}

/// `~` is the workspace's folder: an environment opened where the
/// workspace is gets it as its home, granted, and `cd ~` reaches it.
#[test]
fn cd_home_reaches_the_workspace_folder() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let store = crate::store::Store::at(root.join("store")).with_workspace(&workspace);
    let state = crate::broker::tests::state(&root);
    let environment = crate::home::with_home(
        &state.environment.records,
        &store,
        &state.environment.key,
        state.environment.current(),
    )
    .unwrap();
    assert_eq!(Path::new(&environment.home_dir), workspace);
    let home = crate::home::home_of(&environment, &crate::guest::GuestUser::current());
    assert_eq!(home, workspace);

    let state = Arc::new(Mutex::new(state));
    let env = vec![
        ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
        ("HOME".to_owned(), home.display().to_string()),
    ];
    let (_guest, mut shell, out, err) = session_shell(&state, env, root);
    let status = shell.run_script("cd ~ && echo x > f && pwd");
    let err = String::from_utf8_lossy(&err.take()).into_owned();
    assert_eq!(status, 0, "{err}");
    assert_eq!(
        String::from_utf8_lossy(&out.take()),
        format!("{}\n", workspace.display())
    );
    assert_eq!(std::fs::read_to_string(workspace.join("f")).unwrap(), "x\n");
    let state = state.lock().unwrap();
    assert_eq!(
        entries_for(&state, "write").len(),
        1,
        "an action, in History"
    );
}

/// An environment from before `~` was the workspace's folder gets it
/// without losing anything: its tools and grants stay, and what its old
/// home in the store holds stays where it was, readable there.
#[test]
fn an_existing_environment_gets_home_without_losing_anything() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let store = crate::store::Store::at(root.join("store")).with_workspace(&workspace);
    let state = brokered_state(&root);
    let old_home = store.environment_home(&state.environment.key);
    std::fs::create_dir_all(old_home.join(".config")).unwrap();
    std::fs::write(old_home.join(".bashrc"), "alias ll='ls -l'\n").unwrap();
    std::fs::write(old_home.join(".config/tool.toml"), "kept = true\n").unwrap();
    let before = state.environment.current();
    assert!(before.home_dir.is_empty(), "made before homes");

    let after = crate::home::with_home(
        &state.environment.records,
        &store,
        &state.environment.key,
        before.clone(),
    )
    .unwrap();
    assert_eq!(Path::new(&after.home_dir), workspace);
    assert_eq!(after.tools, before.tools);
    assert_eq!(after.runtime, before.runtime);
    assert!(
        before
            .grants
            .iter()
            .all(|grant| after.grants.contains(grant)),
        "every grant it held is kept"
    );
    assert_eq!(
        std::fs::read_to_string(old_home.join(".bashrc")).unwrap(),
        "alias ll='ls -l'\n",
        "nothing moved"
    );
    assert!(old_home.join(".config/tool.toml").is_file());
    assert!(
        std::fs::read_dir(&workspace).unwrap().next().is_none(),
        "nothing copied into the workspace either"
    );
    // Opening it again changes nothing more.
    let again = crate::home::with_home(
        &state.environment.records,
        &store,
        &state.environment.key,
        after.clone(),
    )
    .unwrap();
    assert_eq!(again, after);

    // The old home reads where it is, and is not written through.
    let state = Arc::new(Mutex::new(state));
    let env = vec![("HOME".to_owned(), after.home_dir)];
    let (_guest, mut shell, out, err) = session_shell(&state, env, workspace);
    let status = shell.run_script(&format!(
        "cat {0}/.bashrc {0}/.config/tool.toml",
        old_home.display()
    ));
    assert_eq!(status, 0, "{}", String::from_utf8_lossy(&err.take()));
    assert_eq!(
        String::from_utf8_lossy(&out.take()),
        "alias ll='ls -l'\nkept = true\n"
    );
    let status = shell.run_script(&format!("rm {}/.bashrc", old_home.display()));
    assert_ne!(
        status,
        0,
        "read-only: {}",
        String::from_utf8_lossy(&err.take())
    );
    assert!(old_home.join(".bashrc").is_file());
}

// The shell at its prompt on a terminal, as Terminal runs it.

const KEY: &str = "0123456789abcdef0123456789abcdef";

fn b64(text: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(text)
}

fn open_pty(cols: u16) -> (std::fs::File, std::fs::File) {
    let (mut master, mut slave) = (-1, -1);
    let size = libc::winsize {
        ws_row: 24,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: both fds are out-parameters `openpty` fills in; `size` is a
    // properly initialised `winsize`.
    let rc = unsafe {
        libc::openpty(
            &raw mut master,
            &raw mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw const size,
        )
    };
    assert_eq!(rc, 0, "{}", std::io::Error::last_os_error());
    // SAFETY: `openpty` succeeded, so both are owned, open descriptors.
    unsafe {
        (
            std::fs::File::from_raw_fd(master),
            std::fs::File::from_raw_fd(slave),
        )
    }
}

/// Play Terminal on the pty's `master`: keep what the shell shows on
/// `screen`, and answer each of the line editor's history questions with
/// the whole of `history`, as one last page, noting `skip:query` in
/// `asked`.
fn play_terminal(
    master: &std::fs::File,
    history: &[&str],
    screen: Arc<Mutex<String>>,
    asked: Arc<Mutex<Vec<String>>>,
) {
    let mut reader = master.try_clone().unwrap();
    let mut writer = master.try_clone().unwrap();
    let history: Vec<String> = history.iter().map(|l| b64(l)).collect();
    std::thread::spawn(move || {
        use std::io::{Read as _, Write as _};
        let ask = format!("\x1b]133;H;k={KEY};");
        let mut buf = [0_u8; 4096];
        let mut pending = String::new();
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            let text = String::from_utf8_lossy(buf.get(..n).unwrap_or_default()).into_owned();
            screen.lock().unwrap().push_str(&text);
            pending.push_str(&text);
            while let Some((_, rest)) = pending.split_once(&ask)
                && let Some((fields, after)) = rest.split_once('\x07')
            {
                let field = |name: &str| {
                    fields
                        .split(';')
                        .find_map(|f| f.strip_prefix(name))
                        .unwrap_or("")
                        .to_owned()
                };
                let (skip, q) = (field("s="), field("q="));
                asked.lock().unwrap().push(format!("{skip}:{q}"));
                let entries = if skip == "0" {
                    history.join(",")
                } else {
                    String::new()
                };
                let answer = format!("\x1b]133;H;k={KEY};s={skip};q={q};e={entries};end=1\x07");
                writer.write_all(answer.as_bytes()).unwrap();
                pending = after.to_owned();
            }
        }
    });
}

/// Whether the terminal `fd` is taking keys one at a time, unechoed.
fn raw(fd: &std::fs::File) -> bool {
    // SAFETY: tcgetattr writes only into the termios we own.
    unsafe {
        let mut now: libc::termios = std::mem::zeroed();
        assert_eq!(libc::tcgetattr(fd.as_raw_fd(), &raw mut now), 0);
        now.c_lflag & (libc::ICANON | libc::ECHO) == 0
    }
}

/// A Tilo-side shell interactive on a pty: keys go in on the master, and
/// what the shell draws on the terminal comes out of it. Terminal is
/// played by a thread that answers the line editor's history questions
/// with `history`, newest first.
struct Prompt {
    _dir: tempfile::TempDir,
    root: PathBuf,
    master: std::fs::File,
    slave: std::fs::File,
    screen: Arc<Mutex<String>>,
    asked: Arc<Mutex<Vec<String>>>,
    session: Option<std::thread::JoinHandle<i32>>,
    out: Arc<Mutex<Vec<u8>>>,
    host: TiloHost,
}

/// What the shell's standard output collected, readable while it runs.
struct Tee(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn prompt(history: &[&str]) -> Prompt {
    prompt_with(history, None)
}

/// [`prompt`], with Terminal's native editor capability served at the
/// prompt when `field` is given.
fn prompt_with(history: &[&str], field: Option<&Arc<SessionField>>) -> Prompt {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let state = Arc::new(Mutex::new(brokered_state(&root)));
    let allowed = root.join("allowed");
    std::fs::create_dir_all(allowed.join("notes")).unwrap();
    std::fs::write(allowed.join("notes/plan.md"), "plan\n").unwrap();
    let (master, slave) = open_pty(100);
    let dup = |file: &std::fs::File| OwnedFd::from(file.try_clone().unwrap());
    let mut place = DevicePlace::install().unwrap().on_terminal(dup(&slave));
    if let Some(field) = field {
        place = place.with_field(Arc::clone(field));
    }
    let place = Arc::new(place);
    let guest = Arc::new(SessionGuest::new(
        local_guest(),
        Box::new(|| false),
        SessionStreams {
            stdin: dup(&slave),
            stdout: null(),
            stderr: dup(&slave),
        },
        Arc::clone(&place),
    ));
    let out = Arc::new(Mutex::new(Vec::new()));
    let stdout = Output::new(Tee(Arc::clone(&out)));
    let stderr = Output::host(Std::Err, slave.try_clone().unwrap());
    let host = TiloHost::new(
        Arc::new(RecordEnvironment::new(Arc::clone(&state))),
        Arc::new(ush_tilo::NativeFiles),
        guest as Arc<dyn Programs>,
        place,
        stderr.clone(),
    );
    let io = Stdio {
        stdin: match field {
            Some(field) => Input::host(field.keys(slave.try_clone().unwrap())),
            None => Input::host(slave.try_clone().unwrap()),
        },
        stdout,
        stderr,
    };
    let env = [
        ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
        ("PS1".to_owned(), "$ ".to_owned()),
    ];
    let shell_host: Arc<dyn ush_shell::Host> = Arc::new(host.clone());
    let session = std::thread::spawn(move || {
        let mut shell = Shell::new(shell_host, env, allowed, io);
        shell.interactive(&ush_shell::Session {
            runs_key: Some(KEY.into()),
        })
    });
    let screen = Arc::new(Mutex::new(String::new()));
    let asked = Arc::new(Mutex::new(Vec::new()));
    play_terminal(&master, history, Arc::clone(&screen), Arc::clone(&asked));
    let prompt = Prompt {
        _dir: dir,
        root,
        master,
        slave,
        screen,
        asked,
        session: Some(session),
        out,
        host,
    };
    prompt.wait_for("$ ");
    prompt
}

impl Prompt {
    fn type_keys(&self, keys: &str) {
        use std::io::Write as _;
        (&self.master).write_all(keys.as_bytes()).unwrap();
    }

    /// Wait until the terminal shows `text`.
    fn wait_for(&self, text: &str) {
        let asked = std::time::Instant::now();
        while !self.screen.lock().unwrap().contains(text) {
            assert!(
                asked.elapsed() < std::time::Duration::from_secs(20),
                "the terminal never showed {text:?}: {:?}",
                self.screen.lock().unwrap()
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Wait until the shell printed `text` on its standard output.
    fn wait_out(&self, text: &str) {
        let asked = std::time::Instant::now();
        while !String::from_utf8_lossy(&self.out.lock().unwrap()).contains(text) {
            assert!(
                asked.elapsed() < std::time::Duration::from_secs(20),
                "the shell never printed {text:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Type `exit` and wait for the session to end: the command lines
    /// Terminal was told ran, and what they printed.
    fn exit(mut self) -> (Vec<String>, String) {
        self.type_keys("exit\r");
        let status = self.session.take().unwrap().join().unwrap();
        assert_eq!(status, 0);
        let out = String::from_utf8_lossy(&self.out.lock().unwrap()).into_owned();
        let runs = out
            .split("\x1b]133;C;")
            .skip(1)
            .filter_map(|mark| mark.split('\x07').next())
            .filter(|mark| !mark.contains(";j="))
            .filter_map(|mark| {
                use base64::Engine as _;
                let cmd = mark.split(';').find_map(|f| f.strip_prefix("cmd="))?;
                let bytes = base64::engine::general_purpose::STANDARD.decode(cmd).ok()?;
                String::from_utf8(bytes).ok()
            })
            .collect();
        let printed = out
            .split("\x1b]133;")
            .enumerate()
            .map(|(i, piece)| {
                if i == 0 {
                    piece
                } else {
                    piece.split_once('\x07').map_or("", |(_, rest)| rest)
                }
            })
            .collect();
        (runs, printed)
    }
}

/// At the prompt the session's terminal takes keys one at a time and
/// unechoed, the line editor's emacs keys edit the line, and the terminal
/// is given back as it was for what the line runs.
#[test]
fn the_prompt_edits_on_the_sessions_terminal_in_raw_mode() {
    let p = prompt(&[]);
    assert!(raw(&p.slave), "the prompt has the terminal raw");
    assert_eq!(ush_shell::Host::columns(&p.host), 100, "as wide as the pty");
    // Ctrl+A, Ctrl+E; Alt+B, Alt+D; Ctrl+W.
    p.type_keys("cho hi\x01e\x05 there\r");
    p.wait_out("hi there");
    p.type_keys("echo first second\x1bb\x1bb\x1bdthird\r");
    p.type_keys("echo one two\x17three\r");
    let (runs, printed) = p.exit();
    assert_eq!(
        runs,
        ["echo hi there", "echo third second", "echo one three"],
        "{printed}"
    );
    assert_eq!(printed, "hi there\nthird second\none three\n");
}

/// Up recalls the person's history, which Terminal holds and answers on
/// the session's terminal; Ctrl+R searches it; Tab completes a path the
/// grants show.
#[test]
fn up_and_ctrl_r_recall_the_persons_history_and_tab_completes_paths() {
    let p = prompt(&["echo newest", "echo older", "echo oldest"]);
    p.type_keys("\x1b[A\x1b[A\r");
    p.wait_out("older\n");
    p.type_keys("\x12oldest\r");
    p.wait_out("oldest\n");
    p.type_keys("cat notes/pl\t\r");
    p.wait_out("plan\n");
    let asked = p.asked.lock().unwrap().clone();
    assert!(asked.contains(&"0:".to_owned()), "{asked:?}");
    let (runs, printed) = p.exit();
    assert_eq!(
        runs,
        ["echo older", "echo oldest", "cat notes/plan.md "],
        "{printed}"
    );
}

/// A program that stops itself (as Ctrl+Z on its terminal does) stops the
/// line it ran in: the prompt comes back, `jobs` lists it stopped, and `fg`
/// continues it in its process group to its end.
#[test]
fn a_program_stopped_in_the_guest_gives_the_prompt_back_and_fg_continues_it() {
    let p = prompt(&[]);
    p.type_keys("sh -c 'kill -TSTP $$; echo resumed'\r");
    p.wait_for("Stopped");
    assert!(raw(&p.slave), "the prompt has the terminal again");
    p.type_keys("jobs\r");
    p.type_keys("fg\r");
    p.wait_out("resumed");
    let (_, printed) = p.exit();
    assert!(printed.contains("[1]+  Stopped"), "{printed}");
    assert!(printed.ends_with("resumed\n"), "{printed}");
}

/// Ctrl+Z that reaches the shell (the program does not hold the
/// terminal) stops the job in the foreground, and its program with it.
#[test]
fn ctrl_z_that_reaches_the_shell_stops_the_foreground_program() {
    let p = prompt(&[]);
    p.type_keys("sh -c 'echo $$ > pid; echo started >&2; sleep 30'\r");
    p.wait_for("started");
    // SAFETY: the shell's own handler takes it (DevicePlace::install).
    unsafe { libc::kill(libc::getpid(), libc::SIGTSTP) };
    p.wait_for("Stopped");
    let pid = std::fs::read_to_string(p.root.join("allowed/pid")).unwrap();
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid.trim())).unwrap();
    let state = stat.rsplit(')').next().unwrap().split_whitespace().next();
    assert_eq!(state, Some("T"), "the program stopped: {stat}");
    p.type_keys("kill %1; wait %1; echo ended $?\r");
    let (_, printed) = p.exit();
    assert!(printed.ends_with("ended 143\n"), "{printed}");
}

/// A background job's program is stopped, continued and ended through its
/// process group in the guest.
#[test]
fn a_background_program_is_stopped_continued_and_ended_as_its_job() {
    let mut f = fixture();
    let (status, out, err) = f.run(concat!(
        "sh -c 'echo $$ > pid; exec sleep 30' &\n",
        "until test -s pid; do sleep 0.02; done\n",
        "kill -STOP %1\n",
        "sh -c 'cut -d\" \" -f3 /proc/$(cat pid)/stat'\n",
        "jobs\n",
        "kill -CONT %1\n",
        "jobs\n",
        "kill %1\n",
        "wait %1\n",
        "echo ended $?\n",
    ));
    assert_eq!(status, 0, "{err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.first(), Some(&"T"), "{out}");
    assert!(out.contains("Stopped"), "{out}");
    assert!(out.contains("Running"), "{out}");
    assert!(out.ends_with("ended 143\n"), "{out}");
}

/// Tab offers the programs of the images the guest is built from, along
/// `PATH`.
#[test]
fn the_guests_programs_are_named_from_its_images_along_path() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let runtime = dir.path().join("runtime");
    let tool = dir.path().join("tool");
    for (root, name) in [(&runtime, "bash"), (&tool, "rg"), (&runtime, "ls")] {
        std::fs::create_dir_all(root.join("usr/bin")).unwrap();
        let file = root.join("usr/bin").join(name);
        std::fs::write(&file, "").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(runtime.join("usr/bin/README"), "").unwrap();
    std::os::unix::fs::symlink("bash", runtime.join("usr/bin/sh")).unwrap();
    let names = programs_in(&[tool, runtime], "/run/ush/bin:/usr/bin:relative");
    assert_eq!(names, ["bash", "ls", "rg", "sh"]);
}

/// What Terminal's client was answered, for the test to wait on.
struct Answered(std::sync::mpsc::Sender<Result<(), tilo_process::editor::Failure>>);

impl tilo_process::editor::Completion for Answered {
    fn send(
        &self,
        send: &mut dyn FnMut() -> std::io::Result<()>,
    ) -> Result<(), tilo_process::editor::Failure> {
        send().map_err(|_| tilo_process::editor::Failure::Retired)
    }

    fn finish(self: Box<Self>, result: Result<(), tilo_process::editor::Failure>) {
        let _ = self.0.send(result);
    }
}

/// Terminal's native editing and dictation at a ush prompt: Terminal's own
/// client of the editor capability reads the line's lease, puts text in at
/// the cursor as if typed, takes it back and makes it again; an edit of a
/// line that changed since is refused, and Enter runs what the line holds.
#[test]
fn terminals_editor_client_edits_the_line_at_a_ush_prompt() {
    use runtime_portal::editor::{Request, Status};
    use tilo_process::editor::{Client, Failure, Snapshot};

    let (app, producer) = runtime_portal::editor::Socket::pair().unwrap();
    let field = SessionField::serve(Arc::new(producer)).unwrap();
    let p = prompt_with(&[], Some(&field));
    let client = Client::new(app, Arc::new(|| {})).unwrap();
    let lease_at = |at: u32| -> Snapshot {
        let asked = std::time::Instant::now();
        loop {
            if let Some(snapshot) = client.snapshot()
                && snapshot.lease.focus == at
            {
                return snapshot;
            }
            assert!(
                asked.elapsed() < std::time::Duration::from_secs(20),
                "no lease with the cursor at {at}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    };
    let submit = |request: Request, expected: Status| {
        let (tx, rx) = std::sync::mpsc::channel();
        client.submit(client.epoch(), request, expected, Box::new(Answered(tx)));
        rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap()
    };

    p.type_keys("echo hel");
    let typed = lease_at(8);
    assert_eq!(
        submit(
            Request::Replace {
                lease: typed.lease,
                transaction: 1,
                text: "lo wörld".into(),
            },
            Status::Applied,
        ),
        Ok(())
    );
    p.wait_for("lo wörld");
    let applied = lease_at(17);
    assert!(applied.can_undo, "the edit can be taken back");
    assert_eq!(
        submit(
            Request::Undo {
                lease: applied.lease.clone(),
                transaction: applied.transaction,
            },
            Status::Undone,
        ),
        Ok(())
    );
    let undone = lease_at(8);
    assert!(undone.can_redo, "and made again");
    assert_eq!(
        submit(
            Request::Redo {
                lease: undone.lease.clone(),
                transaction: undone.transaction,
            },
            Status::Redone,
        ),
        Ok(())
    );
    lease_at(17);

    // A key makes the line the person's again: the old lease is stale.
    p.type_keys("!");
    lease_at(18);
    assert_eq!(
        submit(
            Request::Replace {
                lease: applied.lease,
                transaction: 1,
                text: "x".into(),
            },
            Status::Applied,
        ),
        Err(Failure::Rejected)
    );

    p.type_keys("\r");
    p.wait_out("hello wörld!");
    drop(client);
    let (runs, printed) = p.exit();
    assert_eq!(runs, ["echo hello wörld!"], "{printed}");
}
