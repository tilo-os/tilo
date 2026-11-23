//! The keeper as its own process, as the launcher runs it: whatever the desktop, a session or
//! its own output streams do, it keeps its app and keeps serving the next session.

use std::io::{IoSlice, Read as _, Write as _};
use std::mem::MaybeUninit;
use std::os::fd::AsFd as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tilo_app_keeper::wire::{self, End, FromKeeper, ToKeeper};
use tilo_test_env::SpawnUnderEnvLock as _;

/// A desktop dispatched on a thread of its own for as long as the value lives.
struct Desktop {
    connector: tilo_compositor::ClientConnector,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Desktop {
    fn new() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let (sender, connector) = std::sync::mpsc::channel();
        let stopped = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let mut compositor = crate::support::desktop(640, 480);
            let _ = sender.send(compositor.client_connector());
            while !stopped.load(Ordering::Acquire) {
                let _ = compositor.dispatch(Duration::from_millis(5));
            }
        });
        Self {
            connector: connector.recv().expect("the desktop's connector"),
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The keeper program, started with piped output streams.
struct Keeper {
    child: Child,
    dir: PathBuf,
    _scratch: tempfile::TempDir,
}

impl Keeper {
    /// A keeper whose app runs until `<scratch>/stop` exists, then exits 3.
    fn start() -> Self {
        Self::with_script("while [ ! -e '@@STOP@@' ]; do sleep 0.02; done; exit 3")
    }

    /// [`Self::start`] with the app's `script`, whose `@@STOP@@` is the file that ends it.
    fn with_script(script: &str) -> Self {
        let scratch = tempfile::tempdir().expect("a directory");
        let dir = scratch.path().join("keeper");
        let stop = scratch.path().join("stop");
        let script = script.replace("@@STOP@@", &stop.display().to_string());
        let child = Command::new(env!("CARGO_BIN_EXE_tilo-app-keeper"))
            .arg("--dir")
            .arg(&dir)
            .arg("--launch")
            .arg("process-test")
            .arg("--")
            .arg("sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn_locked()
            .expect("the keeper starts");
        let started = Instant::now();
        while !dir.join("control").exists() {
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "no control socket"
            );
            std::thread::park_timeout(Duration::from_millis(10));
        }
        Self {
            child,
            dir,
            _scratch: scratch,
        }
    }

    /// Close the keeper's output streams, as a launcher whose session went away leaves them.
    fn close_output(&mut self) {
        drop(self.child.stdout.take());
        drop(self.child.stderr.take());
    }

    fn end_app(&self) {
        let stop = self.dir.parent().expect("scratch").join("stop");
        std::fs::write(stop, b"").expect("stop the app");
    }

    fn running(&mut self) -> bool {
        let status = self.child.try_wait().expect("the keeper can be waited for");
        if let Some(status) = status {
            eprintln!("the keeper ended: {status:?}");
        }
        status.is_none()
    }

    fn wait_for_record(&mut self) {
        let started = Instant::now();
        while !self.dir.join("ended").exists() {
            assert!(self.running(), "the keeper died before recording the end");
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "no end recorded"
            );
            std::thread::park_timeout(Duration::from_millis(10));
        }
    }

    /// It stays up for a while: it waits its grace period for a session.
    fn stays_up(&mut self) {
        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(500) {
            assert!(self.running(), "the keeper died");
            std::thread::park_timeout(Duration::from_millis(20));
        }
    }
}

impl Drop for Keeper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A greeted control connection.
struct Control {
    stream: UnixStream,
    buffer: Vec<u8>,
    frames: std::collections::VecDeque<Vec<u8>>,
}

impl Control {
    fn raw(dir: &Path) -> UnixStream {
        tilo_files::native::socket::connect(&dir.join("control")).expect("the control socket")
    }

    fn greet(dir: &Path) -> Self {
        let stream = Self::raw(dir);
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .expect("a timeout");
        let mut control = Self {
            stream,
            buffer: Vec::new(),
            frames: std::collections::VecDeque::new(),
        };
        control.send(&ToKeeper::Hello {
            version: wire::INTERFACE_VERSION,
        });
        assert_eq!(
            control.receive(),
            FromKeeper::Hello {
                version: wire::INTERFACE_VERSION
            }
        );
        control
    }

    fn send(&mut self, message: &ToKeeper) {
        let frame = wire::encode(message).expect("a frame");
        self.stream.write_all(&frame).expect("sent");
    }

