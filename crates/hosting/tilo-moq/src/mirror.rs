//! A session's own view of the relay: every broadcast a scoped source origin
//! announces, re-published in an origin the host owns for that one session.
//!
//! The session is served only from this view, never from the shared origin
//! directly. Dropping the [`Mirror`] aborts every track it serves, so the
//! session's subscriptions end at once: revocation cuts media to a session
//! before the transport is torn down, whatever the client does.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use crate::Task;
use crate::carrier::{OriginTask, origin_config};
use crate::relay::{Next, next};
use crate::voice;

/// The voice relay's downlink to one session: every voice datagram stream
/// the session is served is re-bundled ([`voice::Rebundler`]) with the
/// redundancy that the loss on this session's own QUIC connection calls for.
#[derive(Default)]
pub(crate) struct Downlink {
    connection: OnceLock<quinn::Connection>,
    state: Mutex<DownlinkState>,
}

#[derive(Default)]
struct DownlinkState {
    redundancy: voice::Redundancy,
    sampled: Option<Instant>,
}

impl Downlink {
    /// Measure loss on `connection` from now on.
    pub(crate) fn attach(&self, connection: quinn::Connection) {
        let _ = self.connection.set(connection);
    }

    /// Earlier objects to add to each bundle, sampling the connection's loss
    /// counters at most once per [`voice::LOSS_WINDOW`].
    pub(crate) fn level(&self, now: Instant) -> usize {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let due = state
            .sampled
            .is_none_or(|at| now.saturating_duration_since(at) >= voice::LOSS_WINDOW);
        if let (true, Some(connection)) = (due, self.connection.get()) {
            state.sampled = Some(now);
            let path = connection.stats().path;
            return state
                .redundancy
                .observe(now, path.sent_packets, path.lost_packets);
        }
        state.redundancy.level()
    }
}

/// Groups copied concurrently per mirrored track; a newer group evicts the
/// oldest instead of queueing behind it.
const MAX_GROUPS_IN_FLIGHT: usize = 8;

/// A session-owned copy of `source`. Drop it to cut the session off.
pub(crate) struct Mirror {
    origin: moq_net::origin::Producer,
    _watch: Task,
    _task: OriginTask,
}

impl Mirror {
    /// Mirror every broadcast `source` announces, with the given cache bound
    /// and retention for the session's copy. With a `downlink`, datagrams of
    /// the voice track are re-bundled for that downlink's measured loss.
    pub(crate) fn start(
        source: moq_net::origin::Consumer,
        bytes: u64,
        retain: Duration,
        downlink: Option<Arc<Downlink>>,
    ) -> Self {
        let (origin, task) = OriginTask::produce(origin_config(bytes, retain));
        let watch = Task::spawn(watch(source, origin.clone(), downlink));
        Self {
            origin,
            _watch: watch,
            _task: task,
        }
    }

    /// The view to serve the session from.
    pub(crate) fn consume(&self) -> moq_net::origin::Consumer {
        self.origin.consume()
    }
}

async fn watch(
    source: moq_net::origin::Consumer,
    target: moq_net::origin::Producer,
    downlink: Option<Arc<Downlink>>,
) {
    let mut announced = source.announced();
    let mut broadcasts: HashMap<String, Task> = HashMap::new();
    while let Some(update) = announced.next().await {
        let path = update.prefix.as_str().to_owned();
        broadcasts.retain(|_, task| !task.is_finished());
        if update.kind.is_active() {
            broadcasts.entry(path.clone()).or_insert_with(|| {
                Task::spawn(broadcast(
                    source.clone(),
                    target.clone(),
                    path,
                    downlink.clone(),
                ))
            });
        } else {
            broadcasts.remove(&path);
        }
    }
}

/// Mirror one broadcast for as long as the source serves it. Tracks are
/// copied on the session's demand.
async fn broadcast(
    source: moq_net::origin::Consumer,
    target: moq_net::origin::Producer,
    path: String,
    downlink: Option<Arc<Downlink>>,
) {
    let Ok(input) = source.routed_broadcast(path.as_str()).await else {
        return;
    };
    let Ok(output) = target.create_broadcast(path.as_str()) else {
        return;
    };
    let mut dynamic = output.dynamic();
    if output.announce(moq_net::origin::Route::default()).is_err() {
        return;
    }
    let mut tracks: Vec<Task> = Vec::new();
    loop {
        tokio::select! {
            request = dynamic.requested_track() => {
                let Ok(request) = request else { break };
                tracks.retain(|task| !task.is_finished());
                // Only the voice track is re-bundled; any other is copied as is.
                let voice = downlink
                    .clone()
                    .filter(|_| request.name() == crate::relay::VOICE_TRACK);
                tracks.push(Task::spawn(track(input.clone(), request, voice)));
            }
            _ = input.closed() => break,
        }
    }
    drop(tracks);
    output.finish();
}

async fn track(
    input: moq_net::broadcast::Consumer,
    request: moq_net::track::Request,
    downlink: Option<Arc<Downlink>>,
) {
    let subscription = request.subscription().unwrap_or_default();
    let attempt = match input.track(request.name()) {
        Ok(track) => track.subscribe(subscription).await,
        Err(error) => Err(error),
    };
    let mut reader = match attempt {
        Ok(reader) => reader,
        Err(error) => {
            request.reject(error);
            return;
        }
    };
    let output = request.accept(reader.info().clone());
    copy(&mut reader, output, downlink).await;
}

async fn copy(
    input: &mut moq_net::track::Subscriber,
    mut output: moq_net::track::Producer,
    downlink: Option<Arc<Downlink>>,
) {
    let mut groups: VecDeque<Task> = VecDeque::new();
    let mut rebundler = voice::Rebundler::default();
    loop {
        let next = tokio::select! {
            next = next(input) => next,
            _ = output.unused() => Next::End,
        };
        match next {
            Next::Datagram(datagram) => {
                let payload = match &downlink {
                    // The relay validated the bundle on its way in; one that
                    // carries nothing new for this session is not sent.
                    Some(downlink) => {
                        let level = downlink.level(Instant::now());
                        match rebundler.next(&datagram.payload, level) {
                            Ok(Some(bundle)) => bytes::Bytes::from(bundle),
                            Ok(None) | Err(_) => continue,
                        }
                    }
                    None => datagram.payload,
                };
                let _ = output.insert_datagram(datagram.sequence, datagram.timestamp, payload);
            }
            Next::Group(group) => {
                groups.retain(|task| !task.is_finished());
                if groups.len() >= MAX_GROUPS_IN_FLIGHT {
                    groups.pop_front();
                }
                if let Ok(out) = output.create_group(moq_net::group::Info::from(group.sequence)) {
                    groups.push_back(Task::spawn(copy_group(*group, out)));
                }
            }
            Next::End => break,
        }
    }
    drop(groups);
    let _ = output.finish();
}

async fn copy_group(mut input: moq_net::group::Consumer, mut output: moq_net::group::Producer) {
    loop {
        match input.read_frame().await {
            Ok(Some(frame)) => {
                if output.write_frame(frame.timestamp, frame.payload).is_err() {
                    return;
                }
            }
            Ok(None) => {
                let _ = output.finish();
                return;
            }
            Err(error) => {
                let _ = output.abort(error);
                return;
            }
        }
    }
}
