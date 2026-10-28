//! `tilolang-host`: the process the session starts as a tilolang host, for one room and author.
//!
//! It holds its door (`TILO_SVC_TILOLANG_HOST_FD`) and its storage
//! (`TILO_TILOLANG_HOST_STORAGE`) and nothing else. It waits on the door, its instances' windows
//! and its own wake, and runs a frame of every instance when one of them has something for it.
//! It ends when the session closes its door and its last window is gone.

use std::io::{Read as _, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use runtime_portal::tilolang_host::{STORAGE_ENV, receive_instance, send_ended, take_door};
use tilolang_host::TilolangHost;

fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let Some(door) = take_door() else {
        eprintln!("tilolang-host: no door: the session starts the tilolang host");
        return std::process::ExitCode::from(2);
    };
    let storage = std::env::var_os(STORAGE_ENV)
        .filter(|dir| !dir.is_empty())
        .map(std::path::PathBuf::from);
    let (wake_read, wake_write) = match UnixStream::pair() {
        Ok(pair) => pair,
        Err(error) => {
            eprintln!("tilolang-host: no wake: {error}");
            return std::process::ExitCode::from(2);
        }
    };
    if let Err(error) = wake_read
        .set_nonblocking(true)
        .and_then(|()| wake_write.set_nonblocking(true))
    {
        eprintln!("tilolang-host: no wake: {error}");
        return std::process::ExitCode::from(2);
    }
    let wake_write = Arc::new(wake_write);
    let wake: tilolang_tilo::background::Waker = Arc::new(move || {
        // A full pipe already wakes the host.
        let _ = (&*wake_write).write(&[1]);
    });
    let mut host = TilolangHost::new(storage, wake);
    let mut door = Some(door);
    loop {
        // What a frame changed may ask for another (a view that read what it changed), until
        // nothing changes: every change is a message taken in or a step made, so it settles.
        while host.turn() {}
        // The session stops the host once its last instance ended.
        for instance in host.take_ended() {
            if let Some(open) = door.as_ref()
                && let Err(error) = send_ended(open, instance)
            {
                tracing::warn!(%error, instance, "the session was not told an instance ended");
            }
        }
        if door.is_none() && host.is_empty() {
            return std::process::ExitCode::SUCCESS;
        }
        let ready = wait(
            door.as_ref().map(|door| door.as_fd()),
            wake_read.as_fd(),
            &host,
        );
        if ready.wake {
            let mut drained = [0_u8; 256];
            while matches!((&wake_read).read(&mut drained), Ok(n) if n > 0) {}
        }
        if ready.door
            && let Some(open) = door.as_ref()
        {
            match receive_instance(open) {
                Ok(Some((request, fds))) => {
                    let (app, instance) = (request.app.clone(), request.instance);
                    if let Err(error) = host.admit(request, fds) {
                        tracing::error!(app, %error, "an instance did not start");
                        let _ = send_ended(open, instance);
                    }
                }
                Ok(None) => door = None,
                Err(error) => tracing::warn!(%error, "the door carried something unreadable"),
            }
        }
    }
}

struct Ready {
    door: bool,
    wake: bool,
}

/// Wait until the door, the wake or a window has something.
fn wait(door: Option<BorrowedFd<'_>>, wake: BorrowedFd<'_>, host: &TilolangHost) -> Ready {
    let mut fds = vec![libc::pollfd {
        fd: wake.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    }];
    if let Some(door) = door {
        fds.push(libc::pollfd {
            fd: door.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });
    }
    for window in host.windows() {
        fds.push(libc::pollfd {
            fd: window.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });
    }
    let count = libc::nfds_t::try_from(fds.len()).unwrap_or(libc::nfds_t::MAX);
    // SAFETY: `fds` is a live array of initialized pollfds naming open descriptors.
    let polled = unsafe { libc::poll(fds.as_mut_ptr(), count, -1) };
    if polled < 0 {
        return Ready {
            door: false,
            wake: false,
        };
    }
    Ready {
        wake: fds.first().is_some_and(|fd| fd.revents != 0),
        door: door.is_some() && fds.get(1).is_some_and(|fd| fd.revents != 0),
    }
}
