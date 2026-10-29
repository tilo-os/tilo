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
