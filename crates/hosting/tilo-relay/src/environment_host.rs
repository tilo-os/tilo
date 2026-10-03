//! A compute worker's environment sessions: the guests it runs for members.
//!
//! The relay routes a member's environment stream to a worker
//! ([`crate::environment_routing`]); the worker hands it to
//! [`EnvironmentHost::serve`]. A session is independent of the stream that
//! opened it: its guest keeps running, and its output keeps draining into a
//! bounded scrollback, while no terminal is attached. A later stream
//! re-attaches by session id and receives the scrollback first. One terminal
//! is attached at a time; a new attachment supersedes the previous one.
//!
//! # Tenancy
//!
//! A session belongs to the member's *user* root, so any of that person's
//! devices may re-attach to it and nobody else's can. Each member has an
//! area of their own under the host root ([`MemberArea`]): environment homes,
//! the relay-side files that granted directories map onto, and the layer
//! store of the member's own supervisor. Per-member quotas
//! ([`MemberQuota`]) bound concurrent sessions and disk here, and CPU,
//! memory and process count in the launcher ([`GuestLauncher`]); a session
//! that is detached and quiet for [`EnvironmentHostConfig::idle_timeout`]
//! is reaped.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::time::Instant;

use tilo_relay_protocol::environment::{
    ClientFrame, EnvironmentRecordRef, EnvironmentSpec, HomePolicy, LegacyUsage,
    MAX_ENVIRONMENT_FRAME_BYTES, MAX_PTY_CHUNK_BYTES, MAX_SCROLLBACK_BYTES, MemberIdentity,
    PtySize, QuotaKind, Refusal, ServerFrame, SessionId, SessionRequest, ToolImage, bounded_detail,
};

use crate::frame::{read_frame_or_eof, write_frame};

/// A guest's pty output.
type GuestOutput = Pin<Box<dyn AsyncRead + Send>>;

/// Frames queued for one attached terminal before it counts as stalled.
const ATTACHMENT_QUEUE: usize = 64;
/// Tool announcements a detached session keeps for its next terminal.
const MAX_PENDING_TOOLS: usize = 16;

/// Per-member limits a worker enforces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemberQuota {
    /// Concurrent environment sessions.
    pub max_sessions: u32,
    /// Bytes the member's area may hold before new sessions are refused.
    pub disk_bytes: u64,
    /// Memory for all of the member's guests together (cgroup `memory.max`).
    pub memory_bytes: u64,
    /// CPU for all of the member's guests together, in percent of one core
    /// (cgroup `cpu.max`).
    pub cpu_percent: u32,
    /// Processes for all of the member's guests together (cgroup
    /// `pids.max`).
    pub max_processes: u32,
    /// Concurrent legacy VMs.
    pub max_legacy_vms: u32,
}

impl Default for MemberQuota {
    fn default() -> Self {
        Self {
            max_sessions: 4,
            disk_bytes: 10 * 1024 * 1024 * 1024,
            memory_bytes: 2 * 1024 * 1024 * 1024,
            cpu_percent: 200,
            max_processes: 512,
            max_legacy_vms: 1,
        }
    }
}

/// How a worker hosts environments.
#[derive(Clone, Debug)]
pub struct EnvironmentHostConfig {
    /// Root of every member's area (`<root>/members/<user hex>/`).
    pub root: PathBuf,
    /// Limits applied to every member.
    pub quota: MemberQuota,
    /// A detached session with no activity for this long is ended.
    pub idle_timeout: Duration,
    /// Sessions this worker runs at once, for all members together.
    pub capacity: u32,
}

impl EnvironmentHostConfig {
    /// Defaults under `root`.
    #[must_use]
    pub fn at(root: PathBuf) -> Self {
        Self {
            root,
            quota: MemberQuota::default(),
            idle_timeout: Duration::from_secs(30 * 60),
            capacity: 32,
        }
    }
}

impl MemberQuota {
    /// The quota a relay's configuration sets.
    #[must_use]
    pub fn from_config(config: &tilo_relay_protocol::config::EnvironmentsConfig) -> Self {
        Self {
            max_sessions: config.member_sessions,
            disk_bytes: config.member_disk_bytes,
            memory_bytes: config.member_memory_bytes,
            cpu_percent: config.member_cpu_percent,
            max_processes: config.member_processes,
            max_legacy_vms: config.member_legacy_vms,
        }
    }
}

