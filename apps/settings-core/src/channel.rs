//! The channels Settings' background work answers on, compiled once.
//!
//! `std::sync::mpsc` is generic over what it carries: every payload type
//! compiles the channel's send, receive and disconnect code for all three of
//! its flavours again, and Settings' areas each answer with a type of their
//! own. These channels carry every payload as one boxed [`Erased`] value;
//! only their typed edges ([`Sender::send`], [`Receiver::try_recv`]) know
//! `T`. The interface follows `std::sync::mpsc`.

use std::any::Any;
use std::marker::PhantomData;
use std::sync::mpsc;
use std::time::Duration;

pub use std::sync::mpsc::{RecvError, RecvTimeoutError, TryRecvError};

/// A payload on its way, its type known only to the channel's edges.
pub type Erased = Box<dyn Any + Send>;

/// The receiver has hung up; the value was not sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Disconnected;

/// An unbounded channel, as [`std::sync::mpsc::channel`].
pub fn channel<T: Send + 'static>() -> (Sender<T>, Receiver<T>) {
    let (sender, receiver) = mpsc::channel();
    (Sender::new(sender), Receiver::new(receiver))
}

/// A channel holding at most `bound` values, as
/// [`std::sync::mpsc::sync_channel`].
pub fn sync_channel<T: Send + 'static>(bound: usize) -> (SyncSender<T>, Receiver<T>) {
    let (sender, receiver) = mpsc::sync_channel(bound);
    (SyncSender::new(sender), Receiver::new(receiver))
}

/// The sending half of [`channel`].
pub struct Sender<T> {
    inner: mpsc::Sender<Erased>,
    of: PhantomData<fn(T)>,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self::new(self.inner.clone())
    }
}

impl<T> Sender<T> {
    fn new(inner: mpsc::Sender<Erased>) -> Self {
        Self {
            inner,
            of: PhantomData,
        }
    }
}

impl<T: Send + 'static> Sender<T> {
    /// Send `value`; fails only when the receiver is gone.
    pub fn send(&self, value: T) -> Result<(), Disconnected> {
        self.inner.send(Box::new(value)).map_err(|_| Disconnected)
    }
}

/// The sending half of [`sync_channel`].
pub struct SyncSender<T> {
    inner: mpsc::SyncSender<Erased>,
    of: PhantomData<fn(T)>,
}

impl<T> Clone for SyncSender<T> {
    fn clone(&self) -> Self {
        Self::new(self.inner.clone())
    }
}

impl<T> SyncSender<T> {
    fn new(inner: mpsc::SyncSender<Erased>) -> Self {
        Self {
            inner,
            of: PhantomData,
        }
    }
}

impl<T: Send + 'static> SyncSender<T> {
    /// Send `value`, waiting while the channel is full; fails only when the
    /// receiver is gone.
    pub fn send(&self, value: T) -> Result<(), Disconnected> {
        self.inner.send(Box::new(value)).map_err(|_| Disconnected)
    }
}

/// The receiving half of [`channel`] or [`sync_channel`].
pub struct Receiver<T> {
    inner: mpsc::Receiver<Erased>,
    of: PhantomData<fn() -> T>,
}

impl<T> Receiver<T> {
    fn new(inner: mpsc::Receiver<Erased>) -> Self {
        Self {
            inner,
            of: PhantomData,
        }
    }
}

impl<T: 'static> Receiver<T> {
    /// The next value if one is waiting.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        open(self.inner.try_recv()?).ok_or(TryRecvError::Disconnected)
    }

    /// The next value, waiting for it.
    pub fn recv(&self) -> Result<T, RecvError> {
        open(self.inner.recv()?).ok_or(RecvError)
    }

    /// The next value, waiting at most `timeout` for it.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        open(self.inner.recv_timeout(timeout)?).ok_or(RecvTimeoutError::Disconnected)
    }
}

/// `value` as the `T` its channel's sender boxed. Only a [`Sender<T>`] or
/// [`SyncSender<T>`] sends on a [`Receiver<T>`]'s channel, so this is never
/// `None`.
fn open<T: 'static>(value: Erased) -> Option<T> {
    value.downcast::<T>().ok().map(|value| *value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_arrive_typed_and_in_order() {
        let (sender, receiver) = channel::<Result<u32, String>>();
        let other = sender.clone();
        sender.send(Ok(1)).ok();
        other.send(Err("two".into())).ok();
        assert_eq!(receiver.try_recv(), Ok(Ok(1)));
        assert_eq!(receiver.recv(), Ok(Err("two".into())));
        assert_eq!(receiver.try_recv(), Err(TryRecvError::Empty));
        drop(sender);
        assert_eq!(
            receiver.try_recv(),
            Err(TryRecvError::Empty),
            "a clone still holds the channel open"
        );
        drop(other);
        assert_eq!(receiver.try_recv(), Err(TryRecvError::Disconnected));
    }

    #[test]
    fn a_bounded_channel_reports_its_receiver_gone() {
        let (sender, receiver) = sync_channel::<String>(1);
        assert_eq!(sender.send("one".into()), Ok(()));
        assert_eq!(
            receiver.recv_timeout(Duration::from_millis(1)),
            Ok("one".to_string())
        );
        assert_eq!(
            receiver.recv_timeout(Duration::from_millis(1)),
            Err(RecvTimeoutError::Timeout)
        );
        drop(receiver);
        assert_eq!(sender.send("two".into()), Err(Disconnected));
    }
}