    fn attach(&self, desktop: &Desktop, keeper_pid: u32) {
        let socket = desktop
            .connector
            .connect_for(keeper_pid)
            .expect("a connection");
        let frame = wire::encode(&ToKeeper::Attach).expect("a frame");
        let fds = [socket.as_fd()];
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut control = rustix::net::SendAncillaryBuffer::new(&mut space);
        assert!(control.push(rustix::net::SendAncillaryMessage::ScmRights(&fds)));
        rustix::net::sendmsg(
            &self.stream,
            &[IoSlice::new(&frame)],
            &mut control,
            rustix::net::SendFlags::empty(),
        )
        .expect("attached");
    }

    fn receive(&mut self) -> FromKeeper {
        loop {
            self.frames
                .extend(wire::take_frames(&mut self.buffer).expect("frames"));
            if let Some(frame) = self.frames.pop_front() {
                return serde_json::from_slice(&frame).expect("a message");
            }
            let mut bytes = [0_u8; 4096];
            let read = self.stream.read(&mut bytes).expect("the keeper answers");
            assert!(read > 0, "the keeper closed the connection");
            self.buffer.extend(&bytes[..read]);
        }
    }

    fn wait_for<T>(&mut self, mut want: impl FnMut(FromKeeper) -> Option<T>) -> T {
        loop {
            if let Some(found) = want(self.receive()) {
                return found;
            }
        }
    }

    /// The keeper's description, once `ready` holds for it.
    fn described_when(&mut self, ready: impl Fn(&wire::Description) -> bool) -> wire::Description {
        let started = Instant::now();
        loop {
            self.send(&ToKeeper::Describe);
            let described = self.wait_for(|message| match message {
                FromKeeper::Described(described) => Some(described),
                _ => None,
            });
            if ready(&described) {
                return described;
            }
            assert!(started.elapsed() < Duration::from_secs(20), "never ready");
            std::thread::park_timeout(Duration::from_millis(20));
        }
    }
}

/// A keeper attached to a desktop, its app running.
fn shown() -> (Keeper, Desktop, Control) {
    let keeper = Keeper::start();
    let desktop = Desktop::new();
    let mut control = Control::greet(&keeper.dir);
    control.attach(&desktop, keeper.child.id());
    control.described_when(|described| described.attached && described.app_pid.is_some());
    (keeper, desktop, control)
}

/// A later session still gets everything: the app's end once it ends.
fn a_later_session_hears_the_end(keeper: &mut Keeper) {
    keeper.stays_up();
    let mut control = Control::greet(&keeper.dir);
    let described = control.described_when(|described| described.app_pid.is_some());
    assert_eq!(described.ended, None, "the app still runs");
    keeper.end_app();
    let end = control.wait_for(|message| match message {
        FromKeeper::Ended { end } => Some(end),
        _ => None,
    });
    assert_eq!(end, End::Exited { code: 3 });
}

/// What the keeper says of itself goes to its own bounded log in its directory: the streams the
/// launcher passes on carry the app's words alone, which is what a failed launch is read for.
#[test]
fn a_keeper_that_logs_leaves_the_apps_streams_alone() {
    let mut keeper = Keeper::with_script(
        "printf 'said out\\n'; printf 'said err\\n' >&2; while [ ! -e '@@STOP@@' ]; do sleep 0.02; done; exit 3",
    );
    let desktop = Desktop::new();
    let mut control = Control::greet(&keeper.dir);
    control.attach(&desktop, keeper.child.id());
    control.described_when(|described| described.attached && described.app_pid.is_some());
    keeper.end_app();
    control.wait_for(|message| matches!(message, FromKeeper::Ended { .. }).then_some(()));
    // A session heard the end, so the keeper leaves.
    let started = Instant::now();
    let status = loop {
        if let Some(status) = keeper
            .child
            .try_wait()
            .expect("the keeper can be waited for")
        {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the keeper did not end"
        );
        std::thread::park_timeout(Duration::from_millis(10));
    };
    assert!(status.success(), "{status:?}");
    let mut said_out = String::new();
    let mut said_err = String::new();
    keeper
        .child
        .stdout
        .take()
        .expect("its output")
        .read_to_string(&mut said_out)
        .expect("what the app wrote");
    keeper
        .child
        .stderr
        .take()
        .expect("its error stream")
        .read_to_string(&mut said_err)
        .expect("what the app wrote");
    assert_eq!(said_out, "said out\n", "the app's output alone");
    assert_eq!(
        said_err, "said err\n",
        "the app's errors alone: no line of the keeper's"
    );
    let log = std::fs::read_to_string(keeper.dir.join("keeper.log")).expect("the keeper's log");
    assert!(
        log.contains("the app ended"),
        "the keeper said how the app ended in its own log: {log}"
    );
}

