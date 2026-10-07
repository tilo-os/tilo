//! Wake the native loop for ordered TERM/INT/HUP teardown, on any thread.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use smithay::reexports::calloop::{Interest, Mode, generic::Generic};

pub(super) struct SessionTermination {
    requested: Arc<AtomicBool>,
    signals: Vec<signal_hook_registry::SigId>,
}

impl SessionTermination {
    pub(super) fn source() -> io::Result<(Self, Generic<Arc<OwnedFd>>)> {
        // SAFETY: eventfd returns a fresh owned descriptor, with no pointer arguments.
        let raw = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the successful eventfd above transfers sole descriptor ownership.
        let fd = Arc::new(unsafe { OwnedFd::from_raw_fd(raw) });
        let mut guard = Self {
            requested: Arc::new(AtomicBool::new(false)),
            signals: Vec::new(),
        };
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            let writer = Arc::clone(&fd);
            let requested = Arc::clone(&guard.requested);
            // SAFETY: the handler only stores an atomic flag and performs a
            // nonblocking write(2), which is async-signal-safe. Captured Arcs
            // keep the descriptor and flag alive until the action unregisters.
            let id = unsafe {
                signal_hook_registry::register(signal, move || {
                    requested.store(true, Ordering::SeqCst);
                    let value = 1_u64.to_ne_bytes();
                    libc::write(writer.as_raw_fd(), value.as_ptr().cast(), value.len());
                })
            }?;
            guard.signals.push(id);
        }
        Ok((guard, Generic::new(fd, Interest::READ, Mode::Level)))
    }

    pub(super) fn requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }
}

impl Drop for SessionTermination {
    fn drop(&mut self) {
        for id in self.signals.drain(..) {
            signal_hook_registry::unregister(id);
        }
    }
}
