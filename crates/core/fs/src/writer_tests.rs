use super::*;
use tilo_test_env::SpawnUnderEnvLock;

/// A process table under a temporary `/proc`.
struct Procs {
    dir: tempfile::TempDir,
}

impl Procs {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("proc")).unwrap();
        Self { dir }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("proc")
    }

    /// Process `pid`, child of `parent`, started `start` ticks after boot, with a terminal
    /// when `tty`, and the cgroup path `cgroup`.
    fn spawn(&self, pid: u32, parent: u32, start: u64, tty: bool, cgroup: &str) {
        let dir = self.root().join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let tty = if tty { 34816 } else { 0 };
        std::fs::write(
            dir.join("stat"),
            format!("{pid} (odd (name) x) S {parent} {pid} {pid} {tty} -1 4194304 0 0 0 0 0 0 0 0 20 0 1 0 {start} 0 0"),
        )
        .unwrap();
        std::fs::write(dir.join("cgroup"), format!("0::{cgroup}\n")).unwrap();
    }

    fn resolver(&self) -> WriterResolver {
        let applications = self.dir.path().join("applications");
        std::fs::create_dir_all(&applications).unwrap();
        std::fs::write(
            applications.join("org.gnome.TextEditor.desktop"),
            "[Desktop Entry]\nName=Text Editor\n[Desktop Action new]\nName=New\n",
        )
        .unwrap();
        WriterResolver::new(self.root(), vec![applications])
    }
}

tilo_test_env::scenario! {
    fn a_write_is_named_for_the_app_or_person_its_process_acts_for;

    phase apps_are_named_by_their_scope;
    {
        let procs = Procs::new();
        let slice = "/user.slice/user-1000.slice/user@1000.service/app.slice";
        procs.spawn(600, 1, 1, false, &format!("{slice}/app-tilo-so.tilo.notes-4242.scope"));
        procs.spawn(601, 1, 1, false, &format!("{slice}/app-tilo-so.tilo.my\\x2dapp-17.scope"));
        procs.spawn(602, 1, 1, false, &format!("{slice}/app-gnome-org.gnome.TextEditor-9876.scope"));
        procs.spawn(603, 1, 1, false, &format!("{slice}/app-flatpak-org.mozilla.firefox-12345.scope"));
        procs.spawn(604, 1, 1, false, &format!("{slice}/app-org.kde.kate@abc123.service"));
        // A command in a terminal of a desktop app's scope is the person's.
        procs.spawn(605, 1, 1, true, &format!("{slice}/app-gnome-org.gnome.Ptyxis-1.scope"));
        procs.spawn(606, 1, 1, false, "/user.slice/user-1000.slice/session-2.scope");
        let resolver = procs.resolver();
        assert_eq!(resolver.resolve(600), Writer::App { id: "so.tilo.notes".into() });
        let notes = resolver.resolve(600).invoker(Some("ad")).unwrap();
        // The person writes through the app, with their own access.
        assert_eq!(notes.acting.person_id(), Some("ad"));
        assert!(notes.acting.app().is_some_and(|app| app.as_str() == "so.tilo.notes"));
        assert_eq!(resolver.resolve(601), Writer::App { id: "so.tilo.my-app".into() });
        assert_eq!(
            resolver.resolve(602),
            Writer::Program { id: "org.gnome.TextEditor".into(), name: "Text Editor".into() }
        );
        assert_eq!(
            resolver.resolve(603),
            Writer::Program { id: "org.mozilla.firefox".into(), name: "firefox".into() }
        );
        assert_eq!(resolver.resolve(604), Writer::Program { id: "org.kde.kate".into(), name: "kate".into() });
        assert_eq!(resolver.resolve(605), Writer::Person);
        assert_eq!(resolver.resolve(606), Writer::Person);
        assert_eq!(resolver.resolve(999), Writer::Person, "a process that is gone");
    }

    phase a_label_is_bounded_and_presentable;
    {
        let writer = Writer::Program {
            id: "org.example.Editor".into(),
            name: format!("a\u{7}{}", "é".repeat(200)),
        };
        let name = writer.invoker(None).unwrap().name.unwrap();
        assert!(name.len() <= MAX_TEXT_BYTES);
        assert!(!name.contains('\u{7}'));
    }
}

/// A process whose cgroup cannot be read (it waits for the mount, as a process faulting in a page
/// of a file mapped from it does) is answered for at once, as the person, and is not remembered:
/// once its cgroup reads, its next request finds the app it acts for.
#[test]
fn a_process_that_does_not_read_never_holds_up_a_request() {
    let procs = Procs::new();
    let notes =
        "/user.slice/user-1000.slice/user@1000.service/app.slice/app-tilo-so.tilo.notes-7.scope";
    procs.spawn(700, 1, 7, false, notes);
    let cgroup = procs.root().join("700/cgroup");
    std::fs::remove_file(&cgroup).unwrap();
    let made = std::process::Command::new("mkfifo")
        .arg(&cgroup)
        .status_locked()
        .unwrap();
    assert!(made.success());
    let resolver = procs.resolver();

    let started = std::time::Instant::now();
    assert_eq!(resolver.resolve(700), Writer::Person);
    assert_eq!(
        resolver.resolve(700),
        Writer::Person,
        "a read still waiting starts no other"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the requests waited {:?}",
        started.elapsed()
    );

    // The cgroup reads now: the waiting read ends, and the next request reads it again.
    let line = format!("0::{notes}\n");
    let mut fifo = std::fs::OpenOptions::new()
        .write(true)
        .open(&cgroup)
        .unwrap();
    std::io::Write::write_all(&mut fifo, line.as_bytes()).unwrap();
    drop(fifo);
    tilo_test_env::wait_until("the waiting read to end", || {
        !resolver.procs.waiting.load(Ordering::Acquire)
    });
    std::fs::remove_file(&cgroup).unwrap();
    std::fs::write(&cgroup, line).unwrap();
    assert_eq!(
        resolver.resolve(700),
        Writer::App {
            id: "so.tilo.notes".into()
        }
    );
}
