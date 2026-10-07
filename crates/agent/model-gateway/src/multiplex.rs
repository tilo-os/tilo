//! One reader for each inherited socket, with bounded per-request mailboxes.
//! Duplicating an fd duplicates access, not the stream: independent readers
//! would steal each other's replies. No request is replayed after uncertainty.

use std::collections::HashMap;
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak, mpsc};

use zeroize::Zeroizing;

use super::{ClientError, FRAMING, MAX_CONCURRENT_REQUESTS, MAX_QUEUED_RESPONSE_BYTES};
use super::{RPC_VERSION, Request, RequestFrame, Response, ResponseFrame, write_frame};

const MAX_CONNECTIONS: usize = 8;
const MAX_QUEUED_FRAMES: usize = 32;
type SocketIdentity = (libc::dev_t, libc::ino_t);
type Registry = Mutex<HashMap<SocketIdentity, Weak<Connection>>>;
static CONNECTIONS: OnceLock<Registry> = OnceLock::new();

struct Writer {
    stream: UnixStream,
    next_id: u64,
}

struct Connection {
    writer: Mutex<Writer>,
    control: UnixStream,
    pending: Mutex<HashMap<u64, mpsc::SyncSender<Queued>>>,
    failed: AtomicBool,
    bytes: Arc<AtomicUsize>,
}

struct Charge {
    bytes: Arc<AtomicUsize>,
    amount: usize,
}

impl Drop for Charge {
    fn drop(&mut self) {
        self.bytes.fetch_sub(self.amount, Ordering::AcqRel);
    }
}

struct Queued {
    response: Response,
    _charge: Charge,
}

pub(super) struct Pending {
    connection: Arc<Connection>,
    id: u64,
    receiver: mpsc::Receiver<Queued>,
    finished: bool,
}

impl Pending {
    pub(super) fn read(&mut self) -> Result<Response, ClientError> {
        let queued = self.receiver.recv().map_err(|_| ClientError::Unavailable)?;
        self.finished = queued.response.event.is_none();
        Ok(queued.response)
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        // A caller abandoning an unfinished stream cannot leave an orphaned
        // mailbox, or silently permit a later request to consume its output.
        let outstanding = self
            .connection
            .pending
            .lock()
            .map_or(true, |pending| pending.contains_key(&self.id));
        if !self.finished && outstanding {
            self.connection.fail();
        }
    }
}

impl Connection {
    fn fail(&self) {
        self.failed.store(true, Ordering::Release);
        let _ = self.control.shutdown(Shutdown::Both);
        if let Ok(mut pending) = self.pending.lock() {
            pending.clear();
        }
    }

    fn start(self: &Arc<Self>, request: Request) -> Result<Pending, ClientError> {
        // IDs are assigned while holding the writer, so their wire order is
        // strictly increasing even when independent agent jobs start at once.
        let mut writer = self.writer.lock().map_err(|_| ClientError::Unavailable)?;
        if self.failed.load(Ordering::Acquire) {
            return Err(ClientError::Unavailable);
        }
        let id = writer.next_id;
        writer.next_id = id.checked_add(1).ok_or(ClientError::Unavailable)?;
        let (sender, receiver) = mpsc::sync_channel(MAX_QUEUED_FRAMES);
        {
            let mut pending = self.pending.lock().map_err(|_| ClientError::Unavailable)?;
            if pending.len() >= MAX_CONCURRENT_REQUESTS {
                return Err(ClientError::Protocol(
                    "too many active gateway requests".into(),
                ));
            }
            pending.insert(id, sender);
        }
        if let Err(error) = write_frame(
            &mut writer.stream,
            &RequestFrame {
                version: RPC_VERSION,
                request_id: Some(id),
                value: request,
            },
        ) {
            if matches!(error, ClientError::Protocol(_)) {
                // Serialization/size refusal precedes any bytes being sent.
                if let Ok(mut pending) = self.pending.lock() {
                    pending.remove(&id);
                }
            } else {
                self.fail();
            }
            return Err(error);
        }
        drop(writer);
        Ok(Pending {
            connection: Arc::clone(self),
            id,
            receiver,
            finished: false,
        })
    }
}

