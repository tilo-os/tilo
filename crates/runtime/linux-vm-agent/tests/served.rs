//! A served run, through the agent program itself: what the run's own
//! processes ask is carried to the host and answered, nothing else in the
//! machine is answered, and the run ends as its host says.
//!
//! The test is the host: it starts `tilo-linux-vm-agent serve-run` as the guest
//! agent starts a program, reads its frames and writes the answers. The
//! program served is a stand-in for a coding harness, a shell script that
//! asks its model endpoint over the machine's loopback as a harness does.


use std::io::{Read as _, Write as _};
use std::net::{Ipv4Addr, TcpStream};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use linux_vm_protocol::served::{self, Asked, FromRun, ToRun};
use linux_vm_protocol::{ExitStatus, recv, send};
use tilo_test_env::{DiesWithTheTest as _, SpawnUnderEnvLock as _};

/// Says where its model is, then asks it once for every line it is given
/// (`at ADDRESS` asks there instead) and prints the whole answer.
const HARNESS: &str = r#"#!/bin/bash
trap '' PIPE
trap 'echo interrupted; exit 0' INT
echo "model at $MODEL"
ask() {
  local hostport=${1#http://} answer
  exec 3<>"/dev/tcp/${hostport%:*}/${hostport#*:}"
  printf 'POST /v1/messages?beta=true HTTP/1.1\r\nHost: model\r\nAuthorization: Bearer placeholder\r\nX-Api-Key: placeholder\r\nContent-Type: application/json\r\nContent-Length: %s\r\n\r\n%s' "${#2}" "$2" >&3 2>/dev/null
  answer=$(cat <&3 2>/dev/null)
  exec 3<&- 3>&-
  echo "answered: [${answer//$'\r'/}]"
}
while IFS= read -r line; do
  case $line in
    "at "*) ask "${line#at }" "from another run" ;;
    *) ask "$MODEL" "$line" ;;
  esac
done
echo "input ended"
"#;

/// Ignores every ask to end.
const STUBBORN: &str = r"#!/bin/bash
trap '' INT TERM
echo ready
while :; do read -r -t 1 _ || true; done
";

