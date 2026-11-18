//! The launch's own X11 display, served by an X server that is a client of the keeper.
//!
//! An X11 program of a kept launch must outlive the desktop just as its Wayland windows do, so
//! its X server cannot be the desktop's: the keeper binds a display of its own (`--x11`), and
//! `xwayland-satellite` (a rootless Xwayland and the window manager that turns its windows into
//! ordinary toplevels) runs as one more client of the keeper's Wayland socket. X11 windows are
//! then windows of the keeper like any other, held and shown again when the desktop restarts.
//!
//! The keeper binds the display's sockets and leaves them listening; the server starts only when
//! an X client first connects, and is handed the sockets (`-listenfd`), so a launch that never
//! uses X runs no X server. The server lives as long as the keeper. When it ends (it crashed),
//! its X clients are gone with it, as they would be with any X server, and the next client to
//! connect starts a new one on the same display; the keeper itself runs on.
//!
//! The display is this launch's alone. An X server isolates none of its clients from each other,
//! so a client must show the cookie in the keeper's private directory (`Xauthority`, handed to
//! the app as `XAUTHORITY`): another user, or another launch, cannot reach the launch's X11
//! programs through the display's socket. The display's name is kept beside it as `x11`, so a
//! session serving input methods to X11 programs finds the display and its cookie in the launch's
//! directory, and `x11-server` while its server is up: serving a display whose server is not up yet
//! would start one the app never asked for.

use std::io;
use std::os::fd::{AsFd as _, AsRawFd as _, RawFd};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use smithay::reexports::calloop::RegistrationToken;
use smithay::xwayland::XWaylandListener;

use crate::Data;

/// The program that serves the display.
const SERVER: &str = "xwayland-satellite";

/// A server that ends sooner than this after it started never served: the clients waiting for
/// it are let go before the display listens again, or they would start it again at once.
const FAILED_START: Duration = Duration::from_secs(2);

/// The display and the server on it while one runs.
pub(crate) struct XDisplay {
    listener: XWaylandListener,
    /// The display's cookie, in the keeper's directory.
    authority: PathBuf,
    cookie: [u8; 16],
    /// What watches the sockets for a client while no server runs.
    watching: Vec<RegistrationToken>,
    server: Option<Server>,
}

