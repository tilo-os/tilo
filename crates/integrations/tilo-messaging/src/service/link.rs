//! One two-way call channel over a byte stream: either side calls the other and is
//! answered by id, and either side sends events that need no answer. Frames are a
//! little-endian length, then JSON: `{"Call":{"id":1,"call":…}}`, `{"Reply":{"id":1,"reply":…}}`
//! or `{"Event":…}`.
//!
//! The app socket and the session's action API both run on it: a call whose link closes is
//! answered by nothing, and its receiver sees the sender dropped.
//!
//! A frame is read and written as a [`Value`] by code that is the same for every link, and each
//! side turns into its own types only what it takes and sends: an app that only calls
//! ([`Link::start_calling`]) never compiles the service's half of the protocol.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use wire_frame::Framing;

const FRAMING: Framing = Framing::little_endian(16 * 1024 * 1024).reject_empty();

/// A frame as it is on the wire. A link reads and writes it through [`Value`]; this is its
/// shape, as the protocol's description names it.
#[derive(Serialize, Deserialize)]
pub enum Frame<C, R, E> {
    Call { id: u64, call: C },
    Reply { id: u64, reply: R },
    Event(E),
}

pub trait Handler<C, R, E>: Send + Sync {
    fn call(&self, call: C, reply: Responder<C, R, E>);
    fn event(&self, event: E);
    fn closed(&self) {}
}

/// What a side that only calls takes from the other: its events, and that the link closed.
pub trait Events<E>: Send + Sync {
    fn event(&self, event: E);
    fn closed(&self) {}
}

enum Waiter<R> {
    Async(tokio::sync::oneshot::Sender<R>),
    Blocking(std::sync::mpsc::SyncSender<R>),
}

/// The writing half of a link, and whether it closed.
struct Wire {
    writer: Mutex<Box<dyn Write + Send>>,
    closed: AtomicBool,
}

impl Wire {
    fn write(&self, frame: &Value) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        let Ok(body) = serde_json::to_vec(frame) else {
            return false;
        };
        let mut writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        FRAMING.write(&mut *writer, &body).is_ok() && writer.flush().is_ok()
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        let mut writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        *writer = Box::new(std::io::sink());
    }
}

/// `{"<kind>": inner}`.
fn tagged(kind: &str, inner: Value) -> Value {
    let mut frame = Map::new();
    frame.insert(kind.to_owned(), inner);
    Value::Object(frame)
}

/// `{"<kind>": {"id": id, "<field>": payload}}`.
fn numbered(kind: &str, id: u64, field: &str, payload: Value) -> Value {
    let mut inner = Map::new();
    inner.insert("id".to_owned(), Value::from(id));
    inner.insert(field.to_owned(), payload);
    tagged(kind, Value::Object(inner))
}

/// A frame as it was read, its payload not yet any side's type.
enum Incoming {
    Call(u64, Value),
    Reply(u64, Value),
    Event(Value),
}

impl Incoming {
    fn parse(body: &[u8]) -> Option<Self> {
        let Value::Object(frame) = serde_json::from_slice::<Value>(body).ok()? else {
            return None;
        };
        let mut entries = frame.into_iter();
        let (kind, inner) = entries.next()?;
        if entries.next().is_some() {
            return None;
        }
        let numbered = |inner: Value, field: &str| -> Option<(u64, Value)> {
            let Value::Object(mut inner) = inner else {
                return None;
            };
            let id = inner.get("id")?.as_u64()?;
            Some((id, inner.remove(field)?))
        };
        match kind.as_str() {
            "Call" => numbered(inner, "call").map(|(id, call)| Self::Call(id, call)),
            "Reply" => numbered(inner, "reply").map(|(id, reply)| Self::Reply(id, reply)),
            "Event" => Some(Self::Event(inner)),
            _ => None,
        }
    }
}

/// What a link does with what it reads. Whether to go on reading: a payload that does not
/// read as its type closes the link.
trait Inbound: Send + Sync {
    fn call(&self, id: u64, call: Value) -> bool;
    fn reply(&self, id: u64, reply: Value) -> bool;
    fn event(&self, event: Value) -> bool;
    fn closed(&self);
}

/// Read frames until the stream ends or one does not read, then close the link.
fn read_frames(mut reader: Box<dyn Read + Send>, wire: &Wire, inbound: &dyn Inbound) {
    while let Ok(Some(body)) = FRAMING.read(&mut reader) {
        let read = match Incoming::parse(&body) {
            Some(Incoming::Call(id, call)) => inbound.call(id, call),
            Some(Incoming::Reply(id, reply)) => inbound.reply(id, reply),
            Some(Incoming::Event(event)) => inbound.event(event),
            None => false,
        };
        if !read {
            tracing::warn!(target: "tilo::messaging", "unreadable frame; closing the link");
            break;
        }
    }
    wire.closed.store(true, Ordering::Release);
    inbound.closed();
}