#[test]
fn an_app_that_ends_while_nobody_listens_is_recorded_and_waited_for_with_output_closed() {
    let (mut keeper, desktop, control) = shown();
    keeper.close_output();
    drop(control);
    drop(desktop);
    keeper.end_app();
    keeper.wait_for_record();
    keeper.stays_up();
    let mut control = Control::greet(&keeper.dir);
    let end = control.wait_for(|message| match message {
        FromKeeper::Ended { end } => Some(end),
        _ => None,
    });
    assert_eq!(end, End::Exited { code: 3 });
}

#[test]
fn a_desktop_that_dies_and_closed_output_leave_the_app_running() {
    let (mut keeper, desktop, control) = shown();
    keeper.close_output();
    drop(desktop);
    drop(control);
    a_later_session_hears_the_end(&mut keeper);
}

#[test]
fn control_peers_that_vanish_mid_message_or_send_garbage_are_dropped_alone() {
    let (mut keeper, _desktop, _control) = shown();
    // Half a frame, then gone.
    let mut half = Control::raw(&keeper.dir);
    let frame = wire::encode(&ToKeeper::Hello {
        version: wire::INTERFACE_VERSION,
    })
    .expect("a frame");
    half.write_all(&frame[..frame.len() / 2])
        .expect("half sent");
    drop(half);
    // Not a frame at all.
    let mut garbage = Control::raw(&keeper.dir);
    let _ = garbage.write_all(&[0xff; 64]);
    // A prefix announcing more than a frame may hold.
    let mut oversized = Control::raw(&keeper.dir);
    let _ = oversized.write_all(&u32::MAX.to_le_bytes());
    // Greeted, then garbage inside a valid frame.
    let mut greeted = Control::greet(&keeper.dir);
    let _ = greeted
        .stream
        .write_all(&wire::FRAMING.encode(b"\xff\xfe not json").expect("a frame"));
    // Asks for a description and never reads it, then vanishes.
    let mut deaf = Control::greet(&keeper.dir);
    for _ in 0..64 {
        deaf.send(&ToKeeper::Describe);
    }
    drop(deaf);
    drop(garbage);
    drop(oversized);
    drop(greeted);
    a_later_session_hears_the_end(&mut keeper);
}

#[test]
fn a_keeper_whose_session_never_reads_keeps_serving_the_next() {
    let (mut keeper, _desktop, mut control) = shown();
    // A session that asks and asks and never reads what it is told: the keeper gives up on it
    // instead of waiting on it.
    control.stream.set_nonblocking(true).expect("non-blocking");
    let frame = wire::encode(&ToKeeper::Describe).expect("a frame");
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(3) {
        match control.stream.write(&frame) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::park_timeout(Duration::from_millis(5));
            }
            Err(_) => break,
        }
    }
    drop(control);
    a_later_session_hears_the_end(&mut keeper);
}

#[test]
fn a_desktop_that_stops_reading_and_dies_mid_write_is_held_and_the_next_shows_the_app() {
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const RED: [u8; 4] = [255, 0, 0, 255];
    let mut rig = crate::support::Rig::new(None);
    let mut app = crate::support::App::connect(&mut rig);
    app.open_window(&mut rig, "Busy", Some("busy"));
    app.show(&mut rig, (100, 80), GREEN);
    // The desktop hangs: its connection stays open and nothing reads it, so what the keeper
    // sends piles up, more than the socket holds.
    let hung = rig.desktop.take();
    let surface = app.surface.clone().expect("a window");
    let buffer = app.buffer((100, 80), RED);
    for frame in 0..6000 {
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, 100, 80);
        surface.commit();
        if frame % 50 == 0 {
            rig.settle(&mut [&mut app]);
        }
    }
    // And then it is gone, with all of that unread.
    drop(hung);
    rig.settle(&mut [&mut app]);
    assert!(!app.broken, "the app keeps its connection");
    rig.attach_new(640, 480);
    rig.pump(&mut [&mut app], |rig, _| !rig.windows().is_empty());
    let (_, _, (x, y, _, _)) = rig.windows()[0];
    assert_eq!(rig.pixel((x + 50) as u32, (y + 40) as u32), RED);
}

