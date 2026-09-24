//! The compositor's async runtime for session services.
//!
//! Portals, the notification server and the power service are D-Bus servers
//! that live as long as the session. They share one bounded runtime rather
//! than each building a pool of their own; its worker threads are named
//! `tilo-session-rt-N`. The compositor loop never runs on it and never waits
//! on it: work is spawned, and results come back through a [`Mailbox`] whose
//! [`Delivery`] wakes the loop.
//!
//! Peer sync keeps a separate runtime (see `ShellNetwork::spawn`), because it
//! is torn down and rebuilt with the network and its tasks must end with it.

use std::sync::{Arc, OnceLock, mpsc};

use tokio::runtime::Runtime;

/// Two workers: the services here wait on the bus, they do not compute.
const WORKERS: usize = 2;

/// The session runtime, started on first use.
///
/// # Errors
///
/// A description of why the runtime could not be started. The failure is
/// remembered, so every caller degrades the same way.
pub fn runtime() -> Result<&'static Runtime, String> {
    static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tilo_rt::daemon("tilo-session")
                .workers(WORKERS)
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Wakes the compositor loop; on the native backend, a calloop ping.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// The sending half of a [`Mailbox`]: publishes a value and wakes the loop.
///
/// Dropping a `Delivery` also wakes the loop, so a task that ends without
/// delivering (cancelled with its runtime, or panicked) is observed as
/// [`Taken::Closed`] rather than waited on forever.
pub struct Delivery<T> {
    sender: mpsc::Sender<T>,
    wake: Wake,
}

impl<T> Clone for Delivery<T> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            wake: Arc::clone(&self.wake),
        }
    }
}

impl<T> Delivery<T> {
    /// Publish `value` to the loop and wake it. A mailbox the loop has
    /// already dropped discards the value.
    pub fn deliver(&self, value: T) {
        let _ = self.sender.send(value);
        (self.wake)();
    }
}

impl<T> Drop for Delivery<T> {
    fn drop(&mut self) {
        (self.wake)();
    }
}

/// What [`Mailbox::take`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum Taken<T> {
    /// A delivered value, in delivery order.
    Value(T),
    /// Nothing yet; a sender still exists.
    Empty,
    /// Nothing, and every sender is gone: nothing more will arrive.
    Closed,
}

/// The loop's receiving half of a [`Delivery`]. Never blocks.
pub struct Mailbox<T> {
    receiver: mpsc::Receiver<T>,
}

impl<T> Mailbox<T> {
    /// The next delivered value, without waiting.
    pub fn take(&self) -> Taken<T> {
        match self.receiver.try_recv() {
            Ok(value) => Taken::Value(value),
            Err(mpsc::TryRecvError::Empty) => Taken::Empty,
            Err(mpsc::TryRecvError::Disconnected) => Taken::Closed,
        }
    }

    /// Every value delivered so far, in order, without waiting.
    pub fn drain(&self) -> impl Iterator<Item = T> + '_ {
        self.receiver.try_iter()
    }
}

/// A mailbox whose deliveries call `wake`.
pub fn mailbox<T>(wake: Wake) -> (Delivery<T>, Mailbox<T>) {
    let (sender, receiver) = mpsc::channel();
    (Delivery { sender, wake }, Mailbox { receiver })
}

/// A one-shot job started on a runtime's blocking pool, whose result the loop
/// collects with [`PendingStart::poll`].
pub struct PendingStart<T> {
    mailbox: Mailbox<Result<T, String>>,
}

impl<T> PendingStart<T> {
    /// A start that already has its outcome, e.g. one that could not be
    /// scheduled at all. The next [`PendingStart::poll`] returns it.
    pub fn finished(outcome: Result<T, String>) -> Self {
        let (delivery, mailbox) = mailbox(Arc::new(|| {}));
        delivery.deliver(outcome);
        Self { mailbox }
    }

    /// The job's outcome once it has finished, `None` while it runs. A job
    /// that ended without an outcome — its runtime shut down, or it panicked
    /// — is reported as an error, so nothing waits on it forever.
    pub fn poll(&self) -> Option<Result<T, String>> {
        match self.mailbox.take() {
            Taken::Value(outcome) => Some(outcome),
            Taken::Empty => None,
            Taken::Closed => Some(Err("the start ended before reporting an outcome".into())),
        }
    }
}

/// Run `job` on `runtime`'s blocking pool and deliver its outcome to the
/// loop through `wake`. For startup work that blocks by nature (building a
/// runtime, binding sockets, reading keys) and must not run on the loop.
pub fn start_blocking<T, F>(runtime: &tokio::runtime::Handle, wake: Wake, job: F) -> PendingStart<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    let (delivery, mailbox) = mailbox(wake);
    drop(runtime.spawn_blocking(move || delivery.deliver(job())));
    PendingStart { mailbox }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, PoisonError};

    fn channel_wake() -> (Wake, mpsc::Receiver<()>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let wake: Wake = Arc::new(move || {
            let _ = tx.lock().unwrap_or_else(PoisonError::into_inner).send(());
        });
        (wake, rx)
    }

    fn test_runtime() -> Runtime {
        tilo_rt::daemon("tilo-session-test")
            .workers(1)
            .build()
            .unwrap()
    }

    #[test]
    fn deliveries_arrive_in_order_and_wake_the_loop() {
        let (wake, woken) = channel_wake();
        let (delivery, mailbox) = mailbox::<u32>(wake);
        assert_eq!(mailbox.take(), Taken::Empty);
        delivery.deliver(1);
        delivery.deliver(2);
        woken.recv().unwrap();
        assert_eq!(mailbox.drain().collect::<Vec<_>>(), vec![1, 2]);
        drop(delivery);
        assert_eq!(mailbox.take(), Taken::Closed);
    }

    #[test]
    fn a_blocking_start_reports_once_it_finishes() {
        let runtime = test_runtime();
        let (wake, woken) = channel_wake();
        let (release, released) = mpsc::channel::<()>();
        let pending = start_blocking(runtime.handle(), wake, move || {
            released.recv().map_err(|error| error.to_string())?;
            Ok(7_u32)
        });
        assert!(pending.poll().is_none(), "still running");
        release.send(()).unwrap();
        let outcome = loop {
            woken.recv().unwrap();
            if let Some(outcome) = pending.poll() {
                break outcome;
            }
        };
        assert_eq!(outcome, Ok(7));
    }

    #[test]
    fn a_start_that_never_reports_is_a_failure_not_a_hang() {
        let (wake, _woken) = channel_wake();
        let (delivery, mailbox) = mailbox::<Result<u32, String>>(wake);
        let pending = PendingStart { mailbox };
        assert!(pending.poll().is_none());
        drop(delivery);
        assert!(matches!(pending.poll(), Some(Err(_))));
    }
}
