#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::Mutex;

use tilo_environment::dir_grant;
use ush_shell::io::Captured;
use ush_shell::{Input, Shell};

use super::*;

/// An environment that grants `dir` and records every action asked of it.
struct Granting {
    grants: Vec<CapOverride>,
    acted: Mutex<Vec<(FileAction, Option<Run>)>>,
}

impl Granting {
    fn new(dir: &Path) -> Arc<Self> {
        Arc::new(Self {
            grants: vec![dir_grant(dir, true).unwrap()],
            acted: Mutex::new(Vec::new()),
        })
    }

    fn acted(&self) -> Vec<(FileAction, Option<Run>)> {
        self.acted.lock().unwrap().clone()
    }
}

impl Environment for Granting {
    fn grants(&self) -> Vec<CapOverride> {
        self.grants.clone()
    }

    fn act(&self, action: FileAction, run: Option<&Run>) -> Result<Acted, String> {
        action.admit(&self.grants, None)?;
        self.acted.lock().unwrap().push((action, run.cloned()));
        Ok(Acted {
            entry: Some("entry".into()),
            operations: 1,
        })
    }
}

struct Ran {
    status: i32,
    out: String,
    err: String,
}

fn shell(environment: Arc<Granting>, cwd: &Path) -> (Shell, Captured, Captured) {
    let (stdout, out) = Output::buffer();
    let (stderr, err) = Output::buffer();
    let host = TiloHost::new(
        environment,
        Arc::new(NativeFiles),
        Arc::new(NoPrograms),
        Arc::new(Nowhere),
        stderr.clone(),
    );
    let io = Stdio {
        stdin: Input::null(),
        stdout,
        stderr,
    };
    (
        Shell::new(Arc::new(host), [], cwd.to_path_buf(), io),
        out,
        err,
    )
}

fn run(shell: &mut Shell, out: &Captured, err: &Captured, script: &str) -> Ran {
    let status = shell.run_script(script);
    Ran {
        status,
        out: String::from_utf8_lossy(&out.take()).into_owned(),
        err: String::from_utf8_lossy(&err.take()).into_owned(),
    }
}

/// A granted directory with a few files, and one beside it that is not
/// granted.
fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let granted = root.join("granted");
    std::fs::create_dir_all(granted.join("notes")).unwrap();
    std::fs::write(granted.join("notes/a.md"), "apples\npears\n").unwrap();
    std::fs::write(granted.join("notes/b.md"), "plums\n").unwrap();
    let outside = root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "the secret\n").unwrap();
    (tmp, granted, outside)
}

#[test]
fn tilos_own_commands_read_the_granted_files() {
    let (_tmp, granted, _) = fixture();
    let environment = Granting::new(&granted);
    let (mut sh, out, err) = shell(Arc::clone(&environment), &granted);
    let ran = run(
        &mut sh,
        &out,
        &err,
        "ls notes; cat notes/a.md | grep pe; grep -c . notes/b.md",
    );
    assert_eq!(ran.out, "a.md\nb.md\npears\n1\n", "{}", ran.err);
    assert_eq!(ran.status, 0);
    assert!(environment.acted().is_empty(), "reading changes nothing");
}

#[test]
fn reading_outside_the_grants_is_refused() {
    let (_tmp, granted, outside) = fixture();
    let environment = Granting::new(&granted);
    let (mut sh, out, err) = shell(environment, &granted);

    let ran = run(
        &mut sh,
        &out,
        &err,
        &format!("cat {}/secret.txt", outside.display()),
    );
    assert_ne!(ran.status, 0);
    assert!(!ran.out.contains("secret"), "{}", ran.out);
    assert!(ran.err.contains("not granted"), "{}", ran.err);

    let ran = run(&mut sh, &out, &err, &format!("ls {}", outside.display()));
    assert_ne!(ran.status, 0);
    assert!(!ran.out.contains("secret.txt"), "{}", ran.out);

    // `..` does not climb out of the grant, and neither does a link.
    let ran = run(&mut sh, &out, &err, "cat ../outside/secret.txt");
    assert!(!ran.out.contains("secret"), "{}", ran.out);
    std::os::unix::fs::symlink(outside.join("secret.txt"), granted.join("link")).unwrap();
    let ran = run(&mut sh, &out, &err, "cat link; grep secret link");
    assert!(
        !ran.out.contains("secret"),
        "a link out of a grant: {}",
        ran.out
    );
    assert!(ran.err.contains("not granted"), "{}", ran.err);
}