/// The environment hosting a relay's own compute worker runs: `ush`
/// guests under `root`, as `config` describes. `None` config: none.
#[must_use]
pub fn worker_environments(
    config: Option<&tilo_relay_protocol::config::EnvironmentsConfig>,
    root: &Path,
) -> crate::compute_worker::WorkerEnvironments {
    let Some(config) = config else {
        return crate::compute_worker::WorkerEnvironments::default();
    };
    let quota = MemberQuota::from_config(config);
    let mut launcher = crate::environment_guest::UshLauncherConfig::at(root.to_path_buf());
    launcher.ush_binary.clone_from(&config.ush_binary);
    launcher.cgroup_root.clone_from(&config.cgroup_root);
    if let Some(first) = config.member_uid_first {
        launcher.isolation = crate::environment_guest::MemberIsolation::SeparateUids {
            first,
            count: config.member_uid_count,
        };
    }
    let launcher = Arc::new(crate::environment_guest::UshGuestLauncher::new(launcher));
    let host = EnvironmentHost::new(
        EnvironmentHostConfig {
            root: root.to_path_buf(),
            quota,
            idle_timeout: Duration::from_secs(config.idle_timeout_secs),
            capacity: config.capacity,
        },
        Arc::clone(&launcher) as Arc<dyn GuestLauncher>,
        Arc::new(TracingMetering),
    );
    crate::compute_worker::WorkerEnvironments {
        host: Some(host),
        legacy: None,
    }
}

/// One member's private area on the worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberArea {
    root: PathBuf,
}

impl MemberArea {
    /// The area of `user` under `host_root`.
    #[must_use]
    pub fn of(host_root: &Path, user: &[u8; 32]) -> Self {
        Self {
            root: host_root.join("members").join(hex::encode(user)),
        }
    }

    /// The area's root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The member's relay-side files. Granted directories resolve here.
    #[must_use]
    pub fn files(&self) -> PathBuf {
        self.root.join("files")
    }

    /// The environment records the member's supervisor builds guests from.
    #[must_use]
    pub fn records(&self) -> PathBuf {
        self.root.join("records")
    }

    /// The layer store of the member's supervisor.
    #[must_use]
    pub fn store(&self) -> PathBuf {
        self.root.join("ush")
    }

    /// Runtime directory of the member's supervisor (its socket).
    #[must_use]
    pub fn runtime(&self) -> PathBuf {
        self.root.join("run")
    }

    /// Home of one environment, or of one session when the environment's
    /// home is ephemeral.
    #[must_use]
    pub fn home(
        &self,
        record: &EnvironmentRecordRef,
        policy: HomePolicy,
        session: &SessionId,
    ) -> PathBuf {
        let environment =
            blake3::hash(format!("{}:{}", record.workspace_id, record.record_id).as_bytes());
        let environment = hex::encode(environment.as_bytes().get(..16).unwrap_or_default());
        match policy {
            HomePolicy::Persistent => self.root.join("homes").join(environment),
            HomePolicy::Ephemeral => self
                .root
                .join("ephemeral")
                .join(format!("{environment}-{}", hex::encode(session))),
        }
    }

    /// Where a granted directory's contents live on the relay: the same
    /// path below [`Self::files`]. `None` for a path that is not absolute
    /// and normal.
    #[must_use]
    pub fn grant_source(&self, path: &str) -> Option<PathBuf> {
        if !tilo_relay_protocol::environment::normal_absolute_path(path) {
            return None;
        }
        let relative = path.trim_start_matches('/');
        Some(if relative.is_empty() {
            self.files()
        } else {
            self.files().join(relative)
        })
    }

