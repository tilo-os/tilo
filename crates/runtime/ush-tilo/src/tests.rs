#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::Mutex;

use tilo_environment::dir_grant;
use ush_shell::io::Captured;
use ush_shell::{Input, Shell};

use super::*;

/// An environment that grants `dir`, records every action asked of it, and
/// keeps content in memory.
struct Granting {
    grants: Vec<CapOverride>,
    acted: Mutex<Vec<(FileAction, Option<Run>)>>,
    kept: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>,
}

impl Granting {
    fn new(dir: &Path) -> Arc<Self> {
        Arc::new(Self {
            grants: vec![dir_grant(dir, true).unwrap()],
            acted: Mutex::new(Vec::new()),
            kept: Arc::default(),
        })
    }

    fn kept(&self, hash: &str) -> Option<Vec<u8>> {
        self.kept.lock().unwrap().get(hash).cloned()
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

    fn keep(&self) -> io::Result<Box<dyn Keeping>> {
        Ok(Box::new(Keeper {
            bytes: Vec::new(),
            kept: Arc::clone(&self.kept),
        }))
    }
}

struct Keeper {
    bytes: Vec<u8>,
    kept: Arc<Mutex<std::collections::HashMap<String, Vec<u8>>>>,
}

impl Write for Keeper {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Keeping for Keeper {
    fn finish(self: Box<Self>) -> io::Result<Kept> {
        let hash = blake3::hash(&self.bytes).to_hex().to_string();
        self.kept.lock().unwrap().insert(hash.clone(), self.bytes);
        Ok(Kept { hash, hold: None })
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
                    content: None,
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

/// Past what an action carries inside it, a redirect, `tee` and `sort -o`
/// stream what they write into the content store, and are still one action
/// each, naming it by reference.
#[test]
fn a_write_of_any_size_is_one_action_naming_its_content() {
    let (_tmp, granted, _) = fixture();
    let line = "0123456789abcdef".repeat(8);
    let lines = 2 * MAX_TEXT_BYTES / line.len();
    let big = format!("{line}\n").repeat(lines);
    std::fs::write(granted.join("big.txt"), &big).unwrap();
    let environment = Granting::new(&granted);
    let (mut sh, out, err) = shell(Arc::clone(&environment), &granted);
    let ran = run(
        &mut sh,
        &out,
        &err,
        "cat big.txt > copy.txt; cat big.txt >> log.txt; cat big.txt | tee teed.txt > /dev/null; \
         sort -o sorted.txt big.txt; printf '\\377\\376' >> bin.dat",
    );
    assert_eq!(ran.status, 0, "{}", ran.err);
    let acted = environment.acted();
    let named = |name: &str| {
        acted
            .iter()
            .filter(|(action, _)| action.writes() == [granted.join(name)])
            .map(|(action, _)| action.clone())
            .collect::<Vec<_>>()
    };
    for (name, want) in [
        ("copy.txt", big.clone()),
        ("teed.txt", big.clone()),
        ("sorted.txt", big.clone()),
    ] {
        let actions = named(name);
        assert_eq!(actions.len(), 1, "{name}: one action, {actions:?}");
        let FileAction::Write(op) = &actions[0] else {
            panic!("{name}: {:?}", actions[0]);
        };
        assert!(op.text.is_empty(), "{name}: nothing inline");
        let hash = op.content.as_deref().expect("by reference");
        assert_eq!(environment.kept(hash).unwrap(), want.as_bytes(), "{name}");
        actions[0].admit(&environment.grants, None).unwrap();
    }
    let appended = named("log.txt");
    assert!(matches!(
        appended.as_slice(),
        [FileAction::Append(op)] if op.text.is_empty()
            && environment.kept(op.content.as_deref().unwrap()).unwrap() == big.as_bytes()
    ));
    let binary = named("bin.dat");
    assert!(
        matches!(
            binary.as_slice(),
            [FileAction::Append(op)] if environment.kept(op.content.as_deref().unwrap()).unwrap() == [0xff, 0xfe]
        ),
        "bytes that are not text are appended by reference: {binary:?}"
    );
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

/// A place that holds the person's history and has a terminal, with one
/// program to run.
struct Holding;

impl Place for Holding {
    fn edit_mode(&self, _: bool) -> bool {
        true
    }

    fn history(&self, query: &HistoryQuery) -> Option<Vec<String>> {
        let lines = ["echo from the record", "echo older"];
        Some(
            lines
                .iter()
                .filter(|line| line.contains(&query.matching))
                .skip(query.skip)
                .take(query.limit)
                .map(|line| (*line).to_owned())
                .collect(),
        )
    }
}

struct Frobnicate;

impl Programs for Frobnicate {
    fn run(
        &self,
        argv: &[String],
        _: &[(String, String)],
        _: &Path,
        io: &Stdio,
        _: Option<&dyn Fn() -> bool>,
    ) -> Program {
        let _ = io.stdout.write_str(&format!("ran {}\n", argv.join(" ")));
        Program::Exited(0)
    }

    fn names(&self, _: &str) -> Vec<String> {
        vec!["frobnicate".into()]
    }
}

/// At the prompt the host is the place's: Up recalls the history the place
/// holds, with no question to Terminal, and Tab completes the names of the
/// programs the place runs, which run as the line's job.
#[test]
fn the_prompt_recalls_the_places_history_and_completes_its_programs() {
    let (_tmp, granted, _) = fixture();
    let (stdout, out) = Output::buffer();
    let (stderr, _err) = Output::buffer();
    let host = TiloHost::new(
        Granting::new(&granted),
        Arc::new(NativeFiles),
        Arc::new(Frobnicate),
        Arc::new(Holding),
        stderr.clone(),
    );
    let io = Stdio {
        stdin: Input::bytes(b"\x1b[A\rfrob\t--now\r".to_vec()),
        stdout,
        stderr,
    };
    let mut shell = Shell::new(Arc::new(host), [], granted, io);
    shell.interactive(&ush_shell::Session {
        runs_key: Some("0123456789abcdef0123456789abcdef".into()),
    });
    drop(shell);
    let out = String::from_utf8_lossy(&out.take()).into_owned();
    assert!(
        !out.contains("\x1b]133;H;"),
        "Terminal is not asked: {out:?}"
    );
    assert!(out.contains("from the record\n"), "{out:?}");
    assert!(out.contains("ran frobnicate --now\n"), "{out:?}");
}