#[test]
fn a_redirect_is_the_environments_write_in_the_command_lines_run() {
    let (_tmp, granted, outside) = fixture();
    let environment = Granting::new(&granted);
    let (mut sh, out, err) = shell(Arc::clone(&environment), &granted);
    let line = Run {
        id: "ush-1-1".into(),
        label: "echo hi > a.txt; echo more >> a.txt".into(),
    };
    sh.set_run(Some(line.clone()));
    let ran = run(&mut sh, &out, &err, &line.label);
    assert_eq!(ran.status, 0, "{}", ran.err);
    assert!(
        !granted.join("a.txt").exists(),
        "the host writes nothing itself: the environment carries it out"
    );
    let acted = environment.acted();
    assert_eq!(
        acted,
        vec![
            (
                FileAction::Write(WriteOp::of(
                    vec![granted.join("a.txt")],
                    b"hi\n".to_vec(),
                    false
                )),
                Some(line.clone()),
            ),
            (
                FileAction::Append(AppendFileOp {
                    files: vec![granted.join("a.txt")],
                    text: "more\n".into(),
                }),
                Some(line),
            ),
        ]
    );

    // Outside the grants the redirect fails before its command runs.
    sh.set_run(None);
    let ran = run(
        &mut sh,
        &out,
        &err,
        &format!("echo x > {}/stolen.txt", outside.display()),
    );
    assert_ne!(ran.status, 0);
    assert!(ran.err.contains("not granted"), "{}", ran.err);
    assert_eq!(environment.acted().len(), 2, "nothing more was asked");

    // `set -C` makes `>` a write that only makes a new file.
    let ran = run(&mut sh, &out, &err, "set -C; echo x > notes/a.md");
    assert_ne!(ran.status, 0, "{}", ran.out);
    let ran = run(&mut sh, &out, &err, "set -C; echo x > new.md");
    assert_eq!(ran.status, 0, "{}", ran.err);
    assert!(matches!(
        environment.acted().last(),
        Some((FileAction::Write(op), None)) if op.no_clobber
    ));
}

#[test]
fn the_file_commands_are_the_environments_actions() {
    let (_tmp, granted, _) = fixture();
    let environment = Granting::new(&granted);
    let (mut sh, out, err) = shell(Arc::clone(&environment), &granted);
    let ran = run(&mut sh, &out, &err, "cp notes/a.md c.md; rm -f notes/b.md");
    assert_eq!(ran.status, 0, "{}", ran.err);
    assert!(ran.out.contains("undo it from the environment's History"));
    let verbs: Vec<&str> = environment
        .acted()
        .iter()
        .map(|(action, _)| action.verb())
        .collect();
    assert_eq!(verbs, ["copy", "remove"]);
    assert!(matches!(
        &environment.acted()[0].0,
        FileAction::Copy(op) if op.sources == [granted.join("notes/a.md")]
    ));
}

#[test]
fn a_program_without_a_place_to_run_says_which_tier_it_needs() {
    let (_tmp, granted, _) = fixture();
    let (mut sh, out, err) = shell(Granting::new(&granted), &granted);
    let ran = run(&mut sh, &out, &err, "cargo --version");
    assert_eq!(ran.status, 127);
    assert!(ran.err.contains("tier 3"), "{}", ran.err);
}
