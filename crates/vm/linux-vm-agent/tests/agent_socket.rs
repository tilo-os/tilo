//! The guest's agent socket, through the agent itself: a launch that serves
//! agents has its own programs' connections there carried to the host, and
//! nothing else in the machine is carried.
//!
//! The test is the host: it asks the agent for launches over a stream pair, as
//! the host does over vsock, and serves what the launch carries. The program
//! launched is a stand-in for `tilo-runtime --connect`: it connects to the
//! socket its variable names, sends one line and prints the line it is
//! answered.

use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use linux_vm_agent::server::Agent;
use linux_vm_agent::user::GuestUser;
use linux_vm_protocol::client::{self, AgentServer, LaunchIo};
use linux_vm_protocol::{Display, ExitStatus, Launch, Stdio};

/// Connects where `TILO_AGENT_SOCK` (else `SOCK`) says, says who it is and
/// prints what it is answered: nothing, when the connection is closed on it.
const CLIENT: &str = r#"
import os, socket
path = os.environ.get("TILO_AGENT_SOCK") or os.environ["SOCK"]
print("socket " + path, flush=True)
s = socket.socket(socket.AF_UNIX)
s.connect(path)
try:
    s.sendall(b"initialize\n")
    line = s.makefile("rb").readline().decode().strip()
except OSError:
    line = ""
print("answered " + (line or "nothing"), flush=True)
"#;

fn me(home: &Path) -> GuestUser {
    GuestUser {
        name: "me".into(),
        uid: rustix::process::getuid().as_raw(),
        gid: rustix::process::getgid().as_raw(),
        home: home.to_owned(),
        shell: "/bin/sh".into(),
    }
}

/// The host's server: answers each line with `admitted <line>`, and counts
/// the connections it was handed.
fn server(handed: &Arc<AtomicUsize>) -> AgentServer {
    let handed = Arc::clone(handed);
    Arc::new(move |caller, mut theirs: UnixStream| {
        assert!(
            caller.program.contains("python"),
            "the guest names the program that connected: {caller:?}"
        );
        handed.fetch_add(1, Ordering::SeqCst);
        let mut line = Vec::new();
        let mut byte = [0_u8; 1];
        while theirs.read(&mut byte).is_ok_and(|read| read == 1) {
            if byte[0] == b'\n' {
                let answer = format!("admitted {}\n", String::from_utf8_lossy(&line));
                let _ = theirs.write_all(answer.as_bytes());
                line.clear();
            } else {
                line.push(byte[0]);
            }
        }
    })
}

fn python(env: Vec<(String, String)>) -> Launch {
    Launch {
        argv: vec!["python3".into(), "-I".into(), "-c".into(), CLIENT.into()],
        cwd: Some("/".into()),
        env,
        stdio: Stdio::Pump,
        display: Display::None,
    }
}

/// Launch `request` on `agent`, serving agents with `agents` when given;
/// what it printed and how it ended.
fn launched(
    agent: &Arc<Agent>,
    request: &Launch,
    stdin: Option<Box<dyn std::io::Read + Send>>,
    agents: Option<&AgentServer>,
) -> (String, ExitStatus) {
    let (host, guest) = UnixStream::pair().unwrap();
    let serving = Arc::clone(agent);
    let guest_side = std::thread::spawn(move || serving.serve(guest));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let io = LaunchIo {
        stdin,
        stdout: &mut out,
        stderr: &mut err,
        started: &mut |_| {},
    };
    let status = match agents {
        Some(agents) => client::launch_serving_agents(host, request, io, agents, |_| {}),
        None => client::launch(host, request, io, |_| {}),
    }
    .unwrap();
    guest_side.join().unwrap().unwrap();
    assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
    (String::from_utf8(out).unwrap(), status)
}

/// A reader that gives nothing until told to end.
struct Held(mpsc::Receiver<()>);

impl std::io::Read for Held {
    fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
        let _ = self.0.recv();
        Ok(0)
    }
}

