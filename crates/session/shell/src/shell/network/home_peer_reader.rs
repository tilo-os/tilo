//! The disk inputs of the periodic home-peer refresh, read off the compositor
//! thread.
//!
//! Every two seconds the shell re-derives who its home peers are from three
//! files another process writes: the home-peer registry, the enrolment list
//! and the personal rooms file. Reading them on the compositor's loop put it
//! in uninterruptible sleep whenever the disk was busy (the loop's
//! `network-tick` checkpoint took up to 3.5 s). A worker reads them instead
//! and publishes the newest result; the loop asks for a read, polls for a
//! result and applies it without touching the disk.
//!
//! A result is at most one read old when applied, and a read starts at most
//! one refresh interval after the change it observes, so a change made by
//! another process reaches the loop within one interval plus one read. A
//! change this process queued on the state writer (the rooms export) is read
//! back: the production reader waits for the writer first.

use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex};

/// A worker that runs `read` when asked and keeps the newest result.
pub(super) struct HomePeerReader<T> {
    wake: SyncSender<()>,
    published: Arc<Mutex<Option<T>>>,
}

impl<T: Send + 'static> HomePeerReader<T> {
    /// Start the worker. It ends when the reader is dropped.
    pub(super) fn spawn(read: impl Fn() -> T + Send + 'static) -> Self {
        let (wake, asked) = sync_channel::<()>(1);
        let published = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&published);
        let spawned = std::thread::Builder::new()
            .name("home-peer-reader".to_owned())
            .spawn(move || {
                // One read serves every request made while it ran.
                for () in asked {
                    let result = read();
                    if let Ok(mut slot) = slot.lock() {
                        *slot = Some(result);
                    }
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "home-peer reader did not start");
        }
        Self { wake, published }
    }

    /// Ask for a read. Never waits: a request made while one is queued adds
    /// nothing.
    pub(super) fn request(&self) {
        // Full: a read is already queued. Disconnected: the worker never started.
        let _ = self.wake.try_send(());
    }

    /// The newest result not yet taken, if the worker published one.
    pub(super) fn take(&self) -> Option<T> {
        self.published.lock().ok()?.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::channel;

    #[test]
    fn a_reader_that_never_answers_does_not_hold_the_thread_that_asks() {
        // The read stands for a disk that does not answer.
        let (open, gate) = channel::<()>();
        let reader = HomePeerReader::spawn(move || {
            let _ = gate.recv();
            7_u32
        });
        for _ in 0..1000 {
            reader.request();
            assert!(reader.take().is_none(), "nothing was published");
        }
        open.send(()).expect("worker alive");
        let mut got = None;
        tilo_test_env::wait_until("the read to be published", || {
            got = reader.take();
            got.is_some()
        });
        assert_eq!(got, Some(7));
    }

    #[test]
    fn requests_made_while_a_read_runs_are_served_by_one_more_read() {
        let (open, gate) = channel::<()>();
        let reads = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&reads);
        let (began, running) = channel::<()>();
        let reader = HomePeerReader::spawn(move || {
            let _ = began.send(());
            let _ = gate.recv();
            counted.fetch_add(1, Ordering::SeqCst)
        });
        // The first read is under way before the rest are asked for.
        reader.request();
        running.recv().expect("the first read began");
        for _ in 0..50 {
            reader.request();
        }
        for _ in 0..4 {
            let _ = open.send(());
        }
        tilo_test_env::wait_until("both reads to finish", || reads.load(Ordering::SeqCst) >= 2);
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(
            reads.load(Ordering::SeqCst),
            2,
            "fifty requests while one read ran cost one more read, not fifty"
        );
        assert_eq!(reader.take(), Some(1), "the newest result wins");
        assert!(reader.take().is_none(), "a result is taken once");
    }
}