/// Asks its model, then leaves without waiting for the answer once it is
/// given a line.
const LEAVER: &str = r#"#!/bin/bash
hostport=${MODEL#http://}
exec 3<>"/dev/tcp/${hostport%:*}/${hostport#*:}"
printf 'POST /v1/messages HTTP/1.1\r\nContent-Length: 2\r\n\r\nhi' >&3
read -r _
exec 3<&- 3>&-
echo left
read -r _
"#;

/// Leaves a program of its own running, says which, and ends when its input does.
const LEAVES_ONE: &str = r#"#!/bin/bash
sleep 300 &
echo "left $!"
read -r _ || true
"#;

/// One served run and this test as its host.
struct Run {
    child: Child,
    input: Option<ChildStdin>,
    frames: Receiver<FromRun>,
    /// What the program printed that no assertion took yet.
    printed: String,
}

impl Run {
    /// Serve `program` with one endpoint, `model`, whose address the program
    /// reads from the variable `MODEL`.
    fn serve(program: &[String], stop_grace: Duration) -> Self {
        let mut command = Command::new(tilo_test_env::package_binary!("tilo-linux-vm-agent"));
        command
            .args(served::arguments(&["model"], stop_grace, program))
            .env("MODEL", served::placeholder("model"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .dies_with_the_test();
        let mut child = command.spawn_locked().unwrap();
        let mut output = child.stdout.take().unwrap();
        let (telling, frames_in) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(Some(frame)) = recv::<_, FromRun>(&mut output) {
                if telling.send(frame).is_err() {
                    break;
                }
            }
        });
        Self {
            input: child.stdin.take(),
            child,
            frames: frames_in,
            printed: String::new(),
        }
    }

    /// Serve the stand-in `script`, written into `dir`.
    fn of(dir: &Path, script: &str, stop_grace: Duration) -> Self {
        let script = tilo_test_env::script(dir.join("harness"), script).unwrap();
        Self::serve(&[script.path().display().to_string()], stop_grace)
    }

    fn next(&self) -> FromRun {
        tilo_test_env::recv("a frame from the run", &self.frames)
    }

    fn tell(&mut self, frame: &ToRun) {
        let input = self.input.as_mut().unwrap();
        send(input, frame).unwrap();
        input.flush().unwrap();
    }

    /// The launcher says it is up and the program started: its process.
    fn started(&self) -> u32 {
        assert_eq!(
            self.next(),
            FromRun::Ready {
                version: served::VERSION,
                endpoints: vec!["model".into()],
            }
        );
        match self.next() {
            FromRun::Started { pid } => pid,
            other => panic!("expected the program started, got {other:?}"),
        }
    }

    /// What the program printed up to and with `end`, taken.
    fn printed_until(&mut self, end: &str) -> String {
        loop {
            if let Some(at) = self.printed.find(end) {
                let rest = self.printed.split_off(at.saturating_add(end.len()));
                return std::mem::replace(&mut self.printed, rest);
            }
            match self.next() {
                FromRun::Out(bytes) => self.printed.push_str(&String::from_utf8(bytes).unwrap()),
                other => panic!("expected the program to print {end:?}, got {other:?}"),
            }
        }
    }

    /// The next request made at an endpoint, with its whole body.
    fn asked(&mut self) -> (Asked, Vec<u8>) {
        let asked = loop {
            match self.next() {
                FromRun::Asked(asked) => break asked,
                FromRun::Out(bytes) => self.printed.push_str(&String::from_utf8(bytes).unwrap()),
                other => panic!("expected a request, got {other:?}"),
            }
        };
        let mut body = Vec::new();
        loop {
            match self.next() {
                FromRun::AskedBody { id, bytes, last } => {
                    assert_eq!(id, asked.id);
                    body.extend(bytes);
                    if last {
                        return (asked, body);
                    }
                }
                other => panic!("expected the request's body, got {other:?}"),
            }
        }
    }

    /// Answer request `id` with `words`, in two pieces.
    fn answer(&mut self, id: u64, words: &str) {
        let (first, second) = words.split_at(words.len() / 2);
        self.tell(&ToRun::Answer {
            id,
            status: 200,
            headers: vec![("content-type".into(), "text/plain".into())],
        });
        self.tell(&ToRun::AnswerBody {
            id,
            bytes: first.as_bytes().to_vec(),
            last: false,
        });
        self.tell(&ToRun::AnswerBody {
            id,
            bytes: second.as_bytes().to_vec(),
            last: true,
        });
    }

    /// How the program ended, and with it the launcher.
    fn ended(mut self) -> (ExitStatus, Option<i32>) {
        let status = loop {
            match self.next() {
                FromRun::Exited(status) => break status,
                FromRun::Out(_) | FromRun::Err(_) => {}
                other => panic!("expected the program to end, got {other:?}"),
            }
        };
        (status, self.child.wait().unwrap().code())
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The port of the address a stand-in printed as `model at ADDRESS`.
fn port_of(said: &str) -> u16 {
    said.trim()
        .strip_prefix("model at http://127.0.0.1:")
        .unwrap_or_else(|| panic!("{said:?} names no loopback address"))
        .parse()
        .unwrap()
}

const GRACE: Duration = Duration::from_secs(20);

tilo_test_env::scenario! {
    fn a_run_is_served_by_its_host_and_by_nobody_else;

    phase what_the_runs_own_process_asks_reaches_the_host_and_is_answered;
    {
        let dir = tempfile::tempdir().unwrap();
        let mut run = Run::of(dir.path(), HARNESS, GRACE);
        run.started();
        let port = port_of(&run.printed_until("\n"));
        run.tell(&ToRun::In(b"what is new\n".to_vec()));
        let (asked, body) = run.asked();
        assert_eq!(
            (asked.endpoint.as_str(), asked.method.as_str(), asked.path.as_str()),
            ("model", "POST", "/v1/messages?beta=true")
        );
        assert_eq!(
            asked.headers,
            [("content-type".to_owned(), "application/json".to_owned())],
            "what would sign the request in never leaves the machine"
        );
        assert_eq!(body, b"what is new");
        run.answer(asked.id, "nothing much");
        let answered = run.printed_until("]\n");
        assert!(answered.starts_with("answered: [HTTP/1.1 200 OK\n"), "{answered}");
        assert!(answered.contains("content-type: text/plain\n"), "{answered}");
        assert!(answered.contains("\nnothin\n") && answered.contains("\ng much\n"), "{answered}");

        // A second request is a second number, and the input's end ends the program.
        run.tell(&ToRun::In(b"and now\n".to_vec()));
        let (again, body) = run.asked();
        assert_ne!(again.id, asked.id);
        assert_eq!(body, b"and now");
        run.answer(again.id, "still nothing");
        run.printed_until("]\n");
        run.tell(&ToRun::CloseIn);
        assert_eq!(run.printed_until("\n"), "input ended\n");
        assert_eq!(
            run.ended(),
            (ExitStatus { code: Some(0), signal: None }, Some(0))
        );

        // Its endpoint ended with it: nothing listens where it was.
        assert!(
            TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_err(),
            "the endpoint outlived its run"
        );
    }

    phase another_process_in_the_machine_and_another_run_get_nothing;
    {
        let dir = tempfile::tempdir().unwrap();
        let mut run = Run::of(dir.path(), HARNESS, GRACE);
        run.started();
        let said = run.printed_until("\n");
        let port = port_of(&said);

        // This test's process is in the machine and is not of the run.
        let mut outsider = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        outsider.set_read_timeout(Some(tilo_test_env::hang_bound())).unwrap();
        let _ = outsider.write_all(b"POST /v1/messages HTTP/1.1\r\nContent-Length: 2\r\n\r\nhi");
        let mut got = Vec::new();
        let _ = outsider.read_to_end(&mut got);
        assert!(got.is_empty(), "an outsider was answered: {:?}", String::from_utf8_lossy(&got));
        assert_eq!(run.next(), FromRun::Turned { endpoint: "model".into() });

        // Another run's own process is not of this run either.
        let other_dir = tempfile::tempdir().unwrap();
        let mut other = Run::of(other_dir.path(), HARNESS, GRACE);
        other.started();
        other.printed_until("\n");
        let address = said.trim().strip_prefix("model at ").unwrap();
        other.tell(&ToRun::In(format!("at {address}\n").into_bytes()));
        assert_eq!(other.printed_until("]\n"), "answered: []\n");
        assert_eq!(run.next(), FromRun::Turned { endpoint: "model".into() });

        // Neither reached the host as a request, and the run is still served.
        run.tell(&ToRun::In(b"still mine\n".to_vec()));
        let (asked, body) = run.asked();
        assert_eq!(body, b"still mine");
        run.answer(asked.id, "yes");
        assert!(run.printed_until("]\n").contains("200 OK"));
    }

    phase an_asker_that_leaves_is_told_to_the_host;
    {
        let dir = tempfile::tempdir().unwrap();
        let mut run = Run::of(dir.path(), LEAVER, GRACE);
        run.started();
        let (asked, body) = run.asked();
        assert_eq!(body, b"hi");
        run.tell(&ToRun::In(b"\n".to_vec()));
        assert_eq!(run.printed_until("\n"), "left\n");
        assert_eq!(run.next(), FromRun::AskedGone { id: asked.id });
    }
}

tilo_test_env::scenario! {
    fn a_run_ends_as_its_host_says_and_never_outlives_it;

    phase a_stop_asks_the_program_first_and_it_ends_itself;
    {
        let dir = tempfile::tempdir().unwrap();
        let mut run = Run::of(dir.path(), HARNESS, GRACE);
        run.started();
        run.printed_until("\n");
        run.tell(&ToRun::Stop);
        assert_eq!(run.printed_until("\n"), "interrupted\n");
        assert_eq!(
            run.ended(),
            (ExitStatus { code: Some(0), signal: None }, Some(0))
        );
    }

    phase a_program_that_will_not_end_is_ended_after_its_grace;
    {
        let dir = tempfile::tempdir().unwrap();
        let mut run = Run::of(dir.path(), STUBBORN, Duration::from_millis(150));
        run.started();
        assert_eq!(run.printed_until("\n"), "ready\n");
        run.tell(&ToRun::Stop);
        assert_eq!(
            run.ended(),
            (ExitStatus { code: None, signal: Some(9) }, Some(137))
        );
    }

    phase a_host_that_goes_away_ends_the_program;
    {
        let dir = tempfile::tempdir().unwrap();
        let mut run = Run::of(dir.path(), HARNESS, GRACE);
        let program = run.started();
        run.printed_until("\n");
        // The host's frames end: nobody is left to answer the run.
        drop(run.input.take());
        let (status, launcher) = run.ended();
        assert_eq!(status, ExitStatus { code: None, signal: Some(15) });
        assert_eq!(launcher, Some(143));
        tilo_test_env::exited("the program of a run whose host went away", program);
    }

    phase what_a_program_left_running_ends_with_its_run;
    {
        let dir = tempfile::tempdir().unwrap();
        let mut run = Run::of(dir.path(), LEAVES_ONE, GRACE);
        run.started();
        let left: u32 = run
            .printed_until("\n")
            .trim()
            .strip_prefix("left ")
            .unwrap()
            .parse()
            .unwrap();
        // The program ends by itself and leaves its own program behind, still running.
        run.tell(&ToRun::CloseIn);
        assert_eq!(
            run.ended(),
            (ExitStatus { code: Some(0), signal: None }, Some(0))
        );
        tilo_test_env::exited("what the run's program left running", left);
    }

    phase a_program_that_cannot_be_started_is_said_in_words;
    {
        let run = Run::serve(&["/nonexistent/coding-agent".to_owned()], GRACE);
        assert_eq!(
            run.next(),
            FromRun::Ready { version: served::VERSION, endpoints: vec!["model".into()] }
        );
        match run.next() {
            FromRun::Failed { message } => {
                assert!(
                    message.starts_with("/nonexistent/coding-agent: ")
                        && message.contains("No such file"),
                    "{message}"
                );
            }
            other => panic!("expected a failure in words, got {other:?}"),
        }
        let mut run = run;
        assert_eq!(run.child.wait().unwrap().code(), Some(127));
    }
}
