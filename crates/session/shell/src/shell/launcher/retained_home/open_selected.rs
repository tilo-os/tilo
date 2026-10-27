//! Private producer on the existing certified Shell/root bootstrap channel.
use super::{
    PreparedRetainedHome,
    bootstrap::{AdmittedRootHelper, IoFence},
    refused,
};
use runtime_collab::open_selected::{MAX_WIRE_BYTES, RootReceipt, RootReply, RootRequest};
use runtime_portal::{fd_transport, process_binding::ExecutingPeer};
use std::os::fd::AsFd;
use std::{
    fs::File,
    io::{self, Read},
    os::unix::net::UnixStream,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
static DISPATCHERS: OnceLock<Mutex<Vec<Weak<Dispatcher>>>> = OnceLock::new();
const BUDGET: Duration = Duration::from_secs(10);
/// Constructed only after both genuine bootstrap QPE handshakes.
pub(crate) struct Dispatcher {
    stream: Mutex<UnixStream>,
    root: Arc<AdmittedRootHelper>,
    prepared: Arc<PreparedRetainedHome>,
    operation: String,
    launcher: Mutex<Option<Arc<ExecutingPeer>>>,
    retired: AtomicBool,
    queued: AtomicUsize,
    retirement: Mutex<Option<Arc<super::retirement::Observation>>>,
}
impl Dispatcher {
    pub(super) fn new(
        stream: UnixStream,
        root: Arc<AdmittedRootHelper>,
        prepared: Arc<PreparedRetainedHome>,
        operation: String,
    ) -> Arc<Self> {
        Arc::new(Self {
            stream: Mutex::new(stream),
            root,
            prepared,
            operation,
            launcher: Mutex::new(None),
            retired: AtomicBool::new(false),
            queued: AtomicUsize::new(0),
            retirement: Mutex::new(None),
        })
    }
    pub(super) fn attach_retirement(
        &self,
        observation: Arc<super::retirement::Observation>,
    ) -> io::Result<()> {
        let mut held = self.retirement.lock().map_err(|_| refused())?;
        if held.is_some() {
            return Err(refused());
        }
        *held = Some(observation);
        drop(held);
        Ok(())
    }
    pub(super) fn track_proof(
        &self,
        proof: super::bootstrap::ProofCustody,
    ) -> io::Result<super::bootstrap::ProofCustody> {
        // Attached once and never replaced: the observation outlives the lock.
        let observation = self
            .retirement
            .lock()
            .map_err(|_| refused())?
            .clone()
            .ok_or_else(refused)?;
        Ok(proof.tracked(observation.track()?))
    }
    pub(super) fn bind(self: &Arc<Self>, launcher: Arc<ExecutingPeer>) -> io::Result<()> {
        self.root.check()?;
        self.prepared.check(&self.prepared.command)?;
        let mut held = self.launcher.lock().map_err(|_| refused())?;
        if held.is_some() || !launcher.current() {
            return Err(refused());
        }
        *held = Some(launcher);
        drop(held);
        let mut registry = DISPATCHERS
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .map_err(|_| refused())?;
        registry.retain(|entry| entry.strong_count() > 0);
        if registry.len() >= 64 {
            return Err(io::Error::other("retained consumer registry at capacity"));
        }
        registry.push(Arc::downgrade(self));
        drop(registry);
        Ok(())
    }
    fn matches(&self, room: &str, peer: &ExecutingPeer) -> io::Result<bool> {
        if self.retired.load(Ordering::Acquire) || self.prepared.room.to_string() != room {
            return Ok(false);
        }
        let held_launcher = self.launcher.lock().map_err(|_| refused())?;
        let Some(launcher) = held_launcher.as_ref() else {
            return Ok(false);
        };
        let matched = launcher.current()
            && peer.current()
            && peer.belongs_to(launcher.pid(), launcher.start_time()?);
        drop(held_launcher);
        if !matched {
            return Ok(false);
        }
        self.root.check()?;
        Ok(true)
    }
    fn check(&self, authorize: &dyn Fn() -> Result<(), String>) -> io::Result<()> {
        if self.retired.load(Ordering::Acquire) {
            return Err(refused());
        }
        authorize().map_err(io::Error::other)?;
        self.root.check()?;
        self.prepared.check(&self.prepared.command)
    }
    /// Query the actual installed root handler on this very bootstrap. A
    /// successful bootstrap alone does not mean either selected backend exists.
    pub(super) fn query_selected_support(
        &self,
        request: &runtime_collab::open_selected::discovery::QuerySelectedSupport,
        authorize: &dyn Fn() -> Result<(), String>,
        deadline: Instant,
    ) -> io::Result<bool> {
        use runtime_collab::open_selected::discovery::{RootCommand, RootReply};
        if !request.valid() {
            return Err(refused());
        }
        self.queued
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1).filter(|next| *next <= 16)
            })
            .map_err(|_| io::Error::other("selected discovery queue at capacity"))?;
        let _slot = QueuedRequest(&self.queued);
        let mut stream = loop {
            self.check(authorize)?;
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            match self.stream.try_lock() {
                Ok(value) => break value,
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => return Err(refused()),
            }
        };
        let result = (|| {
            let _fence = IoFence::new(stream.try_clone()?, self.root.cancel.clone(), deadline)?;
            let bytes = runtime_collab::open_selected::encode(&RootCommand::QuerySelectedSupport(
                request.clone(),
            ))?;
            let framing = wire_frame::Framing::little_endian(MAX_WIRE_BYTES).reject_empty();
            framing
                .write(&mut *stream, &bytes)
                .map_err(io::Error::other)?;
            let bytes = framing
                .read(&mut *stream)
                .map_err(io::Error::other)?
                .ok_or_else(refused)?;
            let reply: RootReply = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            self.check(authorize)?;
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            match reply {
                RootReply::Unavailable => Ok(false),
                RootReply::Supported(value) if value.matches(request, &self.operation) => Ok(true),
                RootReply::Supported(_) => Err(refused()),
            }
        })();
        if result.is_err() {
            self.retired.store(true, Ordering::Release);
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        result
    }
    /// Hand root `request` and the selected `objects`; their descriptors
    /// close once root holds its own.
    fn send(
        &self,
        request: &RootRequest,
        objects: Objects,
        executing: &ExecutingPeer,
        authorize: &(dyn Fn() -> Result<(), String> + Send + Sync),
        v2: Option<&runtime_collab::open_selected::v2::RootRequest>,
    ) -> io::Result<RootReceipt> {
        if !request.valid()
            || v2
                .as_ref()
                .is_some_and(|v| !v.valid() || v.navigation() != *request)
        {
            return Err(refused());
        }
        self.queued
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1).filter(|next| *next <= 16)
            })
            .map_err(|_| io::Error::other("selected-open queue at capacity"))?;
        let _slot = QueuedRequest(&self.queued);
        let deadline = Instant::now()
            .checked_add(BUDGET)
            .unwrap_or_else(Instant::now);
        // Waiting for a previous request consumes the same absolute budget.
        let mut stream = loop {
            self.check(authorize)?;
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            match self.stream.try_lock() {
                Ok(stream) => break stream,
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => return Err(refused()),
            }
        };
        let result = (|| {
            stream.set_read_timeout(Some(BUDGET))?;
            stream.set_write_timeout(Some(BUDGET))?;
            let _fence = IoFence::new(stream.try_clone()?, self.root.cancel.clone(), deadline)?;
            let bytes = match v2 {
                Some(request) => runtime_collab::open_selected::encode(
                    &runtime_collab::open_selected::v2::RootCommand::OpenSelected(request.clone()),
                )?,
                None => runtime_collab::open_selected::encode(
                    &runtime_collab::open_selected::RootCommand::OpenSelected(request.clone()),
                )?,
            };
            wire_frame::Framing::little_endian(MAX_WIRE_BYTES)
                .reject_empty()
                .write(&mut *stream, &bytes)
                .map_err(io::Error::other)?;
            for (tag, fd) in [
                (80, executing.pidfd()),
                (81, objects.selected.as_fd()),
                (82, objects.parent.as_fd()),
                (83, objects.proof.as_fd()),
            ] {
                self.check(authorize)?;
                if !executing.current() {
                    return Err(refused());
                }
                fd_transport::send_with_fd(stream.as_fd(), &[tag], fd)?;
                let mut ack = [0];
                stream.read_exact(&mut ack)?;
                if ack != [tag] || Instant::now() >= deadline {
                    return Err(refused());
                }
            }
            let bytes = wire_frame::Framing::little_endian(MAX_WIRE_BYTES)
                .reject_empty()
                .read(&mut *stream)
                .map_err(io::Error::other)?
                .ok_or_else(refused)?;
            let reply = match v2 {
                Some(request) => {
                    runtime_collab::open_selected::v2::decode_admission(&bytes, request.access)?
                }
                None => runtime_collab::open_selected::decode_reply(&bytes)?,
            };
            if reply.request_id() != request.request_id || Instant::now() >= deadline {
                return Err(refused());
            }
            self.check(authorize)?;
            Ok(reply)
        })();
        drop(objects);
        // Anything but an admission for this operation or a plain refusal
        // retires the channel.
        let answered = match &result {
            Ok(RootReply::Admitted { receipt }) => receipt.source_operation == self.operation,
            Ok(RootReply::Refused { .. }) => true,
            _ => false,
        };
        if !answered {
            self.retired.store(true, Ordering::Release);
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        drop(stream);
        match result {
            Ok(RootReply::Admitted { receipt }) if answered => Ok(receipt),
            Ok(RootReply::Refused { reason, .. }) => Err(io::Error::other(format!(
                "selected-open admission refused: {reason:?}"
            ))),
            Ok(_) => Err(refused()),
            Err(error) => Err(error),
        }
    }
}
impl Drop for Dispatcher {
    fn drop(&mut self) {
        if let Ok(stream) = self.stream.get_mut() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
}
pub(crate) struct Objects {
    pub selected: File,
    pub parent: File,
    pub proof: UnixStream,
}
pub(super) fn find(room: &str, peer: &Arc<ExecutingPeer>) -> io::Result<Arc<Dispatcher>> {
    let registry = DISPATCHERS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .map_err(|_| refused())?;
    let candidates = registry
        .iter()
        .filter_map(Weak::upgrade)
        .collect::<Vec<_>>();
    drop(registry);
    let mut matched = None;
    for candidate in candidates {
        if candidate.matches(room, peer)? {
            if matched.is_some() {
                return Err(refused());
            }
            matched = Some(candidate);
        }
    }
    let dispatcher =
        matched.ok_or_else(|| io::Error::other("retained selected-open consumer unavailable"))?;
    Ok(dispatcher)
}
struct QueuedRequest<'a>(&'a AtomicUsize);
impl Drop for QueuedRequest<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Dispatcher {
    pub(super) fn prepared(&self) -> &Arc<PreparedRetainedHome> {
        &self.prepared
    }
    pub(super) fn root(&self) -> &Arc<AdmittedRootHelper> {
        &self.root
    }
    pub(super) fn operation(&self) -> &str {
        &self.operation
    }
    pub(super) fn admit(
        self: &Arc<Self>,
        request: &RootRequest,
        objects: Objects,
        executing: &ExecutingPeer,
        authorize: &(dyn Fn() -> Result<(), String> + Send + Sync),
    ) -> io::Result<RootReceipt> {
        self.send(request, objects, executing, authorize, None)
    }
    pub(super) fn admit_v2(
        &self,
        request: &runtime_collab::open_selected::v2::RootRequest,
        objects: Objects,
        executing: &ExecutingPeer,
        authorize: &(dyn Fn() -> Result<(), String> + Send + Sync),
    ) -> io::Result<RootReceipt> {
        self.send(
            &request.navigation(),
            objects,
            executing,
            authorize,
            Some(request),
        )
    }
    pub(super) fn transact<T>(
        &self,
        authorize: &(dyn Fn() -> Result<(), String> + Send + Sync),
        exchange: impl FnOnce(&mut UnixStream) -> io::Result<T>,
    ) -> io::Result<T> {
        self.transact_classified(authorize, |stream, _| exchange(stream).map(Ok), false)?
    }
    /// Same authenticated queue/socket, with per-operation outcomes only after
    /// shared framing is complete. Root/source integrity failures remain global.
    pub(super) fn transact_operation<T>(
        &self,
        authorize: &(dyn Fn() -> Result<(), String> + Send + Sync),
        exchange: impl FnOnce(&mut UnixStream, Instant) -> io::Result<io::Result<T>>,
    ) -> io::Result<io::Result<T>> {
        self.transact_classified(authorize, exchange, true)
    }
    fn transact_classified<T>(
        &self,
        authorize: &(dyn Fn() -> Result<(), String> + Send + Sync),
        exchange: impl FnOnce(&mut UnixStream, Instant) -> io::Result<io::Result<T>>,
        operation_outcome: bool,
    ) -> io::Result<io::Result<T>> {
        self.check(authorize)?;
        self.queued
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1).filter(|next| *next <= 16)
            })
            .map_err(|_| io::Error::other("selected-open queue at capacity"))?;
        let _slot = QueuedRequest(&self.queued);
        let deadline = Instant::now()
            .checked_add(BUDGET)
            .unwrap_or_else(Instant::now);
        let mut stream = loop {
            self.check(authorize)?;
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            match self.stream.try_lock() {
                Ok(stream) => break stream,
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => return Err(refused()),
            }
        };
        let result = (|| {
            stream.set_read_timeout(Some(BUDGET))?;
            stream.set_write_timeout(Some(BUDGET))?;
            let _fence = IoFence::new(stream.try_clone()?, self.root.cancel.clone(), deadline)?;
            let result = exchange(&mut stream, deadline)?;
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            self.root.check()?;
            self.prepared.check(&self.prepared.command)?;
            if self.retired.load(Ordering::Acquire) {
                return Err(refused());
            }
            if let Err(error) = authorize().map_err(io::Error::other) {
                if operation_outcome {
                    return Ok(Err(error));
                }
                return Err(error);
            }
            Ok(result)
        })();
        if result.is_err() {
            self.retired.store(true, Ordering::Release);
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        result
    }
}
