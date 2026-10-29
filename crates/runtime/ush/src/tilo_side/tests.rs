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
        Arc::new(RecordEnvironment::new(Arc::clone(&state))),
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
    let env = [("PATH".to_owned(), "/usr/bin:/bin".to_owned())];
    let shell = Shell::new(Arc::new(host), env, allowed.clone(), io);
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