#[test]
fn a_client_that_vanishes_mid_request_or_speaks_garbage_ends_alone() {
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    let mut rig = crate::support::Rig::new(None);
    let mut app = crate::support::App::connect(&mut rig);
    app.open_window(&mut rig, "Steady", Some("steady"));
    app.show(&mut rig, (100, 80), GREEN);
    // wl_display.get_registry (object 1, opcode 1, 12 bytes), cut off after its header.
    let mut cut = tilo_files::native::socket::connect(rig.keeper.socket()).expect("connects");
    cut.write_all(&[1, 0, 0, 0, 1, 0, 12, 0])
        .expect("half a request");
    drop(cut);
    // A request on an object that does not exist, and one longer than any message.
    let mut garbage = tilo_files::native::socket::connect(rig.keeper.socket()).expect("connects");
    garbage
        .write_all(&[99, 0, 0, 0, 7, 0, 8, 0, 1, 0, 0, 0, 1, 0, 0xff, 0xff])
        .expect("garbage");
    // A window made and its client gone before the request that shows it.
    let mut gone = crate::support::App::connect(&mut rig);
    gone.open_window(&mut rig, "Gone", Some("gone"));
    let half = gone.buffer((10, 10), GREEN);
    gone.surface
        .as_ref()
        .expect("a window")
        .attach(Some(&half), 0, 0);
    let _ = gone.connection.flush();
    drop(gone);
    rig.settle(&mut [&mut app]);
    drop(garbage);
    rig.settle(&mut [&mut app]);
    assert!(!app.broken);
    let windows = rig.windows();
    assert_eq!(
        windows.len(),
        1,
        "only the steady app's window: {windows:?}"
    );
    assert_eq!(windows[0].1.as_deref(), Some("Steady"));
}

/// The `/tmp/.X<n>-lock` a live X11 display holds for `pid`, once there is one.
fn x11_lock_of(pid: u32) -> PathBuf {
    let started = Instant::now();
    loop {
        let held = std::fs::read_dir("/tmp")
            .expect("/tmp")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".X") && name.ends_with("-lock"))
            })
            .find(|path| {
                std::fs::read_to_string(path)
                    .is_ok_and(|held| held.trim().parse::<u32>().ok() == Some(pid))
            });
        if let Some(lock) = held {
            return lock;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the keeper holds no X11 lock"
        );
        std::thread::park_timeout(Duration::from_millis(10));
    }
}

/// A login's end tells the keeper to end with SIGTERM: its app ends with it, and the display it
/// served leaves no lock or socket behind in `/tmp`, where they would pile up login after login.
#[test]
fn a_keeper_told_to_end_ends_its_app_and_lets_its_x11_display_go() {
    let scratch = tempfile::tempdir().expect("a directory");
    let dir = scratch.path().join("keeper");
    let mut child = Command::new(env!("CARGO_BIN_EXE_tilo-app-keeper"))
        .arg("--dir")
        .arg(&dir)
        .arg("--launch")
        .arg("process-test")
        .arg("--x11")
        .arg("--")
        .arg("sleep")
        .arg("600")
        .stdin(Stdio::null())
        .spawn_locked()
        .expect("the keeper starts");
    let lock = x11_lock_of(child.id());
    let number = lock
        .to_string_lossy()
        .trim_start_matches("/tmp/.X")
        .trim_end_matches("-lock")
        .to_owned();
    let socket = PathBuf::from(format!("/tmp/.X11-unix/X{number}"));
    assert!(socket.exists(), "the display listens");
    let desktop = Desktop::new();
    Control::greet(&dir).attach(&desktop, child.id());
    let started = Instant::now();
    let app = loop {
        if let Some(app) = std::fs::read(dir.join("app"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<wire::AppProcess>(&bytes).ok())
        {
            break app;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the app never started"
        );
        std::thread::park_timeout(Duration::from_millis(10));
    };

    // SAFETY: kill takes a pid and a signal number.
    let sent = unsafe { libc::kill(libc::pid_t::try_from(child.id()).unwrap(), libc::SIGTERM) };
    assert_eq!(sent, 0);
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("the keeper can be waited for") {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the keeper did not end"
        );
        std::thread::park_timeout(Duration::from_millis(10));
    };
    assert!(status.success(), "it ended as it chose to: {status:?}");
    assert!(
        !Path::new(&format!("/proc/{}", app.pid)).exists(),
        "its app ended with it"
    );
    assert!(!lock.exists(), "no X11 lock is left behind");
    assert!(!socket.exists(), "no X11 socket is left behind");
}