    /// Bytes stored in the area. Symlinks are not followed.
    ///
    /// # Errors
    ///
    /// Returns an error when the area cannot be walked.
    pub fn usage_bytes(&self) -> io::Result<u64> {
        fn walk(path: &Path, total: &mut u64) -> io::Result<()> {
            let metadata = match std::fs::symlink_metadata(path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error),
            };
            if metadata.is_dir() {
                for entry in std::fs::read_dir(path)? {
                    walk(&entry?.path(), total)?;
                }
            } else {
                *total = total.saturating_add(metadata.len());
            }
            Ok(())
        }
        let mut total = 0;
        walk(&self.root, &mut total)?;
        Ok(total)
    }
}

/// Who a guest is launched for, and within which limits.
#[derive(Clone, Debug)]
pub struct MemberContext {
    /// The certified member.
    pub identity: MemberIdentity,
    /// Their area.
    pub area: MemberArea,
    /// Their limits.
    pub quota: MemberQuota,
}

/// What a launcher needs to start one guest.
#[derive(Clone, Debug)]
pub struct GuestRequest {
    /// The session the guest belongs to.
    pub session: SessionId,
    /// The record it was opened from.
    pub record: EnvironmentRecordRef,
    /// The record's resolved contents.
    pub spec: EnvironmentSpec,
    /// Initial size.
    pub size: PtySize,
    /// `TERM`.
    pub term: String,
}

/// A running guest's terminal, as the host drives it.
pub struct Guest {
    /// The pty's output.
    pub output: Pin<Box<dyn AsyncRead + Send>>,
    /// The pty's input.
    pub input: Pin<Box<dyn AsyncWrite + Send>>,
    /// Resize and teardown.
    pub control: Box<dyn GuestControl>,
}

/// Out-of-band control of a running guest.
pub trait GuestControl: Send + Sync {
    /// Resize the guest's terminal.
    ///
    /// # Errors
    ///
    /// Returns an error when the pty refuses the size.
    fn resize(&self, size: PtySize) -> io::Result<()>;
    /// End the guest. Its output then reaches end of file.
    fn terminate(&self);
    /// The guest's exit code, once its output ended, when known.
    fn exit_code(&self) -> Option<i32>;
}

/// Failure to start a guest.
#[derive(Debug, Error)]
pub enum LaunchError {
    /// A quota of the member's would be exceeded.
    #[error("quota exceeded: {0:?}")]
    Quota(QuotaKind),
    /// The guest could not be started.
    #[error("guest launch failed: {0}")]
    Failed(String),
}

/// Starts guests for a worker's [`EnvironmentHost`].
pub trait GuestLauncher: Send + Sync + 'static {
    /// Start one guest for `member`. Called on the blocking pool.
    ///
    /// # Errors
    ///
    /// Returns an error when the guest cannot be started.
    fn launch(&self, member: &MemberContext, request: &GuestRequest) -> Result<Guest, LaunchError>;

    /// The member has no sessions left; idle per-member resources may go.
    fn member_idle(&self, _member: &MemberIdentity) {}

    /// The tool images `record` has now, on this worker: a tool the member
    /// installed in a session was built here and added to it. `None` when
    /// the launcher keeps no record.
    fn current_tools(
        &self,
        _member: &MemberContext,
        _record: &EnvironmentRecordRef,
    ) -> Option<Vec<ToolImage>> {
        None
    }
}

/// A metered event, for per-member accounting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MeterEvent {
    /// A session started.
    SessionOpened {
        /// Whose.
        member: MemberIdentity,
        /// Which.
        session: SessionId,
    },
    /// A session ended after running this long.
    SessionClosed {
        /// Whose.
        member: MemberIdentity,
        /// Which.
        session: SessionId,
        /// Wall time it ran, milliseconds.
        wall_ms: u64,
    },
    /// A request was refused by a quota.
    QuotaRefused {
        /// Whose.
        member: MemberIdentity,
        /// Which quota.
        quota: QuotaKind,
    },
    /// Legacy VM usage reported by a KVM worker.
    LegacyUsage {
        /// Whose.
        member: MemberIdentity,
        /// The usage since the last report.
        usage: LegacyUsage,
    },
}

/// Receives metered events; a deployment bills or limits from them.
pub trait Metering: Send + Sync + 'static {
    /// Record one event.
    fn record(&self, event: MeterEvent);
}

/// Metering that keeps nothing.
#[derive(Debug, Default)]
pub struct NoMetering;

