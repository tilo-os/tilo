//! A Terminal window in a workspace a server hosts gets the host's shell, carried by the
//! device's session: what is typed reaches the host, what the host draws comes back, the
//! window's size follows, and how the stream ends is told on the session's lease. A lost
//! connection comes back to the host's session by its id. Whatever the host says, the device's
//! own environment is never what opens.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use runtime_portal::workspace_terminal::{Reply, Request};
use ush::environment_terminals::{FromTerminal, ToTerminal, wire};
use ush::host::{TERMINAL_APP_ID, encode_launch_handoff};
use ush::store::Store;

const WORKSPACE: &str = "e787c8a3-e6d5-4b00-90fe-ff1af708b979";
const RECORD: &str = "uuid:5b0e2a6c-1f7d-4c3e-9a8b-2d4f6e8a0c1e";
const SESSION: &str = "0123456789abcdef0123456789abcdef";
const STREAM: &str = "fedcba9876543210fedcba9876543210";

/// The device's session as the supervisor reaches it at `runtime`: each workspace terminal it is
/// asked for, with the connection it was asked on. The runtime never reads the device key, which
/// only the device's daemon keeps, so the session can look at it as at any process of the login:
/// a request carries no proof of the device's identity.
fn session(runtime: &Path) -> mpsc::Receiver<(Request, UnixStream)> {
    let dir = runtime.join("tilo-session");
    std::fs::create_dir_all(&dir).unwrap();
    let listener = UnixListener::bind(dir.join("agent.sock")).unwrap();
    let (asked, asks) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let stream = stream.unwrap();
            let mut line = Vec::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_until(b'\n', &mut line)
                .unwrap();
            let request = Request::of(line.strip_suffix(b"\n").unwrap())
                .unwrap_or_else(|| panic!("not a workspace terminal: {line:?}"));
            assert!(
                request.proof.is_none(),
                "the runtime proves nothing it would need the device key for: {request:?}"
            );
            if asked.send((request, stream)).is_err() {
                return;
            }
        }
    });
    asks
}

fn serving(dir: &Path) -> PathBuf {
    let socket = dir.join("ush.sock");
    let store = Store::at(dir.join("store"));
    ush::records::testing::keep_serving(store.records_root());
    store.ensure().unwrap();
    let serve_socket = socket.clone();
    std::thread::spawn(move || {
        let _ = ush::supervisor::serve(&serve_socket, &store, &PathBuf::from("/"));
    });
    let deadline = tilo_test_env::hang_deadline();
    while !socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    // SAFETY: PR_GET_DUMPABLE takes no pointers and only reads a flag.
    let dumpable = unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) };
    assert_eq!(
        dumpable, 1,
        "the runtime reads no device key, so the session can look at it as at any process"
    );
    socket
}

/// A Terminal window's open, as the window asks it in the hosted workspace.
struct Window {
    control: UnixStream,
}

/// What the supervisor answered the window.
struct Answered {
    waiting: bool,
    reply: serde_json::Value,
    terminal: Option<(std::fs::File, UnixStream)>,
}

impl Window {
    fn connect(socket: &Path) -> Self {
        let control = UnixStream::connect(socket).unwrap();
        (&control)
            .write_all(&encode_launch_handoff(TERMINAL_APP_ID, Some(WORKSPACE)).unwrap())
            .unwrap();
        (&control)
            .write_all(b"{\"op\":\"hello\",\"protocol\":2}\n")
            .unwrap();
        let mut ready = String::new();
        BufReader::new(&control).read_line(&mut ready).unwrap();
        assert!(ready.contains("ready"), "{ready}");
        Self { control }
    }

    /// Ask for the window's session, coming back to `attach` if named.
    fn open(&self, attach: Option<&str>) {
        let mut request = serde_json::json!({
            "op": "open-session",
            "target": {"kind": "environment"},
            "cols": 80,
            "rows": 24,
            "record": RECORD,
            "end_report": true,
            "workspace": WORKSPACE,
        });
        if let Some(attach) = attach {
            request
                .as_object_mut()
                .unwrap()
                .insert("host_session".into(), attach.into());
        }
        let mut line = serde_json::to_vec(&request).unwrap();
        line.push(b'\n');
        (&self.control).write_all(&line).unwrap();
    }

