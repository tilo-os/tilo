use super::*;

/// A process table under a temporary `/proc`.
struct Procs {
    dir: tempfile::TempDir,
}

impl Procs {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("proc")).unwrap();
        std::fs::write(
            dir.path().join("proc/stat"),
            "cpu 1 2 3\nbtime 1700000000\n",
        )
        .unwrap();
        Self { dir }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("proc")
    }

    /// Process `pid`, child of `parent`, started `start` ticks after boot, with a terminal
    /// when `tty`, the environment `env`, and the cgroup path `cgroup`.
    fn spawn(
        &self,
        pid: u32,
        parent: u32,
        start: u64,
        tty: bool,
        env: &[(&str, &str)],
        cgroup: &str,
    ) {
        let dir = self.root().join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        let tty = if tty { 34816 } else { 0 };
        std::fs::write(
            dir.join("stat"),
            format!("{pid} (odd (name) x) S {parent} {pid} {pid} {tty} -1 4194304 0 0 0 0 0 0 0 0 20 0 1 0 {start} 0 0"),
        )
        .unwrap();
        let mut environ = Vec::new();
        for (name, value) in env {
            environ.extend_from_slice(format!("{name}={value}").as_bytes());
            environ.push(0);
        }
        std::fs::write(dir.join("environ"), environ).unwrap();
        std::fs::write(dir.join("cgroup"), format!("0::{cgroup}\n")).unwrap();
    }

    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    fn claude_session(&self, pid: u32, json: &str) {
        let dir = self.home().join(".claude/sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{pid}.json")), json).unwrap();
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

const TERMINAL: &str = "/user.slice/user-1000.slice/user@1000.service/app.slice/vte-spawn-1.scope";