fn spawn_reading(
    name: &str,
    reader: Box<dyn Read + Send>,
    wire: Arc<Shared>,
    inbound: Box<dyn Inbound>,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || read_frames(reader, &wire.wire, &*inbound))?;
    Ok(())
}

/// What every link holds whatever its types: its writing half and the next call's id.
struct Shared {
    wire: Wire,
    next: AtomicU64,
}

pub struct Link<C, R, E> {
    shared: Arc<Shared>,
    pending: Arc<Mutex<HashMap<u64, Waiter<R>>>>,
    _frames: std::marker::PhantomData<fn(C, E)>,
}

impl<C, R, E> Clone for Link<C, R, E> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
            pending: Arc::clone(&self.pending),
            _frames: std::marker::PhantomData,
        }
    }
}

pub struct Responder<C, R, E> {
    id: u64,
    link: Link<C, R, E>,
}

impl<C, R: Serialize, E> Responder<C, R, E> {
    pub fn send(self, reply: R) {
        let Ok(reply) = serde_json::to_value(reply) else {
            return;
        };
        self.link
            .shared
            .wire
            .write(&numbered("Reply", self.id, "reply", reply));
    }
}

/// A side that answers calls: the service, the session.
struct Serving<C, R, E> {
    link: Link<C, R, E>,
    handler: Arc<dyn Handler<C, R, E>>,
}

impl<C, R, E> Inbound for Serving<C, R, E>
where
    C: DeserializeOwned + Send + 'static,
    R: DeserializeOwned + Send + 'static,
    E: DeserializeOwned + Send + 'static,
{
    fn call(&self, id: u64, call: Value) -> bool {
        let Ok(call) = serde_json::from_value(call) else {
            return false;
        };
        self.handler.call(
            call,
            Responder {
                id,
                link: self.link.clone(),
            },
        );
        true
    }

    fn reply(&self, id: u64, reply: Value) -> bool {
        self.link.answer(id, reply)
    }

    fn event(&self, event: Value) -> bool {
        let Ok(event) = serde_json::from_value(event) else {
            return false;
        };
        self.handler.event(event);
        true
    }

    fn closed(&self) {
        self.link.forget_all();
        self.handler.closed();
    }
}

/// A side that only calls: an app. A call from the other side is never answered.
struct Calling<C, R, E> {
    link: Link<C, R, E>,
    events: Arc<dyn Events<E>>,
}

impl<C, R, E> Inbound for Calling<C, R, E>
where
    C: Send + 'static,
    R: DeserializeOwned + Send + 'static,
    E: DeserializeOwned + Send + 'static,
{
    fn call(&self, _: u64, _: Value) -> bool {
        tracing::warn!(target: "tilo::messaging", "a call on a link that takes none; not answered");
        true
    }

    fn reply(&self, id: u64, reply: Value) -> bool {
        self.link.answer(id, reply)
    }

    fn event(&self, event: Value) -> bool {
        let Ok(event) = serde_json::from_value(event) else {
            return false;
        };
        self.events.event(event);
        true
    }

    fn closed(&self) {
        self.link.forget_all();
        self.events.closed();
    }
}

impl<C, R, E> Link<C, R, E> {
    fn new(writer: impl Write + Send + 'static) -> Self {
        Self {
            shared: Arc::new(Shared {
                wire: Wire {
                    writer: Mutex::new(Box::new(writer)),
                    closed: AtomicBool::new(false),
                },
                next: AtomicU64::new(1),
            }),
            pending: Arc::default(),
            _frames: std::marker::PhantomData,
        }
    }

    fn register(&self, waiter: Waiter<R>) -> u64 {
        let id = self.shared.next.fetch_add(1, Ordering::Relaxed);
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, waiter);
        id
    }

    fn forget(&self, id: u64) {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
    }

    fn forget_all(&self) {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    pub fn is_closed(&self) -> bool {
        self.shared.wire.closed.load(Ordering::Acquire)
    }

    pub fn close(&self) {
        self.shared.wire.close();
    }
}

impl<C, R: DeserializeOwned, E> Link<C, R, E> {
    /// Hand the reply `id` waits for to its caller. Whether it read.
    fn answer(&self, id: u64, reply: Value) -> bool {
        let Ok(reply) = serde_json::from_value::<R>(reply) else {
            return false;
        };
        let waiter = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
        match waiter {
            Some(Waiter::Async(sender)) => drop(sender.send(reply)),
            Some(Waiter::Blocking(sender)) => drop(sender.try_send(reply)),
            None => {}
        }
        true
    }
}

