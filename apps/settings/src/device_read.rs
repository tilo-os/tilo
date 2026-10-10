//! A read of the device made off the UI thread.
//!
//! An area of Settings whose rows show what it reads of the device — its
//! audio graph, its Bluetooth adapter, its accounts — reads it with blocking
//! calls: commands, D-Bus, file scans. After a window's first frame its
//! warm-up ([`crate::effects::AreaReads`]) starts that read on the host's
//! background executor, and the area builds its model from the result on
//! the UI thread. An area asked for its model while its read still runs —
//! its pane opened, an effect or a row lookup reached it — waits for that
//! read rather than starting a second one, so the device is read once.

use std::cell::RefCell;
use std::sync::mpsc::{Receiver, TryRecvError};

/// One background read of a `T`, started at most once.
pub(crate) struct DeviceRead<T> {
    /// The read on its way, until its result is taken off the channel.
    pending: RefCell<Option<Receiver<T>>>,
    /// Its result, once taken off the channel and not yet adopted.
    done: RefCell<Option<T>>,
}

impl<T> Default for DeviceRead<T> {
    fn default() -> Self {
        Self {
            pending: RefCell::new(None),
            done: RefCell::new(None),
        }
    }
}

impl<T: Send + 'static> DeviceRead<T> {
    /// Start `read` on the background executor; the window is woken when it
    /// is done. `false` when a read was already started, or when the host
    /// runs no background work — the area then reads when it is asked.
    pub(crate) fn start(&self, read: impl FnOnce() -> T + Send + 'static) -> bool {
        if self.pending.borrow().is_some() || self.done.borrow().is_some() {
            return false;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let wake = ui::WakeHandle::new();
        let spawned = ui::background::Background::current().spawn("device read", move || {
            let _ = sender.send(read());
            wake.wake();
        });
        if spawned.is_err() {
            return false;
        }
        *self.pending.borrow_mut() = Some(receiver);
        true
    }

    /// Whether the read started has ended: its result is in, or it failed
    /// and the area will read when asked. `true` when none was started.
    pub(crate) fn is_finished(&self) -> bool {
        let mut pending = self.pending.borrow_mut();
        let Some(receiver) = pending.as_ref() else {
            return true;
        };
        match receiver.try_recv() {
            Ok(value) => {
                *self.done.borrow_mut() = Some(value);
                *pending = None;
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                *pending = None;
                true
            }
        }
    }

    /// The started read's result, waiting for it if it still runs. `None`
    /// when no read was started or it failed: the caller reads itself.
    pub(crate) fn take(&self) -> Option<T> {
        if let Some(value) = self.done.borrow_mut().take() {
            return Some(value);
        }
        let receiver = self.pending.borrow_mut().take()?;
        receiver.recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A read runs off the calling thread, its result is taken once, and a
    /// second start while it is held is refused.
    #[test]
    fn a_started_read_runs_off_the_calling_thread_once() {
        let read = DeviceRead::default();
        let caller = std::thread::current().id();
        assert!(read.start(move || std::thread::current().id() != caller));
        assert!(!read.start(|| false), "one read at a time");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !read.is_finished() {
            assert!(std::time::Instant::now() < deadline, "the read finishes");
            std::thread::yield_now();
        }
        assert_eq!(read.take(), Some(true), "the read ran on another thread");
        assert_eq!(read.take(), None, "a result is adopted once");
    }

    /// Asking for the result while the read runs waits for it rather than
    /// reading again.
    #[test]
    fn taking_a_running_read_waits_for_it() {
        let read = DeviceRead::default();
        let (release, gate) = std::sync::mpsc::channel::<()>();
        assert!(read.start(move || {
            let _ = gate.recv();
            7
        }));
        assert!(!read.is_finished(), "the read is held at its gate");
        release.send(()).unwrap();
        assert_eq!(read.take(), Some(7));
        assert!(read.is_finished(), "nothing is left on its way");
    }
}