struct Server {
    child: Child,
    token: Option<RegistrationToken>,
    started: Instant,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl XDisplay {
    /// Bind a free display, its cookie written into the keeper's directory `dir`.
    pub(crate) fn bind(dir: &Path) -> io::Result<Self> {
        socket_directory()?;
        let listener = XWaylandListener::prepare(None, true)?;
        let authority = dir.join("Xauthority");
        match std::fs::remove_file(&authority) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        let mut cookie = [0_u8; 16];
        rustix::rand::getrandom(&mut cookie, rustix::rand::GetRandomFlags::empty())?;
        tilo_files::native::x11_authority::write(&authority, listener.display_number(), &cookie)?;
        // Named beside the cookie, like the control socket: a session that serves the display
        // to an input method reads it here, and finds the cookie next to it.
        let named = dir.join(".x11");
        std::fs::write(&named, format!(":{}", listener.display_number()))?;
        std::fs::rename(&named, dir.join("x11"))?;
        Ok(Self {
            listener,
            authority,
            cookie,
            watching: Vec::new(),
            server: None,
        })
    }

    /// `N` of the display `:N`.
    pub(crate) fn number(&self) -> u32 {
        self.listener.display_number()
    }

    /// The process id of the server, while there is one.
    pub(crate) fn server_pid(&self) -> Option<u32> {
        self.server.as_ref().map(|server| server.child.id())
    }

    /// The file a client finds the display's cookie in.
    pub(crate) fn authority(&self) -> &Path {
        &self.authority
    }
}

/// The directory X servers listen in, made if missing as the X server package would.
fn socket_directory() -> io::Result<()> {
    const DIRECTORY: &str = "/tmp/.X11-unix";
    match std::fs::create_dir(DIRECTORY) {
        Ok(()) => {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(DIRECTORY, std::fs::Permissions::from_mode(0o1777))
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

/// Watch the display's sockets for a client waiting to connect.
pub(crate) fn watch(data: &mut Data) {
    let Some(display) = data.x11.as_mut() else {
        return;
    };
    for socket in display.listener.sockets() {
        let watched = socket
            .try_clone()
            .and_then(|socket| crate::insert(&data.handle, socket, |_, data| client_waiting(data)));
        match watched {
            Ok(token) => display.watching.push(token),
            Err(error) => tracing::warn!(%error, "the X11 display is not watched"),
        }
    }
}

/// A client waits on the display: start the server it waits for.
fn client_waiting(data: &mut Data) {
    let Some(display) = data.x11.as_mut() else {
        return;
    };
    for token in display.watching.drain(..) {
        data.handle.remove(token);
    }
    let started = spawn(display, &data.socket, &data.options.dir).and_then(|mut child| {
        let pidfd = crate::child::pidfd_open(child.id());
        match pidfd.and_then(|pidfd| crate::insert(&data.handle, pidfd, |_, data| ended(data))) {
            Ok(token) => Ok((child, token)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(error)
            }
        }
    });
    match started {
        Ok((child, token)) => {
            let number = display.number();
            tracing::info!(number, "the launch's X server starts");
            // What a session serving input methods reads to find a display it can serve now.
            if let Err(error) = std::fs::write(
                data.options.dir.join("x11-server"),
                display.number().to_string(),
            ) {
                tracing::warn!(%error, "the running X server is not named in the keeper's directory");
            }
            display.server = Some(Server {
                child,
                token: Some(token),
                started: Instant::now(),
            });
            let cookie = display.cookie.to_vec();
            start_drag_and_drop(data, number, cookie);
        }
        Err(error) => {
            tracing::warn!(%error, "the launch's X server did not start: no X11 programs");
            refuse_waiting(display);
            watch(data);
        }
    }
}

/// Drag-and-drop with the new server's programs, on a connection of the keeper's own: it waits
/// for the server off the loop.
fn start_drag_and_drop(data: &mut Data, number: u32, cookie: Vec<u8>) {
    let (sender, heard) = smithay::reexports::calloop::channel::channel();
    let inserted = data.handle.insert_source(heard, |event, (), data| {
        if let smithay::reexports::calloop::channel::Event::Msg(heard) = event {
            data.state.xdnd_heard(heard);
        }
    });
    if let Err(error) = inserted {
        tracing::warn!(error = %error.error, "no drag-and-drop with the launch's X11 programs");
        return;
    }
    match crate::xdnd::start(number, cookie, sender) {
        Ok(bridge) => data.state.xdnd.started(bridge),
        Err(error) => tracing::warn!(%error, "no drag-and-drop with the launch's X11 programs"),
    }
}

/// The server ended: the display listens for the next client.
fn ended(data: &mut Data) {
    let Some(display) = data.x11.as_mut() else {
        return;
    };
    let Some(mut server) = display.server.take() else {
        return;
    };
    match server.child.try_wait() {
        Ok(Some(status)) => {
            tracing::warn!(%status, "the launch's X server ended; its X clients are gone");
        }
        Ok(None) => {
            display.server = Some(server);
            return;
        }
        Err(error) => tracing::warn!(%error, "the launch's X server cannot be waited for"),
    }
    if let Some(token) = server.token.take() {
        data.handle.remove(token);
    }
    let _ = std::fs::remove_file(data.options.dir.join("x11-server"));
    if server.started.elapsed() < FAILED_START {
        refuse_waiting(display);
    }
    drop(server);
    watch(data);
}

/// Start the server on `display`'s sockets as a client of the keeper's `socket`.
fn spawn(display: &XDisplay, socket: &Path, dir: &Path) -> io::Result<Child> {
    let fds: Vec<RawFd> = display
        .listener
        .sockets()
        .iter()
        .map(|socket| socket.as_fd().as_raw_fd())
        .collect();
    let mut command = Command::new(SERVER);
    command
        .arg(format!(":{}", display.number()))
        .arg("-auth")
        .arg(display.authority());
    for fd in &fds {
        command.arg("-listenfd").arg(fd.to_string());
    }
    command
        .env("WAYLAND_DISPLAY", socket)
        // Its own socket for Xwayland goes in the keeper's private directory.
        .env("XDG_RUNTIME_DIR", dir)
        .env_remove("WAYLAND_SOCKET")
        .env_remove("DISPLAY")
        .env_remove("XAUTHORITY")
        // It would report itself ready to the service manager of whatever started the keeper.
        .env_remove("NOTIFY_SOCKET")
        .stdin(Stdio::null());
    let keeper = std::process::id();
    // SAFETY: only async-signal-safe calls between fork and exec: signal, prctl, getppid,
    // close_range, fcntl and _exit.
    unsafe {
        command.pre_exec(move || {
            // The keeper ignores SIGPIPE; the server starts with the default.
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(io::Error::last_os_error());
            }
            if u32::try_from(libc::getppid()).ok() != Some(keeper) {
                libc::_exit(1);
            }
            // What the keeper was handed for its app (a sandbox's descriptors, a password) stays
            // with the app.
            if libc::syscall(
                libc::SYS_close_range,
                3,
                libc::c_uint::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            ) != 0
            {
                return Err(io::Error::last_os_error());
            }
            // The sockets are inherited by the server alone.
            for fd in &fds {
                if libc::fcntl(*fd, libc::F_SETFD, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    command.spawn()
}

/// Let go of the clients waiting on `display` when no server will take them.
fn refuse_waiting(display: &XDisplay) {
    for socket in display.listener.sockets() {
        loop {
            let mut fds = [rustix::event::PollFd::new(
                socket,
                rustix::event::PollFlags::IN,
            )];
            match rustix::event::poll(
                &mut fds,
                Some(&rustix::event::Timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                }),
            ) {
                Ok(1..) => {}
                _ => break,
            }
            if rustix::net::accept(socket).is_err() {
                break;
            }
        }
    }
}
