//! One two-way call channel over a byte stream: either side calls the other
//! and is answered by id, and either side sends events that need no answer.
//!
//! The app socket and the session door both run on it. Frames are a
//! little-endian length, then JSON. A call whose link closes is answered by
//! nothing: its receiver sees the sender dropped.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use wire_frame::Framing;

/// A frame carries at most this much; a history page or a contacts snapshot
/// stays well below it.
const FRAMING: Framing = Framing::little_endian(16 * 1024 * 1024).reject_empty();

#[derive(Serialize, Deserialize)]
enum Frame<C, R, E> {
    Call { id: u64, call: C },
    Reply { id: u64, reply: R },
    Event(E),
}

/// What a link does with what the other side sends.
pub trait Handler<C, R, E>: Send + Sync {
    /// Answer `call` through `reply`, now or later, from any thread.
    fn call(&self, call: C, reply: Responder<C, R, E>);
    fn event(&self, event: E);
    /// The other side is gone.
    fn closed(&self) {}
}

enum Waiter<R> {
    Async(tokio::sync::oneshot::Sender<R>),
    Blocking(std::sync::mpsc::SyncSender<R>),
}

struct Shared<C, R, E> {
    writer: Mutex<Box<dyn Write + Send>>,
    pending: Mutex<HashMap<u64, Waiter<R>>>,
    next: AtomicU64,
    closed: AtomicBool,
    _frames: std::marker::PhantomData<fn(C, E)>,
}

/// One end of a link. Clones share it.
pub struct Link<C, R, E> {
    shared: Arc<Shared<C, R, E>>,
}

impl<C, R, E> Clone for Link<C, R, E> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

/// The answer to one call, sent once.
pub struct Responder<C, R, E> {
    id: u64,
    link: Link<C, R, E>,
}

impl<C, R, E> Responder<C, R, E>
where
    C: Serialize + DeserializeOwned + Send + 'static,
    R: Serialize + DeserializeOwned + Send + 'static,
    E: Serialize + DeserializeOwned + Send + 'static,
{
    pub fn send(self, reply: R) {
        self.link
            .write(&Frame::<C, R, E>::Reply { id: self.id, reply });
    }
}

impl<C, R, E> Link<C, R, E>
where
    C: Serialize + DeserializeOwned + Send + 'static,
    R: Serialize + DeserializeOwned + Send + 'static,
    E: Serialize + DeserializeOwned + Send + 'static,
{
    /// Start reading `reader` on a thread of its own, handing what arrives
    /// to `handler`, and write to `writer`.
    ///
    /// # Errors
    /// The reading thread could not be started.
    pub fn start(
        name: &str,
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        handler: Arc<dyn Handler<C, R, E>>,
    ) -> std::io::Result<Self> {
        let link = Self {
            shared: Arc::new(Shared {
                writer: Mutex::new(Box::new(writer)),
                pending: Mutex::new(HashMap::new()),
                next: AtomicU64::new(1),
                closed: AtomicBool::new(false),
                _frames: std::marker::PhantomData,
            }),
        };
        let reading = link.clone();
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || reading.read(reader, &*handler))?;
        Ok(link)
    }

    fn read(&self, mut reader: impl Read, handler: &dyn Handler<C, R, E>) {
        while let Ok(Some(body)) = FRAMING.read(&mut reader) {
            let Ok(frame) = serde_json::from_slice::<Frame<C, R, E>>(&body) else {
                tracing::warn!(target: "tilo::messaging", "unreadable frame; closing the link");
                break;
            };
            match frame {
                Frame::Call { id, call } => handler.call(
                    call,
                    Responder {
                        id,
                        link: self.clone(),
                    },
                ),
                Frame::Reply { id, reply } => {
                    let waiter = self
                        .shared
                        .pending
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .remove(&id);
                    match waiter {
                        Some(Waiter::Async(sender)) => drop(sender.send(reply)),
                        Some(Waiter::Blocking(sender)) => drop(sender.try_send(reply)),
                        None => {}
                    }
                }
                Frame::Event(event) => handler.event(event),
            }
        }
        self.shared.closed.store(true, Ordering::Release);
        // Every waiting call learns the link is gone.
        self.shared
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        handler.closed();
    }

    fn write(&self, frame: &Frame<C, R, E>) -> bool {
        if self.shared.closed.load(Ordering::Acquire) {
            return false;
        }
        let Ok(body) = serde_json::to_vec(frame) else {
            return false;
        };
        let mut writer = self
            .shared
            .writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        FRAMING.write(&mut *writer, &body).is_ok() && writer.flush().is_ok()
    }

    fn register(&self, waiter: Waiter<R>) -> u64 {
        let id = self.shared.next.fetch_add(1, Ordering::Relaxed);
        self.shared
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, waiter);
        id
    }

    fn forget(&self, id: u64) {
        self.shared
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
    }

    /// Call the other side; the receiver resolves with its answer, or with
    /// an error once the link closed.
    pub fn call(&self, call: C) -> tokio::sync::oneshot::Receiver<R> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let id = self.register(Waiter::Async(sender));
        if !self.write(&Frame::Call { id, call }) {
            self.forget(id);
        }
        receiver
    }

    /// Call the other side and wait up to `timeout` for its answer.
    pub fn call_blocking(&self, call: C, timeout: Duration) -> Option<R> {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let id = self.register(Waiter::Blocking(sender));
        if !self.write(&Frame::Call { id, call }) {
            self.forget(id);
            return None;
        }
        let answer = receiver.recv_timeout(timeout).ok();
        self.forget(id);
        answer
    }

    /// Tell the other side `event`; false once the link is closed.
    pub fn event(&self, event: E) -> bool {
        self.write(&Frame::Event(event))
    }

    pub fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::Acquire)
    }

    /// Close the writing half: the other side reads the end.
    pub fn close(&self) {
        self.shared.closed.store(true, Ordering::Release);
        let mut writer = self
            .shared
            .writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *writer = Box::new(std::io::sink());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;
    impl Handler<String, String, String> for Echo {
        fn call(&self, call: String, reply: Responder<String, String, String>) {
            reply.send(format!("{call}!"));
        }
        fn event(&self, _: String) {}
    }

    #[test]
    fn a_call_is_answered_by_id_and_a_closed_link_answers_nothing() {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let _server = Link::<String, String, String>::start(
            "echo",
            a.try_clone().unwrap(),
            a,
            Arc::new(Echo),
        )
        .unwrap();
        let client = Link::<String, String, String>::start(
            "client",
            b.try_clone().unwrap(),
            b,
            Arc::new(Echo),
        )
        .unwrap();
        assert_eq!(
            client.call_blocking("hi".into(), Duration::from_secs(5)),
            Some("hi!".into())
        );
        client.close();
        assert_eq!(
            client.call_blocking("again".into(), Duration::from_secs(1)),
            None
        );
    }
}
