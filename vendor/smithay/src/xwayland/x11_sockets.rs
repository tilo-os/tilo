use std::{
    io::{ErrorKind, Read, Write},
    os::unix::net::UnixStream,
};

use tracing::{debug, info, warn};

use rustix::{io::Errno, net::SocketAddrUnix};

/// The display numbers searched for a free one: every number whose TCP port (6000 + N) exists, as
/// an X server's own `-displayfd` search allows. Each process that runs X11 programs on a display
/// of its own takes one, so a fixed handful would run out on a busy machine.
const DISPLAYS: std::ops::Range<u32> = 0..(65536 - 6000);

/// Find a free X11 display slot and setup
pub(crate) fn prepare_x11_sockets(
    display: Option<u32>,
    open_abstract_socket: bool,
) -> Result<(X11Lock, Vec<UnixStream>), std::io::Error> {
    match display {
        Some(d) => {
            if let Some(prepared) = prepare_display(d, open_abstract_socket)? {
                return Ok(prepared);
            }
        }
        None => {
            for d in DISPLAYS {
                match prepare_display(d, open_abstract_socket) {
                    Ok(Some(prepared)) => return Ok(prepared),
                    Ok(None) => {}
                    Err(err) => warn!(display = d, "Failed to create sockets: {}", err),
                }
            }
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::AddrInUse,
        "Could not find a free socket for the XServer.",
    ))
}

/// Take display `d` with its lock and sockets, or `None` when it is someone else's.
fn prepare_display(
    d: u32,
    open_abstract_socket: bool,
) -> std::io::Result<Option<(X11Lock, Vec<UnixStream>)>> {
    let Some(reservation) = reserve(d, open_abstract_socket)? else {
        debug!(display = d, "Display taken");
        return Ok(None);
    };
    let Some(lock) = X11Lock::grab(d, reservation.clone_held()?)? else {
        debug!(display = d, "Failed to acquire lock");
        return Ok(None);
    };
    let path = format!("/tmp/.X11-unix/X{}", d);
    let _ = ::std::fs::remove_file(&path);
    // We know this path is not too long, these unwrap cannot fail
    let fs_addr = SocketAddrUnix::new(path.as_bytes()).unwrap();
    let mut sockets = vec![open_socket(fs_addr, true)?];
    sockets.extend(reservation.listening());
    Ok(Some((lock, sockets)))
}

/// What holds a display number for one process alone.
///
/// On Linux, the display's abstract socket name: binding it is atomic between processes, and the
/// kernel lets go of it with the last descriptor of the socket, so a process that was killed
/// leaves nothing that keeps the number taken. The lock file alone cannot do this: a lock left by
/// a dead process has to be removed by whoever finds it, and two processes finding it at once
/// both removed it and both took the display, the second removing the first one's files as it
/// gave up.
struct Reservation {
    #[cfg(target_os = "linux")]
    socket: UnixStream,
    /// Whether the abstract socket listens for clients too.
    #[cfg(target_os = "linux")]
    listens: bool,
}