fn identity(stream: &UnixStream) -> Result<SocketIdentity, ClientError> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat writes a complete stat on success, into valid storage.
    if unsafe { libc::fstat(stream.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(ClientError::Unavailable);
    }
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFSOCK {
        return Err(ClientError::Unavailable);
    }
    Ok((stat.st_dev, stat.st_ino))
}

pub(super) fn start(stream: UnixStream, request: Request) -> Result<Pending, ClientError> {
    connection(stream)?.start(request)
}

fn connection(stream: UnixStream) -> Result<Arc<Connection>, ClientError> {
    let key = identity(&stream)?;
    let mut registry = CONNECTIONS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| ClientError::Unavailable)?;
    if let Some(existing) = registry.get(&key).and_then(Weak::upgrade) {
        // A failed capability never turns into an implicit retry connection.
        return if existing.failed.load(Ordering::Acquire) {
            Err(ClientError::Unavailable)
        } else {
            Ok(existing)
        };
    }
    registry.retain(|_, value| value.strong_count() != 0);
    if registry.len() >= MAX_CONNECTIONS {
        return Err(ClientError::Protocol("too many gateway connections".into()));
    }
    let reader = stream.try_clone().map_err(|_| ClientError::Unavailable)?;
    let control = stream.try_clone().map_err(|_| ClientError::Unavailable)?;
    let connection = Arc::new(Connection {
        writer: Mutex::new(Writer { stream, next_id: 1 }),
        control,
        pending: Mutex::new(HashMap::new()),
        failed: AtomicBool::new(false),
        bytes: Arc::new(AtomicUsize::new(0)),
    });
    let owner = Arc::clone(&connection);
    std::thread::Builder::new()
        .name("model-gateway-replies".into())
        .spawn(move || receive(reader, &owner))
        .map_err(|_| ClientError::Unavailable)?;
    registry.insert(key, Arc::downgrade(&connection));
    drop(registry);
    Ok(connection)
}

fn receive(mut stream: UnixStream, connection: &Connection) {
    while receive_one(&mut stream, connection).is_ok() {}
    connection.fail();
}

