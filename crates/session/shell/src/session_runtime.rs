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

use std::sync::{Arc, Mutex, OnceLock, PoisonError, mpsc};

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

enum Slot<T> {
    Pending,
    Registered(T),
    Released,
}

/// A session service registering on the bus in the background.
///
/// The handle the registration produces (a bus name, an object server) is
/// kept here once it arrives and released when this is dropped. A
/// registration that finishes after the drop is released at once, so the
/// name is never held past the session.
pub struct Registration<T> {
    slot: Arc<Mutex<Slot<T>>>,
}

impl<T: Send + 'static> Registration<T> {
    /// Start `register` on `runtime` and return at once. `registered` sees
    /// the handle when it arrives, to report on it. An error is logged under
    /// `service`; the compositor carries on without that service.
    pub fn start<E, F, R>(
        runtime: &tokio::runtime::Handle,
        service: &'static str,
        register: F,
        registered: R,
    ) -> Self
    where
        E: std::fmt::Display,
        F: Future<Output = Result<T, E>> + Send + 'static,
        R: FnOnce(&T) + Send + 'static,
    {
        let slot = Arc::new(Mutex::new(Slot::Pending));
        let task_slot = Arc::clone(&slot);
        drop(runtime.spawn(async move {
            match register.await {
                Ok(handle) => {
                    registered(&handle);
                    let mut slot = task_slot.lock().unwrap_or_else(PoisonError::into_inner);
                    if matches!(*slot, Slot::Pending) {
                        *slot = Slot::Registered(handle);
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        "{service}: could not register on the bus ({error}); continuing without it"
                    );
                }
            }
        }));
        Self { slot }
    }

    /// Whether the registration has completed and is being held.
    pub fn is_registered(&self) -> bool {
        matches!(
            *self.slot.lock().unwrap_or_else(PoisonError::into_inner),
            Slot::Registered(_)
        )
    }
}

impl<T> Drop for Registration<T> {
    fn drop(&mut self) {
        let released = std::mem::replace(
            &mut *self.slot.lock().unwrap_or_else(PoisonError::into_inner),
            Slot::Released,
        );
        drop(released);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DropSignal(mpsc::Sender<()>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

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
    #[allow(
        clippy::items_after_statements,
        reason = "a phase with an early return keeps its original body as a nested fn"
    )]
    fn session_starts_are_delivered_in_order_and_never_hang() {
        // Phase `deliveries_arrive_in_order_and_wake_the_loop`.
        {
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
        // Phase `a_blocking_start_reports_once_it_finishes`.
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
        a_blocking_start_reports_once_it_finishes();
        // Phase `a_start_that_never_reports_is_a_failure_not_a_hang`.
        {
            let (wake, _woken) = channel_wake();
            let (delivery, mailbox) = mailbox::<Result<u32, String>>(wake);
            let pending = PendingStart { mailbox };
            assert!(pending.poll().is_none());
            drop(delivery);
            assert!(matches!(pending.poll(), Some(Err(_))));
        }
    }

    #[test]
    fn registrations_are_held_until_released_and_late_ones_dropped() {
        // Phase `a_registration_is_held_until_released`.
        {
            let runtime = test_runtime();
            let (registered_tx, registered_rx) = mpsc::channel::<()>();
            let (dropped_tx, dropped_rx) = mpsc::channel::<()>();
            let registration = Registration::start(
                runtime.handle(),
                "test",
                async move {
                    let handle = DropSignal(dropped_tx);
                    let _ = registered_tx.send(());
                    Ok::<_, String>(handle)
                },
                |_| {},
            );
            registered_rx.recv().unwrap();
            // Registered, or about to be: a held handle is never dropped.
            assert!(dropped_rx.try_recv().is_err());
            drop(registration);
            dropped_rx.recv().unwrap();
        }
        // Phase `a_registration_finishing_after_release_is_not_kept`.
        {
            let runtime = test_runtime();
            let (finish, finished) = tokio::sync::oneshot::channel::<()>();
            let (dropped_tx, dropped_rx) = mpsc::channel::<()>();
            let registration = Registration::start(
                runtime.handle(),
                "test",
                async move {
                    let _ = finished.await;
                    Ok::<_, String>(DropSignal(dropped_tx))
                },
                |_| {},
            );
            assert!(!registration.is_registered());
            drop(registration);
            finish.send(()).unwrap();
            // The late handle is dropped by the task instead of being stored.
            dropped_rx.recv().unwrap();
        }
    }
}