tilo_test_env::scenario! {
    fn the_agent_socket_serves_the_launches_that_serve_agents_alone;

    phase a_program_of_a_serving_launch_reaches_the_hosts_server;
    {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("run/tilo/agent.sock");
        let agent = Arc::new(Agent::new(me(dir.path()), "boot".into()).with_agent_socket(&socket));
        let handed = Arc::new(AtomicUsize::new(0));
        let agents = server(&handed);
        let (printed, status) = launched(&agent, &python(Vec::new()), None, Some(&agents));
        assert_eq!(status.code, Some(0), "{printed}");
        assert!(
            printed.contains(&format!("socket {}", socket.display())),
            "the guest names its socket to the program: {printed}"
        );
        assert!(printed.contains("answered admitted initialize"), "{printed}");
        assert_eq!(handed.load(Ordering::SeqCst), 1);

        // A launch that does not serve agents is turned away at the same
        // socket, and its host is handed nothing.
        let elsewhere = vec![("SOCK".to_owned(), socket.display().to_string())];
        let (printed, status) = launched(&agent, &python(elsewhere), None, None);
        assert_eq!(status.code, Some(0), "{printed}");
        assert!(printed.contains("answered nothing"), "{printed}");
        assert_eq!(handed.load(Ordering::SeqCst), 1, "nothing reached the host");
    }

    phase a_process_outside_every_launch_is_turned_away_unread;
    {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("agent.sock");
        let agent = Arc::new(Agent::new(me(dir.path()), "boot".into()).with_agent_socket(&socket));
        let handed = Arc::new(AtomicUsize::new(0));
        let agents = server(&handed);
        let (release, held) = mpsc::channel();
        let running = {
            let (agent, agents) = (Arc::clone(&agent), Arc::clone(&agents));
            let cat = Launch {
                argv: vec!["cat".into()],
                cwd: Some("/".into()),
                env: Vec::new(),
                stdio: Stdio::Pump,
                display: Display::None,
            };
            std::thread::spawn(move || {
                launched(&agent, &cat, Some(Box::new(Held(held))), Some(&agents))
            })
        };
        // This test's own process is no process of that launch.
        let mut outsider = loop {
            if let Ok(stream) = UnixStream::connect(&socket) {
                break stream;
            }
            assert!(!running.is_finished(), "the launch is still running");
            // The socket is bound as the launch starts, on another thread.
            std::thread::sleep(Duration::from_millis(10));
        };
        outsider.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let _ = outsider.write_all(b"initialize\n");
        let mut answer = Vec::new();
        let _ = outsider.read_to_end(&mut answer);
        assert!(answer.is_empty(), "turned away without a byte");
        release.send(()).unwrap();
        let (_, status) = running.join().unwrap();
        assert_eq!(status.code, Some(0));
        assert_eq!(handed.load(Ordering::SeqCst), 0, "the host never heard of it");
    }

    phase what_a_launch_carried_ends_with_it;
    {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("agent.sock");
        let agent = Arc::new(Agent::new(me(dir.path()), "boot".into()).with_agent_socket(&socket));
        // The program leaves a process of its own connected; the launch's end
        // ends its connection on the host.
        let lingering = r#"
import os, socket, sys
s = socket.socket(socket.AF_UNIX)
s.connect(os.environ["TILO_AGENT_SOCK"])
s.sendall(b"initialize\n")
print(s.makefile("rb").readline().decode().strip(), flush=True)
if os.fork() == 0:
    sys.stdout.close()
    sys.stderr.close()
    os.setsid()
    import time
    time.sleep(20)
"#;
        let (ending, ended) = mpsc::channel();
        let agents: AgentServer = {
            let ending = Mutex::new(ending);
            Arc::new(move |_, mut theirs: UnixStream| {
                let mut byte = [0_u8; 1];
                while theirs.read(&mut byte).is_ok_and(|read| read == 1) {
                    if byte[0] == b'\n' {
                        let _ = theirs.write_all(b"admitted\n");
                    }
                }
                let _ = ending.lock().unwrap().send(());
            })
        };
        let request = Launch {
            argv: vec!["python3".into(), "-I".into(), "-c".into(), lingering.into()],
            ..python(Vec::new())
        };
        let (printed, status) = launched(&agent, &request, None, Some(&agents));
        assert_eq!(status.code, Some(0), "{printed}");
        assert!(printed.contains("admitted"), "{printed}");
        ended
            .recv_timeout(Duration::from_secs(10))
            .expect("the host's end of the connection ended with the launch");
    }
}
