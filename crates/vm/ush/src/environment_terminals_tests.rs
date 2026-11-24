#![allow(
    clippy::unwrap_used,
    reason = "a test: a terminal it cannot set up is a test that failed"
)]

use std::path::PathBuf;

use tilo_server_protocol::hosted_vm::VmSize;

use super::*;
use crate::hosted_vm::HostedTarget;
use crate::hosted_vm::testing::{fake_service, fake_vm};

const PATIENCE: Duration = Duration::from_secs(30);

struct Host {
    _dir: tempfile::TempDir,
    terminals: EnvironmentTerminals,
}

impl Host {
    fn start(left_after: Duration) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = fake_service(dir.path(), fake_vm(dir.path()));
        let shells = WorkspaceShells::new(
            dir.path().join("state"),
            Some(HostedTarget {
                socket,
                account: "acct-1".into(),
                size: VmSize::Small,
                plans: None,
            }),
        );
        Self {
            _dir: dir,
            terminals: EnvironmentTerminals::new(Arc::new(shells)).keeping_for(left_after),
        }
    }

    fn open(&self) -> TerminalStream {
        self.open_with(None, None, &[])
    }

    /// A stream on the session as `person` (nobody, when `None`).
    fn open_as(&self, person: Option<&Person>) -> TerminalStream {
        self.open_with(person, None, &[])
    }

    /// [`Self::open`], its runs told to `ran`.
    fn open_telling(&self, ran: Option<RanTo>) -> TerminalStream {
        self.open_with(None, ran, &[])
    }

    /// [`Self::open`], every program the session starts in the VM given `secrets`.
    fn open_giving(&self, secrets: &[VmSecret]) -> TerminalStream {
        self.open_with(None, None, secrets)
    }

    fn open_with(
        &self,
        person: Option<&Person>,
        ran: Option<RanTo>,
        secrets: &[VmSecret],
    ) -> TerminalStream {
        self.terminals
            .open(
                &TerminalAsk {
                    workspace: "ws1",
                    session: "s1",
                    payer: None,
                    person,
                    cols: 100,
                    rows: 30,
                    ran,
                    attach: None,
                    secrets,
                },
                Some(KEY.into()),
            )
            .unwrap()
    }

    fn files(&self) -> PathBuf {
        self.terminals.shells().files("ws1")
    }
}

const KEY: &str = "0123456789abcdef0123";

/// A stream as a person sees it: what it drew so far, read past what was already looked for.
struct Screen {
    stream: TerminalStream,
    drawn: Vec<u8>,
    seen: usize,
}

impl Screen {
    /// Wait for the shell's prompt, then type `text`. Typing ahead of the prompt would hand
    /// the keys to the program that just ended.
    fn type_at_prompt(&mut self, text: &str) {
        self.until(&format!("\x1b]133;A;k={KEY}"));
        self.type_now(text);
    }

    fn type_now(&self, text: &str) {
        assert!(self.stream.tell(&ToTerminal::In(text.as_bytes().to_vec())));
    }

    /// Wait until the stream draws `needle`.
    fn until(&mut self, needle: &str) {
        let deadline = Instant::now().checked_add(PATIENCE).unwrap();
        loop {
            let unseen = self.drawn.get(self.seen..).unwrap_or_default();
            if let Some(at) = unseen
                .windows(needle.len())
                .position(|window| window == needle.as_bytes())
            {
                self.seen = self.seen.saturating_add(at).saturating_add(needle.len());
                return;
            }
            let text = String::from_utf8_lossy(unseen).into_owned();
            assert!(Instant::now() < deadline, "never drew {needle:?}: {text:?}");
            match self.stream.hear(Duration::from_millis(200)) {
                Some(FromTerminal::Out(bytes)) => self.drawn.extend(bytes),
                Some(other) => panic!("{other:?} before {needle:?}: {text:?}"),
                None => {}
            }
        }
    }

    /// How the session ended, skipping what it drew first.
    fn ending(&self) -> FromTerminal {
        let deadline = Instant::now().checked_add(PATIENCE).unwrap();
        loop {
            assert!(Instant::now() < deadline, "the session never ended");
            match self.stream.hear(Duration::from_millis(200)) {
                Some(FromTerminal::Out(_)) | None => {}
                Some(other) => return other,
            }
        }
    }
}