impl<C, R, E> Link<C, R, E>
where
    C: DeserializeOwned + Send + 'static,
    R: DeserializeOwned + Send + 'static,
    E: DeserializeOwned + Send + 'static,
{
    /// A link that answers the other side's calls with `handler`, and calls it.
    pub fn start(
        name: &str,
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        handler: Arc<dyn Handler<C, R, E>>,
    ) -> std::io::Result<Self> {
        let link = Self::new(writer);
        let serving = Serving {
            link: link.clone(),
            handler,
        };
        spawn_reading(
            name,
            Box::new(reader),
            Arc::clone(&link.shared),
            Box::new(serving),
        )?;
        Ok(link)
    }
}

impl<C, R, E> Link<C, R, E>
where
    C: Send + 'static,
    R: DeserializeOwned + Send + 'static,
    E: DeserializeOwned + Send + 'static,
{
    /// A link that only calls the other side, and takes its events with `events`.
    pub fn start_calling(
        name: &str,
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        events: Arc<dyn Events<E>>,
    ) -> std::io::Result<Self> {
        let link = Self::new(writer);
        let calling = Calling {
            link: link.clone(),
            events,
        };
        spawn_reading(
            name,
            Box::new(reader),
            Arc::clone(&link.shared),
            Box::new(calling),
        )?;
        Ok(link)
    }
}

impl<C: Serialize, R, E> Link<C, R, E> {
    fn write_call(&self, id: u64, call: C) -> bool {
        let Ok(call) = serde_json::to_value(call) else {
            return false;
        };
        self.shared.wire.write(&numbered("Call", id, "call", call))
    }

    pub fn call(&self, call: C) -> tokio::sync::oneshot::Receiver<R> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let id = self.register(Waiter::Async(sender));
        if !self.write_call(id, call) {
            self.forget(id);
        }
        receiver
    }

    pub fn call_blocking(&self, call: C, timeout: Duration) -> Option<R> {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let id = self.register(Waiter::Blocking(sender));
        if !self.write_call(id, call) {
            self.forget(id);
            return None;
        }
        let answer = receiver.recv_timeout(timeout).ok();
        self.forget(id);
        answer
    }
}

impl<C, R, E: Serialize> Link<C, R, E> {
    pub fn event(&self, event: E) -> bool {
        let Ok(event) = serde_json::to_value(event) else {
            return false;
        };
        self.shared.wire.write(&tagged("Event", event))
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

    struct Heard(Mutex<Vec<String>>, std::sync::Condvar);
    impl Events<String> for Heard {
        fn event(&self, event: String) {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(event);
            self.1.notify_all();
        }
    }

    #[test]
    fn frames_are_written_and_read_as_their_shape_says() {
        type Shape = Frame<String, u32, Vec<u8>>;
        let written = [
            numbered("Call", 7, "call", Value::from("hi")),
            numbered("Reply", 7, "reply", Value::from(3)),
            tagged("Event", Value::from(vec![1, 2])),
        ];
        for frame in &written {
            let shape: Shape = serde_json::from_value(frame.clone()).unwrap();
            assert_eq!(&serde_json::to_value(&shape).unwrap(), frame);
        }
        let body = serde_json::to_vec(&Shape::Reply { id: 9, reply: 4 }).unwrap();
        let Some(Incoming::Reply(9, reply)) = Incoming::parse(&body) else {
            panic!("a reply frame reads as a reply");
        };
        assert_eq!(reply, Value::from(4));
        assert!(Incoming::parse(br#"{"Call":{"call":1}}"#).is_none());
        assert!(Incoming::parse(br#"{"Event":1,"Reply":{"id":1,"reply":1}}"#).is_none());
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

    #[test]
    fn a_side_that_only_calls_is_answered_and_hears_the_other_sides_events() {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let server = Link::<String, String, String>::start(
            "echo",
            a.try_clone().unwrap(),
            a,
            Arc::new(Echo),
        )
        .unwrap();
        let heard = Arc::new(Heard(Mutex::default(), std::sync::Condvar::new()));
        let client = Link::<String, String, String>::start_calling(
            "client",
            b.try_clone().unwrap(),
            b,
            heard.clone(),
        )
        .unwrap();
        assert_eq!(
            client.call_blocking("hi".into(), Duration::from_secs(5)),
            Some("hi!".into())
        );
        assert!(server.event("changed".into()));
        let events = heard
            .1
            .wait_timeout_while(
                heard.0.lock().unwrap_or_else(PoisonError::into_inner),
                Duration::from_secs(5),
                |events| events.is_empty(),
            )
            .unwrap_or_else(PoisonError::into_inner)
            .0;
        assert_eq!(*events, ["changed"]);
        drop(events);
        // A call from the side that only calls' peer is not answered, and the link goes on.
        assert_eq!(
            server.call_blocking("unasked".into(), Duration::from_millis(200)),
            None
        );
        assert_eq!(
            client.call_blocking("still".into(), Duration::from_secs(5)),
            Some("still!".into())
        );
    }
}
