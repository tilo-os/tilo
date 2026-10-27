//! Keeping a document channel's retained history short.
//!
//! A late joiner's cost follows a route's retained history: every entry is
//! replayed and applied. Main's generation store keeps an application
//! document's entries whole (their bodies are signed `AppDocument`s, which
//! its raw-Automerge proof cannot read), so a record edited for a long time
//! keeps thousands of changes. Retained growth on a channel schedules its
//! upkeep here once the channel has been quiet for [`QUIET`]:
//!
//! - **Cleartext entries** the host folds itself (`fold`). It verifies
//!   each frame the way a receiving device does, merges every unit it can
//!   read into one Automerge document and signs that snapshot as the host
//!   device, framed exactly as a device publishes one. Only entries whose
//!   every change the document contains, by change hash, are removed; what
//!   was retained meanwhile, and whatever the host cannot read, is kept.
//! - **Sealed entries** the host cannot read. It asks a member device that
//!   volunteered ([`Envelope::CompactRequest`]); the member commits a sealed
//!   snapshot and signs a [`CoverageStatement`] naming the entries it
//!   contains, and the host removes exactly those, from the channel it asked
//!   about and only on the word of the member it asked, while that member
//!   still subscribes.
//!
//! Admission authenticates the device that sent a delta, not the document
//! signature inside it, so compaction verifies every entry it relies on
//! itself, once per process for the same bytes (`verified` holds the
//! trust argument).
//!
//! The host's snapshot is retained, never fanned out: connected devices
//! already hold its content. Its certificate is retained beside it, so a
//! replay carries it first (certificate lineages lead a replay) and a native
//! shell accepts the host-signed state. Rooms whose replay a native shell
//! selects (a bound `shell_binding`) are left to that selection.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ChangeHash};
use collab_types::{DocumentMutation, DocumentMutationKind};
use tilo_automerge::ApplyIncoming as _;
use tilo_relay_protocol::compaction::{CoverageStatement, entry_id, snapshot_heads, verify};

mod verified;

use verified::{Checked, Piece, Signatures};

use crate::coedit::CoeditPayload;
pub use crate::payloads::Route;
use crate::payloads::ShellPayloads;
use crate::store::compaction::{ChannelEntry, SnapshotWrite, file_digest};
use crate::transport::Envelope;
use crate::workspace::SharedWorkspace;
use tilo_lock::MutexRecover as _;

/// Tail size past which a channel is compacted: this many entries, or
/// [`COMPACT_TAIL_BYTES`] that are at least half its snapshot's size, so a
/// large document is not reloaded for every few changes.
pub const COMPACT_TAIL_ENTRIES: usize = 256;
/// See [`COMPACT_TAIL_ENTRIES`].
pub const COMPACT_TAIL_BYTES: usize = 1024 * 1024;
/// Largest channel (saved bytes) the host folds itself. A loaded document
/// takes far more memory than its saved form.
pub const MAX_COMPACTED_BYTES: usize = 16 * 1024 * 1024;
/// A burst of edits is compacted once it ends, not reloaded during it.
pub const QUIET: Duration = Duration::from_secs(2);
/// Growth since the last look that makes a channel worth looking at.
const CHECK_ENTRIES: usize = 64;
const CHECK_BYTES: usize = 512 * 1024;
/// How long a member has to answer a request before another may be asked.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(250);
/// How long a request waits for a volunteer that holds every delivery.
const CATCH_UP: Duration = Duration::from_secs(10);
const CATCH_UP_POLL: Duration = Duration::from_millis(20);

/// Document-envelope signatures the compactor verified in this process.
static SIGNATURE_CHECKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Thread CPU time compaction spent in this process: reading a channel,
/// deciding, folding and writing.
static CPU_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Document-envelope signatures compaction verified in this process: what
/// the hosting bench divides by edits.
#[must_use]
pub fn signature_checks() -> u64 {
    SIGNATURE_CHECKS.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
thread_local! {
    /// [`SIGNATURE_CHECKS`] of this thread only: tests run in parallel.
    static THREAD_SIGNATURE_CHECKS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn count_signature_check() {
    SIGNATURE_CHECKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    #[cfg(test)]
    THREAD_SIGNATURE_CHECKS.with(|checks| checks.set(checks.get().saturating_add(1)));
}

#[cfg(test)]
fn thread_signature_checks() -> u64 {
    THREAD_SIGNATURE_CHECKS.with(std::cell::Cell::get)
}

/// Thread CPU nanoseconds compaction spent in this process (0 where the
/// platform has no per-thread clock).
#[must_use]
pub fn cpu_ns() -> u64 {
    CPU_NS.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(unix)]
fn thread_cpu_ns() -> Option<u64> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `time` is a valid, writable timespec for the duration of the
    // call; an unsupported clock reports an error instead of writing.
    let status = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut time) };
    if status != 0 {
        return None;
    }
    u64::try_from(time.tv_sec)
        .ok()?
        .checked_mul(1_000_000_000)?
        .checked_add(u64::try_from(time.tv_nsec).ok()?)
}

#[cfg(not(unix))]
const fn thread_cpu_ns() -> Option<u64> {
    None
}

/// Run `work` on this thread and count its CPU time in [`cpu_ns`].
fn on_cpu<T>(work: impl FnOnce() -> T) -> T {
    let start = thread_cpu_ns();
    let out = work();
    if let (Some(start), Some(end)) = (start, thread_cpu_ns()) {
        CPU_NS.fetch_add(
            end.saturating_sub(start),
            std::sync::atomic::Ordering::Relaxed,
        );
    }
    out
}

#[derive(Default)]
struct Growth {
    entries: usize,
    bytes: usize,
    last: Option<Instant>,
    scheduled: bool,
    /// Deltas of the channel being admitted ([`Admitting`]).
    admitting: usize,
}

struct Outstanding {
    workspace_id: u64,
    channel: [u8; 32],
    device: [u8; 32],
    sent: Instant,
}

/// A workspace and one of its retained channels.
type ChannelKey = (u64, [u8; 32]);