fn screen(stream: TerminalStream) -> Screen {
    Screen {
        stream,
        drawn: Vec::new(),
        seen: 0,
    }
}

fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now().checked_add(PATIENCE).unwrap();
    while !done() {
        assert!(Instant::now() < deadline, "never {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

tilo_test_env::scenario! {
    /// A hosted workspace's shell on a terminal, carried as a stream: keys, sizes, Ctrl+C
    /// and the exit, and the session kept for a stream that comes back.
    fn a_workspace_shell_runs_on_a_terminal_its_stream_carries;

    phase a_command_typed_at_the_prompt_commits_as_the_person_at_it;
    {
        let host = Host::start(LEFT_AFTER);
        let ada = Person {
            name: "Ada Lovelace".into(),
            address: "ada@eu1.tilo.sh".into(),
        };
        let mut screen = screen(host.open_as(Some(&ada)));
        screen.type_at_prompt(
            "sh -c 'echo au\"\"thor=$GIT_AUTHOR_NAME at $GIT_COMMITTER_EMAIL'\r",
        );
        screen.until("author=Ada Lovelace at ada@eu1.tilo.sh");
    }

    phase what_is_typed_reaches_a_program_in_the_vm_on_a_terminal;
    {
        let host = Host::start(LEFT_AFTER);
        let mut screen = screen(host.open());
        // The quotes keep the echoed command line from reading like its output.
        screen.type_at_prompt("sh -c 'test -t 0 && echo on-a-t\"\"erminal; read x; echo \"got $x\"'\r");
        screen.until("on-a-terminal");
        screen.type_now("hello\r");
        screen.until("got hello");
    }

    phase the_sessions_secrets_reach_its_programs_in_the_vm_and_not_the_shell;
    {
        let terminals = Host::start(LEFT_AFTER);
        let mut screen = screen(terminals.open_giving(&[VmSecret {
            variable: "GITHUB_TOKEN".into(),
            value: "the-vaults-own".into(),
        }]));
        // The quotes keep the echoed command line from reading like its output.
        screen.type_at_prompt("sh -c 'echo \"p[$GITHUB_TOKEN]\"'\r");
        screen.until("p[the-vaults-own]");
        screen.type_at_prompt("echo \"s[$GITHUB_TOKEN]\"\r");
        screen.until("s[]");
    }

    phase the_vms_terminal_follows_the_window_size;
    {
        let host = Host::start(LEFT_AFTER);
        let mut screen = screen(host.open());
        screen.type_at_prompt("stty size\r");
        screen.until("30 100");
        assert!(screen.stream.tell(&ToTerminal::Resize { cols: 120, rows: 40 }));
        screen.type_at_prompt("stty size\r");
        screen.until("40 120");
    }

    phase ctrl_c_ends_the_program_and_the_shell_goes_on;
    {
        let host = Host::start(LEFT_AFTER);
        let mut screen = screen(host.open());
        // The pause lets the program's terminal take keys as they come before it says so.
        screen.type_at_prompt("sh -c 'sleep 0.5; echo rea\"\"dy; exec cat'\r");
        screen.until("ready");
        screen.type_now("\x03");
        screen.type_at_prompt("echo ended-$?\r");
        screen.until("ended-130");
    }

    phase ctrl_c_ends_what_the_shell_runs_itself;
    {
        let host = Host::start(LEFT_AFTER);
        let mut screen = screen(host.open());
        screen.type_at_prompt("echo slee\"\"ping; sleep 60; echo after-$?\r");
        screen.until("sleeping");
        screen.type_now("\x03");
        screen.type_at_prompt("echo al\"\"ive\r");
        screen.until("alive");
    }

    phase the_shells_exit_status_ends_the_stream;
    {
        let host = Host::start(LEFT_AFTER);
        let mut screen = screen(host.open());
        screen.type_at_prompt("exit 7\r");
        assert_eq!(screen.ending(), FromTerminal::Exited(7));
        assert!(!screen.stream.tell(&ToTerminal::In(b"echo late\r".to_vec())));
        wait_until("forgot the session", || host.terminals.kept().is_empty());
        assert!(host.terminals.attach("ws1", "s1", screen.stream.id()).is_none());
    }

    phase a_stream_that_comes_back_in_time_finds_the_shell_where_it_was;
    {
        let host = Host::start(LEFT_AFTER);
        let mut first = screen(host.open());
        let id = first.stream.id().to_owned();
        assert_eq!(id.len(), 32);
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
        first.type_at_prompt("mkdir sub; cd sub; echo mo\"\"ved\r");
        first.until("moved");
        // The next prompt is drawn to this stream, so the one coming back types at it.
        first.until(&format!("\x1b]133;A;k={KEY}"));
        drop(first);
        let mut back = screen(host.terminals.attach("ws1", "s1", &id).unwrap());
        back.type_now("echo \"at:$PWD\"\r");
        let here = host.files().join("sub");
        back.until(&format!("at:{}", here.display()));
    }

    phase a_session_nobody_comes_back_to_hangs_up;
    {
        let host = Host::start(Duration::from_millis(300));
        let mut screen = screen(host.open());
        let id = screen.stream.id().to_owned();
        screen.type_at_prompt("mkdir sub; cd sub; sh -c 'echo rea\"\"dy; exec cat'\r");
        screen.until("ready");
        drop(screen);
        // The program on the terminal ends with the hangup, and the shell with it.
        wait_until("hung up the session", || host.terminals.kept().is_empty());
        assert!(host.terminals.attach("ws1", "s1", &id).is_none());
        let next = host.terminals.shells().run("ws1", "s1", "pwd", false).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&next.output).trim(),
            host.files().to_string_lossy(),
            "a new shell, not the one that was in sub"
        );
    }

    phase a_second_stream_supersedes_the_first;
    {
        let host = Host::start(LEFT_AFTER);
        let first = host.open();
        let mut second = screen(host.terminals.attach("ws1", "s1", first.id()).unwrap());
        assert_eq!(first.hear(Duration::ZERO), Some(FromTerminal::Superseded));
        assert!(!first.tell(&ToTerminal::In(b"echo no\r".to_vec())));
        second.type_at_prompt("echo o\"\"ne\r");
        second.until("one");
        // Opening the session again takes it too, rather than starting a second shell.
        let mut third = screen(host.open());
        assert_eq!(third.stream.id(), first.id());
        assert_eq!(second.stream.hear(Duration::ZERO), Some(FromTerminal::Superseded));
        third.type_now("echo t\"\"wo\r");
        third.until("two");
        drop(first);
        drop(second);
        third.type_at_prompt("echo th\"\"ree\r");
        third.until("three");
    }

    phase a_stream_is_carried_over_a_byte_stream_in_frames;
    {
        let host = Host::start(LEFT_AFTER);
        let stream = host.open();
        let (mut device, served) = std::os::unix::net::UnixStream::pair().unwrap();
        let from = served.try_clone().unwrap();
        let serving = std::thread::spawn(move || serve(stream, from, served).unwrap());
        wire::write_to(&mut device, &ToTerminal::Resize { cols: 90, rows: 20 }).unwrap();
        wire::write_to(&mut device, &ToTerminal::In(b"stty size; exit 3\r".to_vec())).unwrap();
        let mut drawn = Vec::new();
        let ended = loop {
            match wire::read_from(&mut device).unwrap().unwrap() {
                FromTerminal::Out(bytes) => drawn.extend(bytes),
                other => break other,
            }
        };
        assert_eq!(ended, FromTerminal::Exited(3));
        assert!(String::from_utf8_lossy(&drawn).contains("20 90"), "{drawn:?}");
        assert_eq!(serving.join().unwrap(), Ended::Exited(3));
    }

    phase a_handed_stream_is_the_devices_it_was_handed_to_alone;
    {
        let host = Arc::new(Host::start(LEFT_AFTER));
        let stream = host.open();
        let session = stream.id().to_owned();
        assert!(
            host.terminals.attach("ws1", "s2", &session).is_none(),
            "a session's id takes no other session"
        );
        let handed = host.terminals.hand_out([1; 32], stream).unwrap();
        let mut hello = Vec::new();
        wire::write_hello(&mut hello, &wire::Hello { stream: handed.clone() }).unwrap();

        // Another device presenting the id is refused, and the id stays the device's.
        let mut refused = Vec::new();
        let error = host
            .terminals
            .serve_handed([2; 32], std::io::Cursor::new(hello.clone()), &mut refused)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(
            wire::read_welcome(&mut refused.as_slice()).unwrap(),
            Some(wire::Welcome::Refused {
                reason: NOT_HANDED.into()
            })
        );

        let (mut device, served) = std::os::unix::net::UnixStream::pair().unwrap();
        let from = served.try_clone().unwrap();
        let serving = {
            let host = Arc::clone(&host);
            std::thread::spawn(move || host.terminals.serve_handed([1; 32], from, served))
        };
        wire::write_hello(&mut device, &wire::Hello { stream: handed }).unwrap();
        assert_eq!(
            wire::read_welcome(&mut device).unwrap(),
            Some(wire::Welcome::Opened { session })
        );
        wire::write_to(&mut device, &ToTerminal::In(b"exit 4\r".to_vec())).unwrap();
        let ended = loop {
            match wire::read_from(&mut device).unwrap().unwrap() {
                FromTerminal::Out(_) => {}
                other => break other,
            }
        };
        assert_eq!(ended, FromTerminal::Exited(4));
        assert_eq!(serving.join().unwrap().unwrap(), Ended::Exited(4));

        // Taken once: the id hands nothing more.
        let mut again = Vec::new();
        assert!(
            host.terminals
                .serve_handed([1; 32], std::io::Cursor::new(hello), &mut again)
                .is_err()
        );
    }

    phase each_command_line_is_told_as_it_ends_with_what_it_printed_attached_or_not;
    {
        let host = Host::start(LEFT_AFTER);
        let queue = RanQueue::new();
        let stream = host.open_telling(Some(queue.to()));
        queue.of(stream.id());
        let mut first = screen(stream);
        first.type_at_prompt("mkdir sub; cd sub; echo he\"\"llo; false\r");
        let ran = queue.next().unwrap();
        assert_eq!(ran.command, "mkdir sub; cd sub; echo he\"\"llo; false");
        assert!(ran.cwd.ends_with("/files"), "where the session starts: {ran:?}");
        assert_eq!(ran.exit, 1);
        let printed = String::from_utf8_lossy(&ran.output).into_owned();
        assert!(printed.contains("hello"), "{printed:?}");
        assert!(!printed.contains("he\"\"llo"), "the line as typed is not its output: {printed:?}");
        assert!(ran.started_ms <= ran.ended_ms && !ran.truncated, "{ran:?}");
        assert_eq!(
            tilo_clock::with_wall_ms(1_700_000_000_123, now_ms),
            1_700_000_000_123
        );

        // Nobody attached: the run is told all the same, once.
        first.type_at_prompt("sleep 1; echo lat\"\"er\r");
        first.until("sleep 1");
        let session = first.stream.id().to_owned();
        drop(first);
        let ran = queue.next().unwrap();
        assert_eq!(ran.command, "sleep 1; echo lat\"\"er");
        assert!(ran.cwd.ends_with("sub"), "{ran:?}");
        assert!(String::from_utf8_lossy(&ran.output).contains("later"), "{ran:?}");

        // Its stream of runs carries the same, and ends with the session.
        let mut back = screen(host.terminals.attach("ws1", "s1", &session).unwrap());
        let (mut device, served) = std::os::unix::net::UnixStream::pair().unwrap();
        let handed = host.terminals.hand_runs([1; 32], queue).unwrap();
        wire::write_hello(&mut device, &wire::Hello { stream: handed }).unwrap();
        let from = served.try_clone().unwrap();
        let serving = std::thread::spawn({
            let terminals = Arc::new(host);
            move || {
                let served = terminals.terminals.serve_handed([1; 32], from, served);
                (terminals, served)
            }
        });
        assert_eq!(
            wire::read_welcome(&mut device).unwrap(),
            Some(wire::Welcome::Opened {
                session
            })
        );
        back.type_at_prompt("echo th\"\"ird\r");
        let ran = wire::read_ran(&mut device).unwrap().unwrap();
        assert_eq!(ran.command, "echo th\"\"ird");
        assert!(String::from_utf8_lossy(&ran.output).contains("third"), "{ran:?}");
        back.type_at_prompt("exit 2\r");
        assert_eq!(back.ending(), FromTerminal::Exited(2));
        drop(back);
        assert_eq!(wire::read_ran(&mut device).unwrap(), None, "exit is no run");
        let (_host, served) = serving.join().unwrap();
        assert_eq!(served.unwrap(), Ended::Left);
    }
}