#[cfg(target_os = "linux")]
fn reserve(d: u32, listen: bool) -> std::io::Result<Option<Reservation>> {
    let path = format!("/tmp/.X11-unix/X{}", d);
    let addr = SocketAddrUnix::new_abstract_name(path.as_bytes()).unwrap();
    // A name that does not listen refuses its clients, which then use the socket file.
    match open_socket(addr, listen) {
        Ok(socket) => Ok(Some(Reservation {
            socket,
            listens: listen,
        })),
        Err(Errno::ADDRINUSE) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

#[cfg(not(target_os = "linux"))]
fn reserve(_d: u32, _listen: bool) -> std::io::Result<Option<Reservation>> {
    Ok(Some(Reservation {}))
}

impl Reservation {
    /// The descriptor the lock keeps for as long as the display is held.
    fn clone_held(&self) -> std::io::Result<Option<UnixStream>> {
        #[cfg(target_os = "linux")]
        return self.socket.try_clone().map(Some);
        #[cfg(not(target_os = "linux"))]
        Ok(None)
    }

    /// The socket clients connect to, when the display listens on its abstract name.
    fn listening(self) -> Option<UnixStream> {
        #[cfg(target_os = "linux")]
        return self.listens.then_some(self.socket);
        #[cfg(not(target_os = "linux"))]
        None
    }
}

#[derive(Debug)]
pub(crate) struct X11Lock {
    display: u32,
    /// The display's reservation, held until the lock's files are gone.
    _held: Option<UnixStream>,
}

impl X11Lock {
    /// Write the lock file of display `number`, held by `held`: `None` when a live X server that
    /// does not reserve the abstract name holds the number.
    ///
    /// The file is written whole under a name of this process's own and then linked into place,
    /// so nobody ever reads a lock without its process id. A lock whose process is gone is
    /// replaced in one rename.
    fn grab(number: u32, held: Option<UnixStream>) -> std::io::Result<Option<X11Lock>> {
        debug!(display = number, "Attempting to aquire an X11 display lock");
        let filename = format!("/tmp/.X{}-lock", number);
        let pid = rustix::process::Pid::as_raw(Some(rustix::process::getpid()));
        let written = format!("/tmp/.X{}-lock.{}", number, pid);
        let _ = ::std::fs::remove_file(&written);
        let wrote = ::std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&written)
            .and_then(|mut file| file.write_all(format!("{:>10}\n", pid).as_bytes()));
        let taken = wrote.and_then(|()| match ::std::fs::hard_link(&written, &filename) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                if Self::holder_alive(&filename) {
                    Ok(false)
                } else {
                    debug!(display = number, "Lock was left by a defunct X11 server, replacing it");
                    ::std::fs::rename(&written, &filename).map(|()| true)
                }
            }
            Err(err) => Err(err),
        });
        let _ = ::std::fs::remove_file(&written);
        if !taken? {
            return Ok(None);
        }
        debug!(display = number, "X11 lock acquired");
        Ok(Some(X11Lock {
            display: number,
            _held: held,
        }))
    }

    /// Whether the process named in the lock file `filename` runs. A lock that cannot be read, or
    /// is still being written by an X server that writes it in place, counts as held.
    fn holder_alive(filename: &str) -> bool {
        let mut spid = String::new();
        let read = ::std::fs::File::open(filename).and_then(|mut file| file.read_to_string(&mut spid));
        if read.is_err() {
            return true;
        }
        let Some(pid) = spid
            .trim()
            .parse::<i32>()
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        else {
            return true;
        };
        !matches!(rustix::process::test_kill_process(pid), Err(Errno::SRCH))
    }

    pub(crate) fn display_number(&self) -> u32 {
        self.display
    }
}

impl Drop for X11Lock {
    fn drop(&mut self) {
        info!("Cleaning up X11 lock.");
        // Cleanup all the X11 files, while the reservation still keeps the display from anyone
        // else
        if let Err(e) = ::std::fs::remove_file(format!("/tmp/.X11-unix/X{}", self.display)) {
            warn!(error = ?e, "Failed to remove X11 socket");
        }
        if let Err(e) = ::std::fs::remove_file(format!("/tmp/.X{}-lock", self.display)) {
            warn!(error = ?e, "Failed to remove X11 lockfile");
        }
    }
}

/// Open an unix socket bound to given address, listening for clients when `listen`
fn open_socket(addr: SocketAddrUnix, listen: bool) -> rustix::io::Result<UnixStream> {
    // create an unix stream socket
    let fd = rustix::net::socket_with(
        rustix::net::AddressFamily::UNIX,
        rustix::net::SocketType::STREAM,
        rustix::net::SocketFlags::CLOEXEC,
        None,
    )?;
    // bind it to requested address
    rustix::net::bind(&fd, &addr)?;
    if listen {
        // Deep enough that several apps opening at once into a bound display are
        // all queued while the server starts: a queued client waits, a refused
        // one fails to open the display at all.
        rustix::net::listen(&fd, 128)?;
    }
    Ok(UnixStream::from(fd))
}