struct Inner {
    key: ed25519_dalek::SigningKey,
    cert: tilo_user_identity::DeviceCert,
    /// Reads document frames and frames the host's own retained bodies.
    payloads: Arc<dyn ShellPayloads>,
    jobs: tokio::sync::Semaphore,
    growth: Mutex<HashMap<ChannelKey, Growth>>,
    volunteers: Mutex<HashMap<u64, Vec<[u8; 32]>>>,
    requests: Mutex<HashMap<[u8; 16], Outstanding>>,
    /// Entry ids of the member snapshot last accepted per channel: its
    /// segments are the snapshot, not a tail to compact again.
    member_snapshots: Mutex<HashMap<ChannelKey, HashSet<[u8; 32]>>>,
    rounds: std::sync::atomic::AtomicU64,
    folding: std::sync::atomic::AtomicU64,
    hold: Mutex<Duration>,
    /// Channels whose last fold ran before they went quiet.
    forced: Mutex<HashSet<ChannelKey>>,
    /// Entry bodies per channel whose signature verified in this process.
    signatures: Mutex<HashMap<ChannelKey, Signatures>>,
    /// The record channels this host stewards, whose sealed history it
    /// folds itself.
    steward: std::sync::OnceLock<Arc<dyn StewardedKeys>>,
    /// The route of each channel found stewarded: only ever folded sealed
    /// once `resealing`.
    stewarded: Mutex<HashMap<ChannelKey, Route>>,
    /// Whether what stewarded channels retain in the clear is sealed: only
    /// once the host's operator decided it ([`crate::reseal`]).
    resealing: std::sync::atomic::AtomicBool,
}

/// Opens a sealed document frame, or refuses it.
pub(crate) type Opener<'a> = dyn Fn(&collab_types::SignedOp) -> Option<Vec<u8>> + 'a;

/// The content keys of the record channels a host stewards (the rooms it
/// governs as a node that may own workspaces). It is a member of each
/// channel's group, so it reads and seals a sealed record like any member.
pub(crate) trait StewardedKeys: Send + Sync {
    /// The keys of `route`'s channel, when this host stewards it.
    fn keys<'a>(
        &'a self,
        route: &'a Route,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Option<channel_crypto::ChannelContentKeys>>
                + Send
                + 'a,
        >,
    >;
}

/// A retained document delta, as [`Compactor::note`] counts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub struct Observed {
    channel: [u8; 32],
    len: usize,
}

/// Schedules and runs compaction for the workspaces of one host.
#[derive(Clone)]
pub struct Compactor(Arc<Inner>);

/// A delta of an observed channel being admitted. The channel is not quiet
/// while one is: a commit whose durability stalls must not let a round start
/// in the middle of its burst. Dropping it ends the admission.
pub(crate) struct Admitting {
    compactor: Compactor,
    key: ChannelKey,
}