fn receive_one(stream: &mut UnixStream, connection: &Connection) -> Result<(), ()> {
    let body = Zeroizing::new(FRAMING.read(stream).map_err(|_| ())?.ok_or(())?);
    // Charge both encoded text and owned parsed strings before deserializing.
    let amount = body
        .len()
        .checked_mul(2)
        .and_then(|n| n.checked_add(1024))
        .ok_or(())?;
    connection
        .bytes
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
            used.checked_add(amount)
                .filter(|next| *next <= MAX_QUEUED_RESPONSE_BYTES)
        })
        .map_err(|_| ())?;
    let charge = Charge {
        bytes: Arc::clone(&connection.bytes),
        amount,
    };
    let frame: ResponseFrame = serde_json::from_slice(&body).map_err(|_| ())?;
    let mut pending = connection.pending.lock().map_err(|_| ())?;
    let sender = pending.get(&frame.request_id).ok_or(())?;
    let final_frame = frame.value.event.is_none();
    sender
        .try_send(Queued {
            response: frame.value,
            _charge: charge,
        })
        .map_err(|_| ())?;
    if final_frame {
        pending.remove(&frame.request_id);
    }
    drop(pending);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read_frame;

    fn request() -> Request {
        Request::SecretStatus {
            profile_id: "test".into(),
        }
    }

    fn response(output: &str, event: Option<&str>) -> Response {
        Response {
            ok: true,
            output: Some(output.into()),
            configured: None,
            error: None,
            event: event.map(str::to_owned),
        }
    }

    #[test]
    fn same_socket_routes_interleaved_streams_and_out_of_order_finals() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let mut first = start(client.try_clone().unwrap(), request()).unwrap();
        let mut second = start(client.try_clone().unwrap(), request()).unwrap();
        let a: RequestFrame = read_frame(&mut server).unwrap();
        let b: RequestFrame = read_frame(&mut server).unwrap();
        assert!(a.request_id < b.request_id);
        write_frame(
            &mut server,
            &ResponseFrame {
                request_id: b.request_id.unwrap(),
                value: response("chat", Some("tool_call")),
            },
        )
        .unwrap();
        write_frame(
            &mut server,
            &ResponseFrame {
                request_id: a.request_id.unwrap(),
                value: response("task", Some("tool_call")),
            },
        )
        .unwrap();
        write_frame(
            &mut server,
            &ResponseFrame {
                request_id: b.request_id.unwrap(),
                value: response("chat-final", None),
            },
        )
        .unwrap();
        assert_eq!(second.read().unwrap().output.as_deref(), Some("chat"));
        assert_eq!(second.read().unwrap().output.as_deref(), Some("chat-final"));
        assert_eq!(first.read().unwrap().output.as_deref(), Some("task"));
        write_frame(
            &mut server,
            &ResponseFrame {
                request_id: a.request_id.unwrap(),
                value: response("task-final", None),
            },
        )
        .unwrap();
        assert_eq!(first.read().unwrap().output.as_deref(), Some("task-final"));
    }

    #[test]
    fn untagged_response_fails_closed_without_replaying_request() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let mut pending = start(client.try_clone().unwrap(), request()).unwrap();
        let _: RequestFrame = read_frame(&mut server).unwrap();
        write_frame(&mut server, &response("old-server", None)).unwrap();
        assert!(pending.read().is_err());
        assert!(start(client, request()).is_err());
        assert!(FRAMING.read(&mut server).unwrap().is_none());
    }

    #[test]
    fn active_request_limit_is_per_socket_not_per_duplicate() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let mut active = Vec::new();
        for _ in 0..MAX_CONCURRENT_REQUESTS {
            active.push(start(client.try_clone().unwrap(), request()).unwrap());
            let _: RequestFrame = read_frame(&mut server).unwrap();
        }
        assert!(start(client, request()).is_err());
        for pending in &mut active {
            write_frame(
                &mut server,
                &ResponseFrame {
                    request_id: pending.id,
                    value: response("done", None),
                },
            )
            .unwrap();
            assert!(pending.read().is_ok());
        }
    }

    #[test]
    fn bounded_mailbox_overflow_fences_all_outstanding_requests() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let mut stalled = start(client.try_clone().unwrap(), request()).unwrap();
        let mut other = start(client, request()).unwrap();
        let first: RequestFrame = read_frame(&mut server).unwrap();
        let _: RequestFrame = read_frame(&mut server).unwrap();
        for _ in 0..=MAX_QUEUED_FRAMES {
            write_frame(
                &mut server,
                &ResponseFrame {
                    request_id: first.request_id.unwrap(),
                    value: response("bounded", Some("tool_call")),
                },
            )
            .unwrap();
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !stalled.connection.failed.load(Ordering::Acquire) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(other.read().is_err());
        let mut received = 0;
        while stalled.read().is_ok() {
            received += 1;
        }
        assert_eq!(received, MAX_QUEUED_FRAMES);
        assert_eq!(stalled.connection.bytes.load(Ordering::Acquire), 0);
    }

    #[test]
    fn oversized_local_request_does_not_abort_an_existing_task() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let mut task = start(client.try_clone().unwrap(), request()).unwrap();
        let first: RequestFrame = read_frame(&mut server).unwrap();
        let oversized = Request::SecretStatus {
            profile_id: "x".repeat(2 * 1024 * 1024),
        };
        assert!(matches!(
            start(client, oversized),
            Err(ClientError::Protocol(_))
        ));
        write_frame(
            &mut server,
            &ResponseFrame {
                request_id: first.request_id.unwrap(),
                value: response("task-preserved", None),
            },
        )
        .unwrap();
        assert_eq!(
            task.read().unwrap().output.as_deref(),
            Some("task-preserved")
        );
    }
}