tilo_test_env::scenario! {
    fn a_write_is_named_for_the_session_app_or_person_its_process_acts_for;

    phase claude_codes_own_process_is_its_session_with_its_title;
    {
        let procs = Procs::new();
        let home = procs.home();
        let home = home.to_str().unwrap();
        procs.spawn(100, 1, 10, true, &[("HOME", home)], TERMINAL);
        // Claude Code's own process: its environment names no session.
        procs.spawn(200, 100, 20, true, &[("HOME", home)], TERMINAL);
        procs.claude_session(
            200,
            r#"{"pid":200,"sessionId":"4f2c","cwd":"/w","name":"Tidy the notes","kind":"interactive","startedAt":1700000000300}"#,
        );
        let resolver = procs.resolver();
        let writer = resolver.resolve(200);
        assert_eq!(
            writer,
            Writer::Agent { harness: Harness::ClaudeCode, session: "4f2c".into(), title: Some("Tidy the notes".into()) }
        );
        let attribution = writer.attribution().unwrap();
        assert_eq!(attribution.actor, "agent:claude:4f2c");
        assert_eq!(attribution.label, "Tidy the notes");
        assert_eq!(attribution.validate(), Ok(()));
        // FUSE names the thread that writes, here one of Claude Code's.
        let thread = procs.root().join("201");
        std::fs::create_dir_all(&thread).unwrap();
        std::fs::write(thread.join("status"), "Name:\tclaude\nTgid:\t200\n").unwrap();
        assert_eq!(resolver.resolve(201), writer);

        // A command it runs (its Bash tool), down a shell: the same session, though its
        // environment names the id Claude Code started with, not the one it holds now.
        procs.spawn(300, 200, 30, false, &[("HOME", home), ("CLAUDE_CODE_SESSION_ID", "0ld")], TERMINAL);
        procs.spawn(301, 300, 31, false, &[("HOME", home), ("CLAUDE_CODE_SESSION_ID", "0ld")], TERMINAL);
        assert_eq!(resolver.resolve(301), writer);

        // The person's own shell beside it is the person.
        procs.spawn(400, 100, 40, true, &[("HOME", home)], TERMINAL);
        assert_eq!(resolver.resolve(400), Writer::Person);
        assert_eq!(Writer::Person.attribution(), None);
    }

    phase an_agent_started_inside_another_is_its_own_session;
    {
        let procs = Procs::new();
        let home = procs.home();
        let home = home.to_str().unwrap();
        procs.spawn(200, 1, 20, true, &[("HOME", home), ("PI_SESSION_ID", "p1")], TERMINAL);
        procs.claude_session(200, r#"{"sessionId":"c1"}"#);
        // pi's id inherited from the pi Claude Code was started in: still Claude Code's.
        procs.spawn(210, 200, 21, false, &[("HOME", home), ("PI_SESSION_ID", "p1")], TERMINAL);
        let resolver = procs.resolver();
        assert_eq!(
            resolver.resolve(210),
            Writer::Agent { harness: Harness::ClaudeCode, session: "c1".into(), title: None }
        );
        assert_eq!(resolver.resolve(210).attribution().unwrap().label, "Claude Code");
        // A pi or a Codex started from Claude Code's commands sets an id of its own.
        procs.spawn(220, 200, 22, false, &[("HOME", home), ("PI_SESSION_ID", "p2")], TERMINAL);
        assert_eq!(resolver.resolve(220), Writer::Agent { harness: Harness::Pi, session: "p2".into(), title: None });
        procs.spawn(230, 200, 23, false, &[("HOME", home), ("PI_SESSION_ID", "p1"), ("CODEX_THREAD_ID", "t9")], TERMINAL);
        assert_eq!(resolver.resolve(230), Writer::Agent { harness: Harness::Codex, session: "t9".into(), title: None });
    }

    phase without_claude_codes_file_the_environment_names_the_session_most_specific_first;
    {
        let procs = Procs::new();
        let home = procs.home();
        let home = home.to_str().unwrap();
        procs.spawn(10, 1, 1, false, &[("HOME", home), ("CLAUDE_CODE_SESSION_ID", "c7")], TERMINAL);
        procs.spawn(11, 1, 1, false, &[("HOME", home), ("CODEX_THREAD_ID", "t1"), ("CLAUDE_CODE_SESSION_ID", "c7")], TERMINAL);
        procs.spawn(12, 1, 1, false, &[("HOME", home), ("PI_SESSION_ID", "p1"), ("CLAUDE_CODE_SESSION_ID", "c7")], TERMINAL);
        let resolver = procs.resolver();
        assert_eq!(resolver.resolve(10), Writer::Agent { harness: Harness::ClaudeCode, session: "c7".into(), title: None });
        assert_eq!(resolver.resolve(11), Writer::Agent { harness: Harness::ClaudeCode, session: "c7".into(), title: None });
        assert_eq!(resolver.resolve(12), Writer::Agent { harness: Harness::Pi, session: "p1".into(), title: None });
    }

    phase a_stale_session_file_of_a_reused_pid_is_not_this_process;
    {
        let procs = Procs::new();
        let home = procs.home();
        let home = home.to_str().unwrap();
        // Started a day after boot; the file was written at boot by an earlier process.
        procs.spawn(500, 1, 100 * 86_400, true, &[("HOME", home)], TERMINAL);
        procs.claude_session(500, r#"{"pid":500,"sessionId":"old","startedAt":1700000000000}"#);
        assert_eq!(procs.resolver().resolve(500), Writer::Person);
        procs.claude_session(500, r#"{"pid":501,"sessionId":"other"}"#);
        assert_eq!(procs.resolver().resolve(500), Writer::Person, "another pid's record");
    }

    phase apps_are_named_by_their_scope;
    {
        let procs = Procs::new();
        let slice = "/user.slice/user-1000.slice/user@1000.service/app.slice";
        procs.spawn(600, 1, 1, false, &[], &format!("{slice}/app-tilo-so.tilo.notes-4242.scope"));
        procs.spawn(601, 1, 1, false, &[], &format!("{slice}/app-tilo-so.tilo.my\\x2dapp-17.scope"));
        procs.spawn(602, 1, 1, false, &[], &format!("{slice}/app-gnome-org.gnome.TextEditor-9876.scope"));
        procs.spawn(603, 1, 1, false, &[], &format!("{slice}/app-flatpak-org.mozilla.firefox-12345.scope"));
        procs.spawn(604, 1, 1, false, &[], &format!("{slice}/app-org.kde.kate@abc123.service"));
        // A command in a terminal of a desktop app's scope is the person's.
        procs.spawn(605, 1, 1, true, &[], &format!("{slice}/app-gnome-org.gnome.Ptyxis-1.scope"));
        procs.spawn(606, 1, 1, false, &[], "/user.slice/user-1000.slice/session-2.scope");
        let resolver = procs.resolver();
        assert_eq!(resolver.resolve(600), Writer::App { id: "so.tilo.notes".into() });
        assert_eq!(resolver.resolve(600).attribution().unwrap().actor, "app:so.tilo.notes");
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
        let writer = Writer::Agent {
            harness: Harness::ClaudeCode,
            session: "s".into(),
            title: Some(format!("a\u{7}{}", "é".repeat(200))),
        };
        let attribution = writer.attribution().unwrap();
        assert!(attribution.label.len() <= fs_protocol::files::MAX_ATTRIBUTION_TEXT_BYTES);
        assert!(!attribution.label.contains('\u{7}'));
        assert_eq!(attribution.validate(), Ok(()));
        assert_eq!(colour_of("agent:claude:s"), attribution.color);
    }
}