impl Drop for Admitting {
    fn drop(&mut self) {
        if let Some(growth) = lock(&self.compactor.0.growth).get_mut(&self.key) {
            growth.admitting = growth.admitting.saturating_sub(1);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock_recover()
}

impl Compactor {
    /// A compactor signing as this host device, reading and framing
    /// co-edit bodies with `payloads`.
    pub fn new(
        key: ed25519_dalek::SigningKey,
        cert: tilo_user_identity::DeviceCert,
        payloads: Arc<dyn ShellPayloads>,
    ) -> Self {
        Self(Arc::new(Inner {
            key,
            cert,
            payloads,
            jobs: tokio::sync::Semaphore::new(1),
            growth: Mutex::default(),
            volunteers: Mutex::default(),
            requests: Mutex::default(),
            member_snapshots: Mutex::default(),
            rounds: std::sync::atomic::AtomicU64::new(0),
            folding: std::sync::atomic::AtomicU64::new(0),
            hold: Mutex::new(Duration::ZERO),
            forced: Mutex::default(),
            signatures: Mutex::default(),
            steward: std::sync::OnceLock::new(),
            stewarded: Mutex::default(),
            resealing: std::sync::atomic::AtomicBool::new(false),
        }))
    }

    /// Fold the sealed history of the channels `keys` names, as their
    /// steward, instead of asking a member to.
    #[cfg(any(feature = "agents", feature = "web"))]
    pub(crate) fn steward_with(&self, keys: Arc<dyn StewardedKeys>) {
        let _ = self.0.steward.set(keys);
    }

    /// Seal what the channels this host stewards retain in the clear, from
    /// now on: the operator decided it ([`crate::reseal`]).
    #[cfg(feature = "agents")]
    pub(crate) fn reseal_existing(&self) {
        self.0
            .resealing
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    fn resealing(&self) -> bool {
        self.0.resealing.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Stop: wait for the round under way, and start none after it, so
    /// the relay's state no longer changes once its relay stopped.
    pub(crate) async fn stop(&self) {
        if let Ok(round) = self.0.jobs.acquire().await {
            self.0.jobs.close();
            drop(round);
        }
    }

    /// Compactions finished so far (host folds and accepted statements).
    pub fn rounds(&self) -> u64 {
        self.0.rounds.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The document channel a delta retains to, and its size, before it is
    /// admitted, as this compactor's payload adapter reads it.
    pub(crate) fn observe(&self, delta: &[u8]) -> Option<Observed> {
        Self::observe_with(&*self.0.payloads, delta)
    }

    /// [`Self::observe`] with `payloads`.
    ///
    /// Runs for every delta on the session task, so it borrows the payload
    /// body and asks the payload adapter only whether it is a document
    /// frame: no allocation or copy proportional to the delta. Public only so
    /// an allocation test with its own counting allocator can measure it.
    #[doc(hidden)]
    pub fn observe_with(payloads: &dyn ShellPayloads, delta: &[u8]) -> Option<Observed> {
        let (channel, _, body) = crate::coedit::retention_view(delta)?;
        payloads.is_document(body).then_some(Observed {
            channel,
            len: delta.len(),
        })
    }

    /// An observed delta of `workspace_id` is being admitted, until the
    /// returned guard drops.
    pub(crate) fn admitting(&self, workspace_id: u64, observed: Observed) -> Admitting {
        let key = (workspace_id, observed.channel);
        let mut growth = lock(&self.0.growth);
        let entry = growth.entry(key).or_default();
        entry.admitting = entry.admitting.saturating_add(1);
        drop(growth);
        Admitting {
            compactor: self.clone(),
            key,
        }
    }

    /// An observed delta was retained in `workspace_id`: schedule its
    /// channel's upkeep when it grew enough.
    pub(crate) fn note(&self, workspace: &SharedWorkspace, workspace_id: u64, observed: Observed) {
        let Observed { channel, len } = observed;
        let schedule = {
            let mut growth = lock(&self.0.growth);
            let entry = growth.entry((workspace_id, channel)).or_default();
            entry.entries = entry.entries.saturating_add(1);
            entry.bytes = entry.bytes.saturating_add(len);
            entry.last = Some(Instant::now());
            let due = entry.entries >= CHECK_ENTRIES || entry.bytes >= CHECK_BYTES;
            let schedule = due && !entry.scheduled;
            entry.scheduled |= schedule;
            drop(growth);
            schedule
        };
        if schedule {
            let this = self.clone();
            let workspace = workspace.clone();
            tokio::spawn(async move { this.upkeep(workspace, workspace_id, channel).await });
        }
    }

    async fn upkeep(self, workspace: SharedWorkspace, workspace_id: u64, channel: [u8; 32]) {
        let key = (workspace_id, channel);
        let quiet = loop {
            tokio::time::sleep(POLL).await;
            let (quiet, large) = lock(&self.0.growth)
                .get(&key)
                .map_or((true, false), |growth| {
                    (
                        growth.admitting == 0
                            && growth.last.is_none_or(|last| last.elapsed() >= QUIET),
                        growth.entries >= COMPACT_TAIL_ENTRIES.saturating_mul(4)
                            || growth.bytes >= COMPACT_TAIL_BYTES.saturating_mul(4),
                    )
                });
            if quiet || large {
                break quiet;
            }
        };
        let Ok(_job) = self.0.jobs.acquire().await else {
            return;
        };
        if let Some(growth) = lock(&self.0.growth).get_mut(&key) {
            *growth = Growth {
                admitting: growth.admitting,
                ..Growth::default()
            };
        }
        // A fold forced during a burst leaves the burst's end behind; the
        // fold after it goes quiet takes that end even when it is short.
        let follow_up = quiet && lock(&self.0.forced).remove(&key);
        if !quiet {
            lock(&self.0.forced).insert(key);
        }
        let min_tail = if follow_up {
            CHECK_ENTRIES
        } else {
            COMPACT_TAIL_ENTRIES
        };
        if let Err(error) = self.run(&workspace, workspace_id, channel, min_tail).await {
            tracing::warn!(workspace_id, %error, "document compaction failed");
        }
    }

    async fn run(
        &self,
        workspace: &SharedWorkspace,
        workspace_id: u64,
        channel: [u8; 32],
        min_tail: usize,
    ) -> Result<(), String> {
        let store = workspace.lock().await.store.clone();
        if store.shell_binding().is_some() {
            return Ok(());
        }
        let resealing = self.resealing();
        let stewarded = resealing
            .then(|| {
                lock(&self.0.stewarded)
                    .get(&(workspace_id, channel))
                    .cloned()
            })
            .flatten();
        if let Some(route) = stewarded
            && self
                .fold_as_steward(workspace, workspace_id, channel, &route, min_tail)
                .await?
        {
            return Ok(());
        }
        let known = lock(&self.0.member_snapshots)
            .get(&(workspace_id, channel))
            .cloned()
            .unwrap_or_default();
        // One round at a time (`jobs`), so the channel's record is taken out
        // for the round and put back after it.
        let mut signatures = lock(&self.0.signatures)
            .remove(&(workspace_id, channel))
            .unwrap_or_else(|| Signatures::with_payloads(self.0.payloads.clone()));
        let key = self.0.key.clone();
        let payloads = self.0.payloads.clone();
        let (signatures, planned) = tokio::task::spawn_blocking(move || {
            let planned = on_cpu(|| {
                let entries = store.channel_entries(&channel)?;
                Ok::<_, crate::store::StoreError>(plan(
                    &entries,
                    &mut signatures,
                    &known,
                    min_tail,
                    |route, snapshot| {
                        document_frames(&*payloads, &key, workspace_id, route, snapshot).ok()
                    },
                ))
            });
            (signatures, planned)
        })
        .await
        .map_err(|error| error.to_string())?;
        lock(&self.0.signatures).insert((workspace_id, channel), signatures);
        let (sealed, plan) = planned.map_err(|error| error.to_string())?;
        // Once resealing, a channel this host stewards is folded sealed,
        // never into a snapshot in the clear.
        let route = plan
            .as_ref()
            .map(|(folded, _)| &folded.route)
            .or(sealed.as_ref())
            .filter(|_| resealing);
        if let Some(route) = route
            && self
                .fold_as_steward(workspace, workspace_id, channel, route, min_tail)
                .await?
        {
            return Ok(());
        }
        if let Some((folded, frames)) = plan {
            self.0
                .folding
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let held = Instant::now();
            while held.elapsed() < *lock(&self.0.hold) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            self.persist_fold(workspace, channel, &folded, frames)
                .await?;
        }
        if let Some(route) = sealed {
            if !resealing
                && self
                    .fold_as_steward(workspace, workspace_id, channel, &route, min_tail)
                    .await?
            {
                return Ok(());
            }
            let (this, workspace) = (self.clone(), workspace.clone());
            tokio::spawn(async move { this.ask(&workspace, workspace_id, channel, &route).await });
        }
        Ok(())
    }

    /// Fold `channel`'s sealed history as the steward of `route`'s channel,
    /// when this host is: open every sealed entry with the channel's keys,
    /// fold them as a cleartext channel is folded, and retain the snapshot
    /// sealed under the channel's current key, signed as this host. A
    /// newcomer then replays one snapshot and the writes since, not every
    /// write ever made. False when this host does not steward the channel.
    async fn fold_as_steward(
        &self,
        workspace: &SharedWorkspace,
        workspace_id: u64,
        channel: [u8; 32],
        route: &Route,
        min_tail: usize,
    ) -> Result<bool, String> {
        let Some(steward) = self.0.steward.get() else {
            return Ok(false);
        };
        let Some(keys) = steward.keys(route).await else {
            return Ok(false);
        };
        lock(&self.0.stewarded).insert((workspace_id, channel), route.clone());
        let store = workspace.lock().await.store.clone();
        let mut signatures = lock(&self.0.signatures)
            .remove(&(workspace_id, channel))
            .unwrap_or_else(|| Signatures::with_payloads(self.0.payloads.clone()));
        let key = self.0.key.clone();
        let payloads = self.0.payloads.clone();
        let resealing = self.resealing();
        let (signatures, planned) = tokio::task::spawn_blocking(move || {
            let planned = on_cpu(|| {
                let entries = store.channel_entries(&channel)?;
                let open = |op: &collab_types::SignedOp| {
                    let metadata = op.encryption?;
                    let ciphertext = collab_types::sealed_ciphertext(op).ok()?;
                    keys.open_content(op.artifact, &metadata, &ciphertext)
                        .ok()
                        .map(|plaintext| plaintext.to_vec())
                };
                Ok::<_, crate::store::StoreError>(
                    plan_opening(
                        &entries,
                        &mut signatures,
                        &HashSet::new(),
                        min_tail,
                        (Some(&open), resealing),
                        |route, snapshot| {
                            sealed_document_frames(
                                &*payloads,
                                (&key, &keys),
                                workspace_id,
                                route,
                                snapshot,
                            )
                            .ok()
                        },
                    )
                    .1,
                )
            });
            (signatures, planned)
        })
        .await
        .map_err(|error| error.to_string())?;
        lock(&self.0.signatures).insert((workspace_id, channel), signatures);
        let Some((folded, frames)) = planned.map_err(|error| error.to_string())? else {
            return Ok(true);
        };
        self.0
            .folding
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.persist_fold(workspace, channel, &folded, frames)
            .await?;
        Ok(true)
    }

    async fn persist_fold(
        &self,
        workspace: &SharedWorkspace,
        channel: [u8; 32],
        folded: &Folded,
        frames: Vec<Vec<u8>>,
    ) -> Result<(), String> {
        let identity = identity_frame(
            &*self.0.payloads,
            &self.0.cert,
            workspace.lock().await.store.workspace_id(),
        )?;
        let remove: HashSet<[u8; 32]> = folded.removed.iter().copied().collect();
        let author = self.0.cert.device_public_key;
        let outcome = owner_run(workspace, move |store| {
            store.retain_checkpoint(&identity.0, &identity.1)?;
            store.replace_channel(&channel, Some(SnapshotWrite { author, frames }), &|bytes| {
                remove.contains(&file_digest(bytes))
            })
        })
        .await?;
        self.0
            .rounds
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tracing::info!(
            removed = outcome.removed,
            kept = outcome.kept,
            snapshot_bytes = folded.snapshot.len(),
            "host compacted a document channel"
        );
        Ok(())
    }

    /// Seal what `channel` of `workspace_id`, the channel of `route` this
    /// host stewards, retains in the clear: writes made before its channel
    /// was opened. Their history goes into the sealed snapshot whole, every
    /// change with its author, and they are removed only once it is durable.
    #[cfg(feature = "agents")]
    pub(crate) async fn reseal(
        &self,
        workspace: &SharedWorkspace,
        workspace_id: u64,
        channel: [u8; 32],
        route: &Route,
    ) -> Result<(), String> {
        let Ok(_job) = self.0.jobs.acquire().await else {
            return Ok(());
        };
        if workspace.lock().await.store.shell_binding().is_some() {
            return Ok(());
        }
        self.fold_as_steward(
            workspace,
            workspace_id,
            channel,
            route,
            COMPACT_TAIL_ENTRIES,
        )
        .await
        .map(drop)
    }

    /// `device` answers requests for `workspace_id` while subscribed.
    pub(crate) fn volunteer(&self, workspace_id: u64, device: [u8; 32]) {
        let mut volunteers = lock(&self.0.volunteers);
        let list = volunteers.entry(workspace_id).or_default();
        if !list.contains(&device) {
            list.push(device);
        }
        drop(volunteers);
    }

    /// Ask volunteers in turn until one answers for `channel`: a volunteer
    /// that leaves a request unanswered does not keep the channel from being
    /// compacted.
    async fn ask(
        &self,
        workspace: &SharedWorkspace,
        workspace_id: u64,
        channel: [u8; 32],
        route: &Route,
    ) {
        let volunteers = lock(&self.0.volunteers)
            .get(&workspace_id)
            .map_or(0, Vec::len);
        for _ in 0..volunteers.max(1) {
            let Some(request) = self.request(workspace, workspace_id, channel, route).await else {
                return;
            };
            tokio::time::sleep(REQUEST_TIMEOUT).await;
            if lock(&self.0.requests).remove(&request).is_none() {
                return;
            }
        }
    }

    async fn request(
        &self,
        workspace: &SharedWorkspace,
        workspace_id: u64,
        channel: [u8; 32],
        route: &Route,
    ) -> Option<[u8; 16]> {
        {
            let mut requests = lock(&self.0.requests);
            requests.retain(|_, request| request.sent.elapsed() < REQUEST_TIMEOUT);
            if requests
                .values()
                .any(|request| request.workspace_id == workspace_id && request.channel == channel)
            {
                return None;
            }
        }
        let candidates = lock(&self.0.volunteers)
            .get(&workspace_id)
            .cloned()
            .unwrap_or_default();
        // A member answers with the replica it holds when the request reaches
        // it. The request overtakes deliveries still queued for a member or
        // left on disk while it lags, so a member that took them all is asked
        // first; after CATCH_UP any open one is.
        let asked = Instant::now();
        let (device, tx) = loop {
            let runtime = workspace.lock().await;
            let open: Vec<_> = candidates
                .iter()
                .filter_map(|device| {
                    let subscriber = runtime
                        .subscribers
                        .get(device)
                        .filter(|subscriber| !subscriber.tx.is_closed())?;
                    let caught_up = runtime
                        .state_sessions
                        .get(device)
                        .is_none_or(crate::peer::StateSession::caught_up);
                    Some((*device, subscriber.tx.clone(), caught_up))
                })
                .collect();
            drop(runtime);
            let chosen = open
                .iter()
                .find(|(_, _, caught_up)| *caught_up)
                .or_else(|| open.first().filter(|_| asked.elapsed() >= CATCH_UP));
            if let Some((device, tx, _)) = chosen {
                break (*device, tx.clone());
            }
            if open.is_empty() {
                return None;
            }
            tokio::time::sleep(CATCH_UP_POLL).await;
        };
        let request: [u8; 16] = rand::random();
        if tx
            .try_send(Envelope::CompactRequest {
                workspace_id,
                request,
                app_id: route.app_id.clone(),
                cluster_id: route.cluster_id.clone(),
                record: route.record_id.clone(),
            })
            .is_err()
        {
            return None;
        }
        lock(&self.0.requests).insert(
            request,
            Outstanding {
                workspace_id,
                channel,
                device,
                sent: Instant::now(),
            },
        );
        // The next request for this workspace goes to someone else first.
        if let Some(list) = lock(&self.0.volunteers).get_mut(&workspace_id)
            && let Some(at) = list.iter().position(|candidate| *candidate == device)
        {
            let chosen = list.remove(at);
            list.push(chosen);
        }
        Some(request)
    }

    /// A member's answer: remove exactly the entries its snapshot covers, in
    /// the channel the host asked that member about.
    ///
    /// Only the device asked answers, while it still subscribes: removing a
    /// device from the room drops its subscription at once, though its
    /// session lives until the next access audit. A statement from anyone
    /// else leaves the request open for the device asked.
    ///
    /// # Errors
    ///
    /// Returns why the statement was refused; nothing is removed then.
    pub(crate) async fn compacted(
        &self,
        workspace: &SharedWorkspace,
        workspace_id: u64,
        device: [u8; 32],
        statement: &[u8],
        signature: &[u8; 64],
    ) -> Result<usize, String> {
        let decoded = CoverageStatement::decode(statement).map_err(|error| error.to_string())?;
        if !verify(&device, statement, signature) {
            return Err("statement signature does not verify".into());
        }
        {
            let mut requests = lock(&self.0.requests);
            let outstanding = requests
                .get(&decoded.request)
                .ok_or("no outstanding compaction request")?;
            if outstanding.device != device
                || outstanding.workspace_id != workspace_id
                || decoded.workspace_id != workspace_id
                || decoded.channel != outstanding.channel
            {
                return Err("statement does not answer this device's request".into());
            }
            requests.remove(&decoded.request);
        }
        if !workspace.lock().await.has_subscriber(&device) {
            return Err("the statement's device no longer subscribes to the workspace".into());
        }
        let channel = decoded.channel;
        let snapshot: HashSet<[u8; 32]> = decoded.snapshot.iter().copied().collect();
        let covered: HashSet<[u8; 32]> = decoded
            .covered
            .iter()
            .copied()
            .filter(|id| !snapshot.contains(id))
            .collect();
        if snapshot.is_empty() {
            return Err("statement names no snapshot".into());
        }
        let retained_snapshot = snapshot.clone();
        let workspace_number = workspace_id;
        let outcome = owner_run(workspace, move |store| {
            let present: HashSet<[u8; 32]> = store
                .channel_entries(&channel)?
                .iter()
                .filter(|entry| entry.author == device)
                .map(|entry| entry_id(&entry.body))
                .collect();
            if !retained_snapshot.is_subset(&present) {
                return Err(crate::persistence::error(
                    "the statement's snapshot is not retained",
                ));
            }
            store.replace_channel(&channel, None, &|bytes| {
                crate::store::retained::body_of(bytes, workspace_number)
                    .is_some_and(|body| covered.contains(&entry_id(&body)))
            })
        })
        .await?;
        lock(&self.0.member_snapshots).insert((workspace_id, channel), snapshot);
        self.0
            .rounds
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tracing::info!(
            workspace_id,
            removed = outcome.removed,
            kept = outcome.kept,
            "member compacted a sealed document channel"
        );
        Ok(outcome.removed)
    }
}

/// Run `operation` on the workspace's persistence owner, serialized with
/// every other retained write.
pub(crate) async fn owner_run<T: Send + 'static>(
    workspace: &SharedWorkspace,
    operation: impl FnOnce(&crate::store::WorkspaceStore) -> Result<T, crate::store::StoreError>
    + Send
    + 'static,
) -> Result<T, String> {
    let owner = crate::persistence::handle(workspace).await;
    let receipt = owner
        .submit_prepared_wait(
            4096,
            async { Ok(()) },
            move |state, ()| {
                // Retention and channel replacement each forget what
                // captures listed of what they write.
                state.retaining_only();
                on_cpu(|| operation(&state.store))
            },
            |result| async { result },
        )
        .await
        .map_err(|error| error.to_string())?;
    crate::persistence::receipt(receipt)
        .await
        .map_err(|error| error.to_string())
}

/// The host's certificate as a retained frame: the channel and the framed
/// envelope.
fn identity_frame(
    payloads: &dyn ShellPayloads,
    cert: &tilo_user_identity::DeviceCert,
    workspace_id: u64,
) -> Result<([u8; 32], Vec<u8>), String> {
    let (channel, body) = payloads.host_identity(cert)?;
    let bytes = crate::transport::encode(&Envelope::Delta {
        workspace_id,
        author_device_public_key: cert.device_public_key,
        authored_at_ns: tilo_clock::unix_now_ns(),
        delta: CoeditPayload::Checkpoint { channel, body }
            .encode()
            .map_err(|error| error.to_string())?,
        signature: [0; 64],
    })
    .map_err(|error| error.to_string())?;
    Ok((channel, bytes))
}

/// What one round decided: the route of a sealed tail to ask a member for,
/// and the fold with its signed frames.
type Plan = (Option<Route>, Option<(Folded, Vec<Vec<u8>>)>);

/// One round over a channel's `entries`: check each once, then find a
/// sealed tail to ask a member for and, when due, fold the readable part
/// and sign it with `sign`.
fn plan(
    entries: &[ChannelEntry],
    signatures: &mut Signatures,
    known: &HashSet<[u8; 32]>,
    min_tail: usize,
    sign: impl FnOnce(&Route, &[u8]) -> Option<Vec<Vec<u8>>>,
) -> Plan {
    plan_opening(entries, signatures, known, min_tail, (None, false), sign)
}

/// [`plan`], reading sealed entries `open` opens (the host stewards their
/// channel) like cleartext ones; `reseal` seals what it retains in the
/// clear.
fn plan_opening(
    entries: &[ChannelEntry],
    signatures: &mut Signatures,
    known: &HashSet<[u8; 32]>,
    min_tail: usize,
    (open, reseal): (Option<&Opener<'_>>, bool),
    sign: impl FnOnce(&Route, &[u8]) -> Option<Vec<Vec<u8>>>,
) -> Plan {
    let checked: Vec<Checked<'_>> = entries
        .iter()
        .map(|entry| signatures.check_opening(entry, open))
        .collect();
    signatures.end_round();
    let sealed = sealed_tail(&checked, known);
    // A channel its steward reads holds nothing in the clear: what was
    // written before its channel was opened is sealed into its snapshot,
    // history and authors whole, however short.
    let reseal = reseal && open.is_some() && checked.iter().any(Checked::is_cleartext);
    let readable = units(checked);
    let folded = compaction_due(entries, &readable, min_tail, reseal)
        .then(|| fold(entries, &readable, if reseal { 1 } else { 2 }))
        .flatten()
        .and_then(|folded| {
            let signed = sign(&folded.route, &folded.snapshot)?;
            Some((folded, signed))
        });
    if reseal && folded.is_none() {
        tracing::warn!(
            route = ?readable.route,
            "a stewarded channel keeps writes made in the clear: they do not fold"
        );
    }
    (sealed, folded)
}

/// What a document channel retains, as a reader holding `open` reads it.
#[cfg(feature = "agents")]
#[derive(Clone, Debug, Default)]
pub(crate) struct Retained {
    /// Entries written in the clear that hold a change.
    pub cleartext: usize,
    /// Every change it reads, by actor and sequence.
    pub changes: std::collections::BTreeSet<(Vec<u8>, u64)>,
}

/// Read `entries` of one channel as a reader holding `open` does: each
/// verified as compaction verifies it, then merged.
#[cfg(feature = "agents")]
pub(crate) fn retained(
    payloads: Arc<dyn ShellPayloads>,
    entries: &[ChannelEntry],
    open: Option<&Opener<'_>>,
) -> Retained {
    let mut signatures = Signatures::with_payloads(payloads);
    let checked: Vec<Checked<'_>> = entries
        .iter()
        .map(|entry| signatures.check_opening(entry, open))
        .collect();
    let clear: HashSet<usize> = checked
        .iter()
        .enumerate()
        .filter(|(_, checked)| checked.is_cleartext())
        .map(|(at, _)| at)
        .collect();
    let readable = units(checked);
    let mut doc = AutoCommit::new();
    let mut cleartext = 0usize;
    for unit in &readable.units {
        let holds = match &unit.mutation {
            None => false,
            Some(DocumentMutation::Snapshot(bytes)) => {
                AutoCommit::load(bytes).ok().is_some_and(|mut other| {
                    let holds = !other.get_heads().is_empty();
                    let _ = doc.merge_incoming(&mut other);
                    holds
                })
            }
            Some(DocumentMutation::Incremental(bytes)) => {
                let _ = doc.load_incoming(bytes);
                !bytes.is_empty()
            }
        };
        if holds {
            cleartext = cleartext
                .saturating_add(unit.entries.iter().filter(|at| clear.contains(at)).count());
        }
    }
    Retained {
        cleartext,
        changes: doc
            .get_changes(&[])
            .iter()
            .map(|change| (change.actor_id().to_bytes().to_vec(), change.seq()))
            .collect(),
    }
}

/// A channel's content in units the host can reason about: a whole
/// mutation, a complete segmented transfer, or something it cannot read.
struct Unit {
    entries: Vec<usize>,
    /// The mutation, or `None` for content the host cannot read.
    mutation: Option<DocumentMutation>,
}

/// Segments by (author, transfer digest): the entries holding them and the
/// segments themselves.
type Transfers = BTreeMap<([u8; 32], [u8; 32]), (Vec<usize>, Vec<collab_types::DocumentSegment>)>;

/// A channel's checked entries as units, and the route they name.
struct Readable {
    units: Vec<Unit>,
    route: Option<Route>,
}

/// Group checked entries (in the order of the entries they were checked
/// from) into units.
fn units(checked: Vec<Checked<'_>>) -> Readable {
    let mut units = Vec::new();
    let mut route: Option<Route> = None;
    let mut transfers: Transfers = BTreeMap::new();
    for (index, checked) in checked.into_iter().enumerate() {
        let author = checked.entry().author;
        let (named, piece) = checked.into_parts();
        // One channel is one route; a frame naming another is not read.
        let piece = match (&route, named) {
            (_, None) => piece,
            (None, Some(named)) => {
                route = Some(named);
                piece
            }
            (Some(route), Some(named)) if *route == named => piece,
            (Some(_), Some(_)) => Piece::Opaque,
        };
        match piece {
            Piece::Whole(mutation) => units.push(Unit {
                entries: vec![index],
                mutation: Some(mutation),
            }),
            Piece::Segment(segment) => {
                let (holders, segments) = transfers.entry((author, segment.digest)).or_default();
                holders.push(index);
                segments.push(segment);
            }
            Piece::Sealed | Piece::Opaque => units.push(Unit {
                entries: vec![index],
                mutation: None,
            }),
        }
    }
    for ((_, digest), (holders, segments)) in transfers {
        units.push(Unit {
            entries: holders,
            mutation: reassemble(digest, segments),
        });
    }
    Readable { units, route }
}

fn reassemble(
    digest: [u8; 32],
    mut segments: Vec<collab_types::DocumentSegment>,
) -> Option<DocumentMutation> {
    segments.sort_by_key(|segment| segment.index);
    segments.dedup_by_key(|segment| segment.index);
    let first = segments.first()?;
    let (kind, count) = (first.kind, first.count());
    if segments.len() != count as usize
        || segments
            .iter()
            .enumerate()
            .any(|(at, segment)| segment.index as usize != at || segment.kind != kind)
    {
        return None;
    }
    let data: Vec<u8> = segments
        .into_iter()
        .flat_map(|segment| segment.data)
        .collect();
    if collab_types::document_transfer::transfer_digest(kind, &data) != digest {
        return None;
    }
    Some(match kind {
        DocumentMutationKind::Snapshot => DocumentMutation::Snapshot(data),
        DocumentMutationKind::Incremental => DocumentMutation::Incremental(data),
    })
}

/// The hashes of the Automerge change chunks `bytes` concatenates, or `None`
/// when any part is not a valid change.
fn change_hashes(bytes: &[u8]) -> Option<Vec<ChangeHash>> {
    const HEADER: usize = 4 + 4 + 1;
    let mut hashes = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let mut len: u64 = 0;
        let mut shift = 0u32;
        let mut at = HEADER;
        loop {
            let byte = *rest.get(at)?;
            at = at.checked_add(1)?;
            len |= u64::from(byte & 0x7f).checked_shl(shift)?;
            if byte & 0x80 == 0 {
                break;
            }
            shift = shift.checked_add(7).filter(|shift| *shift < 64)?;
        }
        let end = at.checked_add(usize::try_from(len).ok()?)?;
        let chunk = rest.get(..end)?;
        hashes.push(automerge::Change::try_from(chunk).ok()?.hash());
        rest = rest.get(end..)?;
    }
    Some(hashes)
}

/// Whether every change of `mutation` is in `doc`: a snapshot's heads (it
/// holds all their ancestors), or each incremental change by hash.
fn covered_by(doc: &mut AutoCommit, mutation: &DocumentMutation) -> bool {
    let hashes = match mutation {
        DocumentMutation::Snapshot(bytes) => snapshot_heads(bytes)
            .map(|heads| heads.into_iter().map(ChangeHash).collect())
            .or_else(|| AutoCommit::load(bytes).ok().map(|mut doc| doc.get_heads())),
        DocumentMutation::Incremental(bytes) => change_hashes(bytes),
    };
    hashes.is_some_and(|hashes| {
        hashes
            .iter()
            .all(|hash| doc.get_change_by_hash(hash).is_some())
    })
}

fn unit_bytes(entries: &[ChannelEntry], unit: &Unit) -> usize {
    unit.entries
        .iter()
        .filter_map(|at| entries.get(*at))
        .map(|entry| entry.len)
        .sum()
}

/// Whether the readable part of a channel outgrew its snapshots, within
/// what the host folds: `min_tail` entries, or [`COMPACT_TAIL_BYTES`] that
/// are at least half the snapshots' size. Within that bound, `reseal`
/// folds it whatever its size.
fn compaction_due(
    entries: &[ChannelEntry],
    readable: &Readable,
    min_tail: usize,
    reseal: bool,
) -> bool {
    let units = &readable.units;
    let (mut snapshot, mut tail, mut tail_entries) = (0usize, 0usize, 0usize);
    for unit in units.iter().filter(|unit| unit.mutation.is_some()) {
        let bytes = unit_bytes(entries, unit);
        if matches!(unit.mutation, Some(DocumentMutation::Snapshot(_))) {
            snapshot = snapshot.saturating_add(bytes);
        } else {
            tail = tail.saturating_add(bytes);
            tail_entries = tail_entries.saturating_add(unit.entries.len());
        }
    }
    let readable_snapshots = units
        .iter()
        .filter(|unit| matches!(unit.mutation, Some(DocumentMutation::Snapshot(_))))
        .count();
    snapshot.saturating_add(tail) <= MAX_COMPACTED_BYTES
        && (reseal
            || tail_entries >= min_tail
            || (tail >= COMPACT_TAIL_BYTES && tail.saturating_mul(2) >= snapshot)
            || readable_snapshots > 2)
}

/// A sealed tail the host should ask a member to compact: the route, when
/// enough sealed entries arrived after the last member snapshot.
fn sealed_tail(checked: &[Checked<'_>], known: &HashSet<[u8; 32]>) -> Option<Route> {
    let (mut count, mut bytes, mut route) = (0usize, 0usize, None);
    for checked in checked {
        let entry = checked.entry();
        if let (Some(named), true) = (checked.route(), checked.is_sealed()) {
            if known.contains(&entry_id(&entry.body)) {
                continue;
            }
            count = count.saturating_add(1);
            bytes = bytes.saturating_add(entry.len);
            route.get_or_insert_with(|| named.clone());
        }
    }
    (count >= COMPACT_TAIL_ENTRIES || bytes >= COMPACT_TAIL_BYTES)
        .then_some(route)
        .flatten()
}

/// A channel folded into one snapshot.
pub(crate) struct Folded {
    route: Route,
    snapshot: Vec<u8>,
    /// File digests of the entries the snapshot contains entirely.
    removed: Vec<[u8; 32]>,
}

/// Fold every readable unit of `entries` into one document, when it
/// removes at least `minimum` entries. Units that depend on content the
/// host cannot read stay out of it, and only entries whose every unit the
/// document contains, by change hash, are removed.
fn fold(entries: &[ChannelEntry], readable: &Readable, minimum: usize) -> Option<Folded> {
    let units = &readable.units;
    let route = readable.route.clone()?;
    let mut snapshots: Vec<&Vec<u8>> = Vec::new();
    let mut changes: Vec<u8> = Vec::new();
    for unit in units {
        match &unit.mutation {
            Some(DocumentMutation::Snapshot(bytes)) => snapshots.push(bytes),
            Some(DocumentMutation::Incremental(bytes)) => changes.extend_from_slice(bytes),
            None => {}
        }
    }
    // Start from the largest snapshot: merging copies every change the
    // receiving side lacks, so the smaller documents go into the larger.
    snapshots.sort_by_key(|bytes| bytes.len());
    let mut doc = match snapshots.pop() {
        Some(bytes) => AutoCommit::load(bytes).ok()?,
        None => AutoCommit::new(),
    };
    for bytes in snapshots {
        // A snapshot the document already contains is not loaded at all:
        // holding two large documents at once is the costly part.
        if snapshot_heads(bytes).is_some_and(|heads| {
            heads
                .iter()
                .all(|head| doc.get_change_by_hash(&ChangeHash(*head)).is_some())
        }) {
            continue;
        }
        let mut other = AutoCommit::load(bytes).ok()?;
        doc.merge_incoming(&mut other).ok()?;
    }
    if !changes.is_empty() {
        doc.load_incoming(&changes).ok()?;
    }
    let all_applied = doc.get_missing_deps(&[]).is_empty();
    let folded_units: Vec<bool> = units
        .iter()
        .map(|unit| match &unit.mutation {
            None => false,
            Some(DocumentMutation::Snapshot(_)) => true,
            Some(mutation) => all_applied || covered_by(&mut doc, mutation),
        })
        .collect();
    let mut holders: HashMap<usize, bool> = HashMap::new();
    for (unit, folded) in units.iter().zip(&folded_units) {
        for entry in &unit.entries {
            let all = holders.entry(*entry).or_insert(true);
            *all &= *folded;
        }
    }
    let removed: Vec<[u8; 32]> = holders
        .into_iter()
        .filter(|(_, folded)| *folded)
        .filter_map(|(at, _)| entries.get(at).map(|entry| entry.digest))
        .collect();
    if removed.len() < minimum {
        return None;
    }
    Some(Folded {
        route,
        snapshot: doc.save(),
        removed,
    })
}

/// Sign `snapshot` for `route` as the host and frame it the way a device
/// publishes it (`payloads` frames each body): one retained envelope per
/// document frame, the first as the checkpoint. The publisher is the host's own handle, which a native shell
/// requires of every document it accepts.
pub(crate) fn document_frames(
    payloads: &dyn ShellPayloads,
    key: &ed25519_dalek::SigningKey,
    workspace_id: u64,
    route: &Route,
    snapshot: &[u8],
) -> Result<Vec<Vec<u8>>, String> {
    frames_of(payloads, (key, None), workspace_id, route, snapshot)
}

/// [`document_frames`] for a sealed channel this host stewards: each frame
/// sealed under the channel's current key with `keys`, as a member seals
/// the snapshot it compacts, so the folded history stays as sealed as the
/// writes it replaces.
fn sealed_document_frames(
    payloads: &dyn ShellPayloads,
    (key, keys): (
        &ed25519_dalek::SigningKey,
        &channel_crypto::ChannelContentKeys,
    ),
    workspace_id: u64,
    route: &Route,
    snapshot: &[u8],
) -> Result<Vec<Vec<u8>>, String> {
    frames_of(payloads, (key, Some(keys)), workspace_id, route, snapshot)
}

fn frames_of(
    payloads: &dyn ShellPayloads,
    (key, keys): (
        &ed25519_dalek::SigningKey,
        Option<&channel_crypto::ChannelContentKeys>,
    ),
    workspace_id: u64,
    route: &Route,
    snapshot: &[u8],
) -> Result<Vec<Vec<u8>>, String> {
    use ed25519_dalek::Signer as _;
    let author = key.verifying_key().to_bytes();
    let artifact =
        collab_types::document_artifact_id(&route.app_id, &route.cluster_id, &route.record_id);
    let attestation =
        collab_types::document_attestation_id(&route.app_id, &route.cluster_id, &route.record_id);
    let now_ns = tilo_clock::unix_now_ns();
    let timestamp = std::time::UNIX_EPOCH
        .checked_add(Duration::from_nanos(now_ns))
        .unwrap_or(std::time::UNIX_EPOCH);
    let channel =
        *blake3::hash(format!("app\0{}\0{}", route.app_id, route.record_id).as_bytes()).as_bytes();
    let mut frames = Vec::new();
    for (index, payload) in DocumentMutation::Snapshot(snapshot.to_vec())
        .encode_frames()
        .map_err(|error| error.to_string())?
        .into_iter()
        .enumerate()
    {
        let author_key = collab_types::Ed25519PublicKey::from_bytes(author);
        let seq = now_ns.saturating_add(index as u64);
        let mut op = match keys {
            None => collab_types::SignedOp::new_unsigned(
                payload,
                artifact,
                author_key,
                attestation,
                collab_types::OpKind::Change,
                timestamp,
                seq,
            ),
            Some(keys) => {
                let retention = if index == 0 {
                    collab_types::ContentRetention::Checkpoint
                } else {
                    collab_types::ContentRetention::Delta
                };
                let (metadata, sealed) = keys
                    .seal_content(artifact, retention, &payload)
                    .map_err(|error| error.to_string())?;
                collab_types::SignedOp::new_unsigned_with_encryption(
                    sealed,
                    artifact,
                    author_key,
                    attestation,
                    collab_types::OpKind::Change,
                    timestamp,
                    seq,
                    metadata,
                )
            }
        };
        let signing = op.signing_bytes().map_err(|error| error.to_string())?;
        op.signature = collab_types::Signature::from_bytes(key.sign(&signing).to_bytes());
        let body = payloads.document_body(&author, route, op)?;
        let payload = if index == 0 {
            CoeditPayload::Checkpoint { channel, body }
        } else {
            CoeditPayload::RetainedDelta { channel, body }
        };
        frames.push(
            crate::transport::encode(&Envelope::Delta {
                workspace_id,
                author_device_public_key: author,
                authored_at_ns: now_ns,
                delta: payload.encode().map_err(|error| error.to_string())?,
                signature: [0; 64],
            })
            .map_err(|error| error.to_string())?,
        );
    }
    Ok(frames)
}

#[cfg(test)]
mod tests;