impl Metering for NoMetering {
    fn record(&self, _event: MeterEvent) {}
}

/// Metering that logs each event, for deployments without billing.
#[derive(Debug, Default)]
pub struct TracingMetering;

impl Metering for TracingMetering {
    fn record(&self, event: MeterEvent) {
        tracing::info!(?event, "environment metering");
    }
}

/// The worker's environment sessions.
#[derive(Clone)]
pub struct EnvironmentHost {
    inner: Arc<HostInner>,
}

struct HostInner {
    config: EnvironmentHostConfig,
    launcher: Arc<dyn GuestLauncher>,
    metering: Arc<dyn Metering>,
    sessions: Mutex<HashMap<SessionId, Arc<Slot>>>,
}

struct Slot {
    id: SessionId,
    owner: MemberIdentity,
    record: EnvironmentRecordRef,
    /// The tool images the session's environment had when last looked at.
    known_tools: Mutex<Vec<String>>,
    opened_at: Instant,
    input: tokio::sync::Mutex<Pin<Box<dyn AsyncWrite + Send>>>,
    control: Box<dyn GuestControl>,
    shared: Mutex<SlotShared>,
}

struct SlotShared {
    scrollback: VecDeque<u8>,
    attachment: Option<Attachment>,
    next_generation: u64,
    last_activity: Instant,
    ended: bool,
    /// Tools built while no terminal was attached, told to the next one.
    pending_tools: Vec<ToolImage>,
}

struct Attachment {
    generation: u64,
    frames: mpsc::Sender<ServerFrame>,
}

impl Slot {
    fn lock(&self) -> std::sync::MutexGuard<'_, SlotShared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn touch(&self) {
        self.lock().last_activity = Instant::now();
    }

    /// Attach a terminal: the scrollback and every later frame go to the
    /// returned receiver, in order. The previous terminal is superseded.
    fn attach(&self, resumed: bool) -> Option<(u64, mpsc::Receiver<ServerFrame>)> {
        let (frames, receiver) = mpsc::channel(ATTACHMENT_QUEUE);
        let mut shared = self.lock();
        if shared.ended {
            return None;
        }
        let scrollback = shared.scrollback.iter().copied().collect();
        // A fresh channel has room for the first frame.
        frames
            .try_send(ServerFrame::Opened {
                session: self.id,
                resumed,
                scrollback,
            })
            .ok()?;
        for tool in std::mem::take(&mut shared.pending_tools) {
            let _ = frames.try_send(ServerFrame::ToolBuilt {
                record: self.record.clone(),
                tool,
            });
        }
        let generation = shared.next_generation;
        shared.next_generation = generation.wrapping_add(1);
        shared.last_activity = Instant::now();
        if let Some(previous) = shared.attachment.replace(Attachment { generation, frames }) {
            let _ = previous.frames.try_send(ServerFrame::Superseded);
        }
        drop(shared);
        Some((generation, receiver))
    }

    fn detach(&self, generation: u64) {
        let mut shared = self.lock();
        if shared
            .attachment
            .as_ref()
            .is_some_and(|attachment| attachment.generation == generation)
        {
            shared.attachment = None;
            shared.last_activity = Instant::now();
        }
    }

    /// Guest output: keep it, and pass it to the attached terminal. A
    /// terminal that cannot keep up is detached; it re-attaches and reads
    /// the scrollback.
    fn output(&self, bytes: &[u8]) {
        let mut shared = self.lock();
        shared.last_activity = Instant::now();
        shared.scrollback.extend(bytes);
        let excess = shared.scrollback.len().saturating_sub(MAX_SCROLLBACK_BYTES);
        shared.scrollback.drain(..excess);
        let stalled = shared.attachment.as_ref().is_some_and(|attachment| {
            attachment
                .frames
                .try_send(ServerFrame::Output(bytes.to_vec()))
                .is_err()
        });
        if stalled {
            shared.attachment = None;
        }
    }

    fn end(&self, code: Option<i32>) {
        let mut shared = self.lock();
        shared.ended = true;
        if let Some(attachment) = shared.attachment.take() {
            let _ = attachment.frames.try_send(ServerFrame::Exited { code });
        }
    }

    fn idle_since(&self) -> Option<Instant> {
        let shared = self.lock();
        (shared.attachment.is_none() && !shared.ended).then_some(shared.last_activity)
    }
}