    fn answer(&self) -> Answered {
        self.control
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        let mut waiting = false;
        loop {
            let (payload, mut fds) =
                runtime_portal::fd_transport::recv_with_fds(&self.control).unwrap();
            // The line saying the host is asked may arrive in one read with the answer.
            let mut lines = payload
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty());
            let mut line = lines.next().unwrap();
            let said: serde_json::Value = serde_json::from_slice(line).unwrap();
            if said.get("result").and_then(serde_json::Value::as_str) == Some("waiting") {
                waiting = true;
                match lines.next() {
                    Some(next) => line = next,
                    None => continue,
                }
            }
            let reply: serde_json::Value = serde_json::from_slice(line).unwrap();
            let terminal = (fds.len() == 2).then(|| {
                let lease = UnixStream::from(fds.pop().unwrap());
                (std::fs::File::from(fds.pop().unwrap()), lease)
            });
            return Answered {
                waiting,
                reply,
                terminal,
            };
        }
    }
}

/// The host's side of an opened stream: the window's frames as they arrive.
struct Host {
    stream: UnixStream,
}

impl Host {
    /// Answer the session's ask on `stream`: the host opened the shell, and the stream presented
    /// is the one handed out.
    fn opens(stream: UnixStream) -> Self {
        let mut stream = stream;
        stream.write_all(&Reply::Hosted.line()).unwrap();
        stream
            .write_all(
                &Reply::Opened {
                    stream: STREAM.into(),
                    session: SESSION.into(),
                }
                .line(),
            )
            .unwrap();
        stream
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        let hello = wire::read_hello(&mut stream).unwrap().unwrap();
        assert_eq!(
            hello.stream, STREAM,
            "the window's device presents the stream handed out"
        );
        wire::write_welcome(
            &mut stream,
            &wire::Welcome::Opened {
                session: SESSION.into(),
            },
        )
        .unwrap();
        Self { stream }
    }

    /// The frames the window sends until one `matches`.
    fn until(&mut self, mut matches: impl FnMut(&ToTerminal, &[u8]) -> bool) {
        let mut typed = Vec::new();
        loop {
            let frame = wire::read_to(&mut self.stream)
                .unwrap()
                .unwrap_or_else(|| panic!("the stream ended; typed {typed:?}"));
            if let ToTerminal::In(bytes) = &frame {
                typed.extend_from_slice(bytes);
            }
            if matches(&frame, &typed) {
                return;
            }
        }
    }

    fn says(&mut self, frame: &FromTerminal) {
        wire::write_from(&mut self.stream, frame).unwrap();
    }
}

fn drawn_until(master: &std::fs::File, needle: &str) -> String {
    crate::host_serve::read_until(master, needle, tilo_test_env::hang_bound())
}

fn report(lease: &UnixStream) -> serde_json::Value {
    lease
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let mut line = String::new();
    BufReader::new(lease).read_line(&mut line).unwrap();
    serde_json::from_str(line.trim()).unwrap_or_else(|_| panic!("no report: {line:?}"))
}

fn resize(master: &std::fs::File, cols: u16, rows: u16) {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCSWINSZ only reads the struct, which lives across the call.
    assert_eq!(
        unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &raw const size) },
        0
    );
}

#[test]
fn a_hosted_workspaces_window_types_into_the_hosts_shell_and_hears_it_end() {
    tilo_test_env::in_own_process(
        module_path!(),
        "a_hosted_workspaces_window_types_into_the_hosts_shell_and_hears_it_end",
        || {
            let dir = tempfile::tempdir().unwrap();
            let runtime = dir.path().join("run");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_RUNTIME_DIR", &runtime);
            env.set("TILO_IDENTITY_ROOT", dir.path().join("identity"));
            env.set("TILO_USH_IMAGES", dir.path().join("images"));
            let asks = session(&runtime);
            let socket = serving(dir.path());

            let window = Window::connect(&socket);
            window.open(None);
            let (asked, stream) = asks.recv_timeout(tilo_test_env::hang_bound()).unwrap();
            assert_eq!(
                (
                    asked.workspace.as_str(),
                    asked.record.as_str(),
                    asked.attach
                ),
                (WORKSPACE, RECORD, None),
                "the session is asked for the window's own record"
            );
            assert_eq!((asked.cols, asked.rows), (80, 24));
            let mut host = Host::opens(stream);
            let answered = window.answer();
            assert!(answered.waiting, "the window hears that the host is asked");
            assert_eq!(answered.reply["result"], "session", "{}", answered.reply);
            assert_eq!(
                answered.reply["host_session"], SESSION,
                "{}",
                answered.reply
            );
            assert!(
                answered.reply.get("commands").is_none(),
                "the host records the runs; the window records none: {}",
                answered.reply
            );
            let (mut master, lease) = answered.terminal.expect("a terminal and its lease");

            master.write_all(b"echo typed\r").unwrap();
            host.until(|_, typed| typed.ends_with(b"echo typed\r"));
            host.says(&FromTerminal::Out(b"drawn by the host\r\n".to_vec()));
            assert!(drawn_until(&master, "drawn by the host").contains("drawn by the host"));

            resize(&master, 120, 40);
            host.until(|frame, _| {
                *frame
                    == ToTerminal::Resize {
                        cols: 120,
                        rows: 40,
                    }
            });

            host.says(&FromTerminal::Exited(0));
            assert_eq!(report(&lease), serde_json::json!({"ended": "exited"}));
        },
    );
}

#[test]
fn a_lost_stream_is_said_and_the_window_comes_back_to_the_hosts_session() {
    tilo_test_env::in_own_process(
        module_path!(),
        "a_lost_stream_is_said_and_the_window_comes_back_to_the_hosts_session",
        || {
            let dir = tempfile::tempdir().unwrap();
            let runtime = dir.path().join("run");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_RUNTIME_DIR", &runtime);
            env.set("TILO_IDENTITY_ROOT", dir.path().join("identity"));
            env.set("TILO_USH_IMAGES", dir.path().join("images"));
            let asks = session(&runtime);
            let socket = serving(dir.path());
            let window = Window::connect(&socket);

            window.open(None);
            let (_, stream) = asks.recv_timeout(tilo_test_env::hang_bound()).unwrap();
            let host = Host::opens(stream);
            let (_master, lease) = window.answer().terminal.unwrap();
            // The connection to the host goes.
            drop(host);
            let lost = report(&lease);
            assert_eq!(lost["ended"], "lost", "{lost}");

            // The window comes back to the session it lost, by the host's id for it.
            window.open(Some(SESSION));
            let (asked, stream) = asks.recv_timeout(tilo_test_env::hang_bound()).unwrap();
            assert_eq!(asked.attach.as_deref(), Some(SESSION));
            let mut host = Host::opens(stream);
            let answered = window.answer();
            assert_eq!(
                answered.reply["host_session"], SESSION,
                "{}",
                answered.reply
            );
            let (_master, lease) = answered.terminal.unwrap();
            host.says(&FromTerminal::Superseded);
            assert_eq!(report(&lease), serde_json::json!({"ended": "superseded"}));

            // A host that refuses is the window's answer: never this device's own environment.
            window.open(Some(SESSION));
            let (_, mut stream) = asks.recv_timeout(tilo_test_env::hang_bound()).unwrap();
            stream.write_all(&Reply::Hosted.line()).unwrap();
            stream
                .write_all(
                    &Reply::Refused {
                        reason: "the terminal session ended".into(),
                    }
                    .line(),
                )
                .unwrap();
            let refused = window.answer();
            assert!(refused.terminal.is_none());
            assert_eq!(refused.reply["result"], "denied", "{}", refused.reply);
            assert_eq!(refused.reply["reason"], "the terminal session ended");
        },
    );
}