/// Failure on one environment stream.
#[derive(Debug, Error)]
pub enum EnvironmentStreamError {
    /// Framing or stream I/O.
    #[error("environment stream I/O: {0}")]
    Io(#[from] io::Error),
    /// A frame did not decode.
    #[error("environment stream codec: {0}")]
    Codec(#[from] postcard::Error),
    /// A frame could not be read.
    #[error("environment stream frame: {0}")]
    Frame(#[from] crate::frame::FrameError),
}

/// A request refused before any guest was attached.
#[derive(Debug, PartialEq, Eq)]
pub struct Refused {
    /// Why.
    pub reason: Refusal,
    /// Detail for the member.
    pub detail: String,
}

impl Refused {
    fn new(reason: Refusal, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: bounded_detail(detail),
        }
    }

    fn frame(&self) -> ServerFrame {
        ServerFrame::Refused {
            reason: self.reason,
            detail: self.detail.clone(),
        }
    }
}

impl EnvironmentHost {
    /// A host starting guests with `launcher`.
    #[must_use]
    pub fn new(
        config: EnvironmentHostConfig,
        launcher: Arc<dyn GuestLauncher>,
        metering: Arc<dyn Metering>,
    ) -> Self {
        Self {
            inner: Arc::new(HostInner {
                config,
                launcher,
                metering,
                sessions: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// The host's configuration.
    #[must_use]
    pub fn config(&self) -> &EnvironmentHostConfig {
        &self.inner.config
    }

    fn sessions(&self) -> std::sync::MutexGuard<'_, HashMap<SessionId, Arc<Slot>>> {
        self.inner
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Sessions running now.
    #[must_use]
    pub fn active_sessions(&self) -> usize {
        self.sessions().len()
    }

    /// Sessions `user` runs now.
    #[must_use]
    pub fn member_sessions(&self, user: &[u8; 32]) -> usize {
        self.sessions()
            .values()
            .filter(|slot| &slot.owner.user == user)
            .count()
    }

    /// Whether `session` runs here.
    #[must_use]
    pub fn has_session(&self, session: &SessionId) -> bool {
        self.sessions().contains_key(session)
    }

    /// The member owning `session`, when it runs here.
    #[must_use]
    pub fn session_owner(&self, session: &SessionId) -> Option<MemberIdentity> {
        self.sessions().get(session).map(|slot| slot.owner)
    }

    /// The record `session` was opened from, when it runs here.
    #[must_use]
    pub fn session_record(&self, session: &SessionId) -> Option<EnvironmentRecordRef> {
        self.sessions().get(session).map(|slot| slot.record.clone())
    }

    fn member_context(&self, member: MemberIdentity) -> MemberContext {
        MemberContext {
            identity: member,
            area: MemberArea::of(&self.inner.config.root, &member.user),
            quota: self.inner.config.quota,
        }
    }

    fn refuse_quota(&self, member: MemberIdentity, quota: QuotaKind, detail: &str) -> Refused {
        self.inner
            .metering
            .record(MeterEvent::QuotaRefused { member, quota });
        Refused::new(Refusal::Quota(quota), detail)
    }

    /// Start a new session for `member`. Its output is not read until the
    /// caller attaches and starts [`Self::drain_guest`], so the opener sees
    /// every byte as output rather than some of it as scrollback.
    async fn open(
        &self,
        member: MemberIdentity,
        session: SessionId,
        request: GuestRequest,
    ) -> Result<(Arc<Slot>, GuestOutput), Refused> {
        if !request.spec.valid() || !request.size.valid() {
            return Err(Refused::new(Refusal::InvalidRequest, "invalid environment"));
        }
        {
            let sessions = self.sessions();
            if sessions.contains_key(&session) {
                return Err(Refused::new(Refusal::InvalidRequest, "session id in use"));
            }
            let capacity = usize::try_from(self.inner.config.capacity).unwrap_or(usize::MAX);
            if sessions.len() >= capacity {
                return Err(Refused::new(Refusal::NoWorker, "this worker is full"));
            }
            let own = sessions
                .values()
                .filter(|slot| slot.owner.user == member.user)
                .count();
            drop(sessions);
            let quota = usize::try_from(self.inner.config.quota.max_sessions).unwrap_or(usize::MAX);
            if own >= quota {
                return Err(self.refuse_quota(
                    member,
                    QuotaKind::Sessions,
                    "you already run as many environment sessions as this relay allows",
                ));
            }
        }
        let context = self.member_context(member);
        let guests = Arc::clone(&self.inner.launcher);
        let started = tokio::task::spawn_blocking(move || {
            let usage = context
                .area
                .usage_bytes()
                .map_err(|error| LaunchError::Failed(format!("member area: {error}")))?;
            if usage > context.quota.disk_bytes {
                return Err(LaunchError::Quota(QuotaKind::Disk));
            }
            guests
                .launch(&context, &request)
                .map(|guest| (guest, request))
        })
        .await
        .map_err(|_| Refused::new(Refusal::LaunchFailed, "guest launch panicked"))?;
        let (guest, request) = match started {
            Ok(started) => started,
            Err(LaunchError::Quota(kind)) => {
                return Err(self.refuse_quota(member, kind, "your space on this relay is used up"));
            }
            Err(LaunchError::Failed(detail)) => {
                return Err(Refused::new(Refusal::LaunchFailed, detail));
            }
        };
        let Guest {
            output,
            input,
            control,
        } = guest;
        let slot = Arc::new(Slot {
            id: session,
            owner: member,
            known_tools: Mutex::new(
                request
                    .spec
                    .tools
                    .iter()
                    .map(|tool| tool.id.clone())
                    .collect(),
            ),
            record: request.record,
            opened_at: Instant::now(),
            input: tokio::sync::Mutex::new(input),
            control,
            shared: Mutex::new(SlotShared {
                scrollback: VecDeque::new(),
                attachment: None,
                next_generation: 0,
                last_activity: Instant::now(),
                ended: false,
                pending_tools: Vec::new(),
            }),
        });
        self.sessions().insert(session, Arc::clone(&slot));
        self.inner
            .metering
            .record(MeterEvent::SessionOpened { member, session });
        Ok((slot, output))
    }

    async fn drain_guest(self, slot: Arc<Slot>, mut output: GuestOutput) {
        let mut buffer = vec![0; MAX_PTY_CHUNK_BYTES];
        loop {
            match output.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => slot.output(buffer.get(..read).unwrap_or_default()),
            }
        }
        let code = slot.control.exit_code();
        slot.end(code);
        let member = slot.owner;
        let remaining = {
            let mut sessions = self.sessions();
            sessions.remove(&slot.id);
            sessions
                .values()
                .filter(|other| other.owner.user == member.user)
                .count()
        };
        let wall_ms = u64::try_from(slot.opened_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.inner.metering.record(MeterEvent::SessionClosed {
            member,
            session: slot.id,
            wall_ms,
        });
        if remaining == 0 {
            let launcher = Arc::clone(&self.inner.launcher);
            let _ = tokio::task::spawn_blocking(move || launcher.member_idle(&member)).await;
        }
    }

    /// End every session that has been detached and quiet for longer than
    /// the idle timeout. Returns the sessions it ended.
    pub fn reap_idle(&self) -> Vec<SessionId> {
        let timeout = self.inner.config.idle_timeout;
        let idle: Vec<Arc<Slot>> = self
            .sessions()
            .values()
            .filter(|slot| {
                slot.idle_since()
                    .is_some_and(|since| since.elapsed() >= timeout)
            })
            .cloned()
            .collect();
        idle.iter()
            .map(|slot| {
                slot.control.terminate();
                slot.id
            })
            .collect()
    }

    /// Reap idle sessions until the host is dropped.
    pub fn spawn_reaper(&self) -> tokio::task::JoinHandle<()> {
        let weak = Arc::downgrade(&self.inner);
        let period = self
            .inner
            .config
            .idle_timeout
            .checked_div(4)
            .unwrap_or_default()
            .max(Duration::from_secs(1));
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(period);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let Some(inner) = weak.upgrade() else { return };
                let host = EnvironmentHost { inner };
                host.refresh_tools().await;
                let reaped = host.reap_idle();
                drop(host);
                if !reaped.is_empty() {
                    tracing::info!(count = reaped.len(), "reaped idle environment sessions");
                }
            }
        })
    }

    /// End every session (worker shutdown).
    pub fn terminate_all(&self) {
        let slots: Vec<Arc<Slot>> = self.sessions().values().cloned().collect();
        for slot in slots {
            slot.control.terminate();
        }
    }

    /// Serve one environment stream for `member`: open or re-attach, then
    /// carry the terminal until it detaches, is superseded, or the guest
    /// ends.
    ///
    /// # Errors
    ///
    /// Returns an error when the stream fails or a frame is malformed. A
    /// refused request is answered on the stream and is not an error.
    pub async fn serve<S, R>(
        &self,
        member: MemberIdentity,
        session: SessionId,
        request: SessionRequest,
        send: &mut S,
        receive: &mut R,
    ) -> Result<(), EnvironmentStreamError>
    where
        S: AsyncWrite + Unpin + Send,
        R: AsyncRead + Unpin + Send,
    {
        let attached = match request {
            SessionRequest::Open {
                record,
                spec,
                size,
                term,
            } => {
                let request = GuestRequest {
                    session,
                    record,
                    spec,
                    size,
                    term: term.unwrap_or_else(|| "xterm-256color".into()),
                };
                match self.open(member, session, request).await {
                    Ok((slot, output)) => {
                        let attached = self.attach(member, session, size, false);
                        tokio::spawn(self.clone().drain_guest(slot, output));
                        attached
                    }
                    Err(refused) => Err(refused),
                }
            }
            SessionRequest::Attach { session, size } => self.attach(member, session, size, true),
        };
        let (slot, generation, mut frames) = match attached {
            Ok(attached) => attached,
            Err(refused) => {
                write_message(send, &refused.frame()).await?;
                let _ = send.shutdown().await;
                return Ok(());
            }
        };
        let outcome = carry(&slot, &mut frames, send, receive).await;
        slot.detach(generation);
        let _ = send.shutdown().await;
        outcome
    }

    fn attach(
        &self,
        member: MemberIdentity,
        session: SessionId,
        size: PtySize,
        resumed: bool,
    ) -> Result<(Arc<Slot>, u64, mpsc::Receiver<ServerFrame>), Refused> {
        let slot = self
            .sessions()
            .get(&session)
            .cloned()
            .ok_or_else(|| Refused::new(Refusal::UnknownSession, "no such session"))?;
        if slot.owner.user != member.user {
            return Err(Refused::new(
                Refusal::NotOwner,
                "that session belongs to someone else",
            ));
        }
        let _ = slot.control.resize(size);
        let (generation, frames) = slot
            .attach(resumed)
            .ok_or_else(|| Refused::new(Refusal::UnknownSession, "the session has ended"))?;
        Ok((slot, generation, frames))
    }
}

async fn carry<S, R>(
    slot: &Slot,
    frames: &mut mpsc::Receiver<ServerFrame>,
    send: &mut S,
    receive: &mut R,
) -> Result<(), EnvironmentStreamError>
where
    S: AsyncWrite + Unpin + Send,
    R: AsyncRead + Unpin + Send,
{
    loop {
        tokio::select! {
            frame = frames.recv() => {
                let Some(frame) = frame else { return Ok(()) };
                let last = matches!(frame, ServerFrame::Exited { .. } | ServerFrame::Superseded);
                write_message(send, &frame).await?;
                if last {
                    return Ok(());
                }
            }
            incoming = read_message::<_, ClientFrame>(receive) => {
                match incoming? {
                    None | Some(ClientFrame::Detach) => return Ok(()),
                    Some(ClientFrame::Input(bytes)) => {
                        if bytes.len() > MAX_PTY_CHUNK_BYTES {
                            return Err(io::Error::other("input chunk exceeds its bound").into());
                        }
                        slot.touch();
                        let mut input = slot.input.lock().await;
                        input.write_all(&bytes).await?;
                        input.flush().await?;
                        drop(input);
                    }
                    Some(ClientFrame::Resize(size)) => {
                        if size.valid() {
                            let _ = slot.control.resize(size);
                        }
                    }
                    Some(ClientFrame::Terminate) => {
                        slot.control.terminate();
                    }
                }
            }
        }
    }
}

/// Write one postcard frame within the environment frame bound.
///
/// # Errors
///
/// Returns an error when encoding fails, the frame is too large, or the
/// write fails.
pub async fn write_message<W, T>(writer: &mut W, message: &T) -> Result<(), EnvironmentStreamError>
where
    W: AsyncWrite + Unpin + Send,
    T: Serialize + Sync,
{
    let bytes = postcard::to_allocvec(message)?;
    if bytes.len() > MAX_ENVIRONMENT_FRAME_BYTES {
        return Err(io::Error::other("environment frame exceeds its bound").into());
    }
    write_frame(writer, &bytes).await?;
    Ok(())
}

/// Read one postcard frame, or `None` at a clean end of stream.
///
/// # Errors
///
/// Returns an error when the stream fails mid-frame, the frame is too
/// large, or it does not decode.
pub async fn read_message<R, T>(reader: &mut R) -> Result<Option<T>, EnvironmentStreamError>
where
    R: AsyncRead + Unpin + Send,
    T: DeserializeOwned,
{
    let Some(bytes) = read_frame_or_eof(reader, MAX_ENVIRONMENT_FRAME_BYTES).await? else {
        return Ok(None);
    };
    Ok(Some(postcard::from_bytes(&bytes)?))
}

impl Slot {
    /// Tell the attached terminal about a tool built for this session's
    /// environment, or the next one to attach.
    fn tool_built(&self, tool: ToolImage) {
        let mut shared = self.lock();
        let sent = shared.attachment.as_ref().is_some_and(|attachment| {
            attachment
                .frames
                .try_send(ServerFrame::ToolBuilt {
                    record: self.record.clone(),
                    tool: tool.clone(),
                })
                .is_ok()
        });
        if !sent && shared.pending_tools.len() < MAX_PENDING_TOOLS {
            shared.pending_tools.push(tool);
        }
    }
}

impl EnvironmentHost {
    /// A tool was built for `session`'s environment: the member's device
    /// records it. Returns whether the session runs here.
    pub fn tool_built(&self, session: &SessionId, tool: ToolImage) -> bool {
        let slot = self.sessions().get(session).cloned();
        slot.map(|slot| slot.tool_built(tool)).is_some()
    }

    /// Tell each running session's terminal about tools built for its
    /// environment on this worker since it last looked. Returns how many.
    pub async fn refresh_tools(&self) -> usize {
        let slots: Vec<Arc<Slot>> = self.sessions().values().cloned().collect();
        let mut told = 0usize;
        for slot in slots {
            let context = self.member_context(slot.owner);
            let launcher = Arc::clone(&self.inner.launcher);
            let record = slot.record.clone();
            let Ok(Some(tools)) =
                tokio::task::spawn_blocking(move || launcher.current_tools(&context, &record))
                    .await
            else {
                continue;
            };
            let new: Vec<ToolImage> = {
                let mut known = slot
                    .known_tools
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let new = tools
                    .into_iter()
                    .filter(|tool| !known.contains(&tool.id))
                    .collect::<Vec<_>>();
                known.extend(new.iter().map(|tool| tool.id.clone()));
                new
            };
            for tool in new {
                slot.tool_built(tool);
                told = told.saturating_add(1);
            }
        }
        told
    }

    /// A handle that does not keep the host alive.
    #[must_use]
    pub fn downgrade(&self) -> WeakEnvironmentHost {
        WeakEnvironmentHost(Arc::downgrade(&self.inner))
    }
}

/// A [`EnvironmentHost`] handle that does not keep it alive.
#[derive(Clone)]
pub struct WeakEnvironmentHost(std::sync::Weak<HostInner>);

impl WeakEnvironmentHost {
    /// The host, while it lives.
    #[must_use]
    pub fn upgrade(&self) -> Option<EnvironmentHost> {
        self.0.upgrade().map(|inner| EnvironmentHost { inner })
    }
}

#[cfg(test)]
pub(crate) mod tests;
