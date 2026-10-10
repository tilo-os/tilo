//! A session's own view of the relay: every broadcast a scoped source origin
//! announces, re-published in an origin the host owns for that one session.
//!
//! The session is served only from this view, never from the shared origin
//! directly. Dropping the [`Mirror`] aborts every track it serves, so the
//! session's subscriptions end at once: revocation cuts media to a session
//! before the transport is torn down, whatever the client does.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::task::Poll;
use std::time::{Duration, Instant};

use crate::Task;
use crate::carrier::{OriginTask, origin_config};
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

enum Next {
    Datagram(moq_net::Datagram),
    Group(Box<moq_net::group::Consumer>),
    End,
    Abort(moq_net::Error),
    Unused,
}

async fn next(track: &mut moq_net::track::Subscriber) -> Next {
    moq_net::kio::wait(|waiter| {
        let datagram = track.poll_recv_datagram(waiter);
        match datagram {
            Poll::Ready(Ok(Some(datagram))) => return Poll::Ready(Next::Datagram(datagram)),
            Poll::Ready(Err(error)) => return Poll::Ready(Next::Abort(error)),
            _ => {}
        }
        let group = track.poll_recv_group(waiter);
        match group {
            Poll::Ready(Ok(Some(group))) => return Poll::Ready(Next::Group(Box::new(group))),
            Poll::Ready(Err(error)) => return Poll::Ready(Next::Abort(error)),
            _ => {}
        }
        if matches!(datagram, Poll::Ready(Ok(None))) && matches!(group, Poll::Ready(Ok(None))) {
            return Poll::Ready(Next::End);
        }
        Poll::Pending
    })
    .await
}

/// SUBSCRIBE can exchange consumer handles. Match the shared relay's existing
/// unused-demand grace; the caller then closes atomically with demand.
async fn unused(output: &moq_net::track::Producer) {
    loop {
        if output.unused().await.is_err() {
            return;
        }
        tokio::time::sleep(crate::live::UNUSED_GRACE).await;
        if !output.is_used() {
            return;
        }
    }
}

async fn copy(
    input: &mut moq_net::track::Subscriber,
    mut output: moq_net::track::Producer,
    downlink: Option<Arc<Downlink>>,
) {
    let mut groups: VecDeque<Task> = VecDeque::new();
    let mut rebundler = voice::Rebundler::default();
    let watched = output.clone();
    let idle = unused(&watched);
    tokio::pin!(idle);
    loop {
        let next = tokio::select! {
            next = next(input) => next,
            () = &mut idle => Next::Unused,
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
            Next::Abort(error) => {
                let _ = output.abort(error);
                return;
            }
            Next::Unused => match output.abort_unused(moq_net::Error::Cancel) {
                Ok(()) => return,
                Err(active) => {
                    output = active;
                    idle.set(unused(&watched));
                }
            },
        }
    }
    // A finished upstream track can still carry cached groups whose copying
    // tasks have not run. Await them before publishing a clean final boundary.
    while let Some(mut group) = groups.pop_front() {
        loop {
            tokio::select! {
                result = group.wait() => {
                    if result.is_err() {
                        let _ = output.abort(moq_net::Error::Cancel);
                        return;
                    }
                    break;
                }
                () = &mut idle => match output.abort_unused(moq_net::Error::Cancel) {
                    Ok(()) => return,
                    Err(active) => {
                        output = active;
                        idle.set(unused(&watched));
                    }
                },
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn unused_video_cancels_even_while_source_groups_arrive() {
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let (origin, _origin_task) =
                OriginTask::produce(origin_config(1024 * 1024, Duration::from_secs(2)));
            let broadcast = origin.create_broadcast("unused")?;
            let input = broadcast.create_track("video", None)?;
            let mut reader = input.consume().subscribe(None).await?;
            let output = broadcast.create_track("copy", None)?;
            let access = output.clone();
            let copying = copy(&mut reader, output, None);
            tokio::pin!(copying);
            let pending = std::future::poll_fn(|cx| {
                Poll::Ready(std::future::Future::poll(copying.as_mut(), cx).is_pending())
            })
            .await;
            assert!(pending, "the existing unused grace must cover handoff");
            tokio::time::advance(crate::live::UNUSED_GRACE / 2).await;
            let mut group = input.append_group()?;
            group.write_frame(
                moq_net::Timestamp::ZERO,
                bytes::Bytes::from_static(b"frame"),
            )?;
            group.finish()?;
            let pending = std::future::poll_fn(|cx| {
                Poll::Ready(std::future::Future::poll(copying.as_mut(), cx).is_pending())
            })
            .await;
            assert!(pending);
            tokio::time::advance(crate::live::UNUSED_GRACE / 2).await;
            let ended = std::future::poll_fn(|cx| {
                Poll::Ready(std::future::Future::poll(copying.as_mut(), cx).is_ready())
            })
            .await;
            assert!(ended, "source traffic must not renew unused demand");
            assert!(matches!(access.closed().await, moq_net::Error::Cancel));
            Ok(())
        }
        .await;
        assert!(result.is_ok(), "{result:?}");
    }

    #[tokio::test]
    async fn upstream_abort_is_not_a_clean_empty_video() {
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let (origin, _origin_task) =
                OriginTask::produce(origin_config(1024 * 1024, Duration::from_secs(2)));
            let broadcast = origin.create_broadcast("abort")?;
            let input = broadcast.create_track("video", None)?;
            let mut reader = input.consume().subscribe(None).await?;
            let output = broadcast.create_track("copy", None)?;
            let mut viewer = output.consume().subscribe(None).await?;
            input.abort(moq_net::Error::ProtocolViolation)?;
            let task = tokio::spawn(async move { copy(&mut reader, output, None).await });
            let result =
                tilo_test_env::within("the mirrored upstream failure", viewer.recv_group()).await;
            assert!(
                matches!(result, Err(moq_net::Error::ProtocolViolation)),
                "the original upstream error must remain observable"
            );
            task.await?;
            Ok(())
        }
        .await;
        assert!(result.is_ok(), "{result:?}");
    }

    #[tokio::test]
    async fn subscriber_handoff_does_not_end_an_open_video() {
        let result: Result<(), Box<dyn std::error::Error>> = async {
        let (origin, _origin_task) =
            OriginTask::produce(origin_config(1024 * 1024, Duration::from_secs(2)));
        let broadcast = origin.create_broadcast("handoff")?;
        let input = broadcast.create_track("video", None)?;
        let mut reader = input.consume().subscribe(None).await?;
        let output = broadcast.create_track("copy", None)?;
        let access = output.clone();
        // SUBSCRIBE exchanges consumer handles; the accepted producer is alive
        // while the replacement subscriber has not yet attached.
        let previous = output.consume();
        drop(previous);
        let copying = copy(&mut reader, output, None);
        tokio::pin!(copying);
        let pending = std::future::poll_fn(|cx| {
            Poll::Ready(std::future::Future::poll(copying.as_mut(), cx).is_pending())
        })
        .await;
        assert!(
            pending,
            "an open video must survive the subscription handle exchange"
        );
        let mut viewer = access.consume().subscribe(None).await?;
        let mut group = input.append_group()?;
        group.write_frame(
            moq_net::Timestamp::ZERO,
            bytes::Bytes::from_static(b"keyframe"),
        )?;
        for frame in [b"reference-1".as_slice(), b"reference-2".as_slice()] {
            group.write_frame(moq_net::Timestamp::ZERO, bytes::Bytes::from_static(frame))?;
        }
        group.finish()?;
        let receive = async {
            let mut group = viewer.recv_group().await?.ok_or(moq_net::Error::Closed)?;
            let frame = group.read_frame().await?.ok_or(moq_net::Error::Closed)?;
            assert_eq!(frame.payload, bytes::Bytes::from_static(b"keyframe"));
            for expected in [b"reference-1".as_slice(), b"reference-2".as_slice()] {
                let frame = group.read_frame().await?.ok_or(moq_net::Error::Closed)?;
                assert_eq!(frame.payload, bytes::Bytes::from_static(expected));
            }
            Ok::<(), moq_net::Error>(())
        };
        tokio::select! {
            () = &mut copying => return Err("mirror ended before the replacement subscriber read".into()),
            result = tilo_test_env::within("the replacement subscriber keyframe", receive) => result?,
        }
        Ok(())
        }.await;
        assert!(result.is_ok(), "{result:?}");
    }

    #[tokio::test]
    async fn completed_cached_group_reaches_mirrored_track() {
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let (origin, _origin_task) =
                OriginTask::produce(origin_config(1024 * 1024, Duration::from_secs(2)));
            let broadcast = origin.create_broadcast("cached")?;
            let input = broadcast.create_track("video", None)?;
            let mut group = input.append_group()?;
            group.write_frame(
                moq_net::Timestamp::ZERO,
                bytes::Bytes::from_static(b"keyframe"),
            )?;
            for frame in [b"reference-1".as_slice(), b"reference-2".as_slice()] {
                group.write_frame(moq_net::Timestamp::ZERO, bytes::Bytes::from_static(frame))?;
            }
            group.finish()?;
            input.finish()?;
            let mut reader = input.consume().subscribe(None).await?;
            let output = broadcast.create_track("copy", None)?;
            let mut viewer = output.consume().subscribe(None).await?;
            let task = tokio::spawn(async move { copy(&mut reader, output, None).await });
            let group =
                tilo_test_env::within("the cached mirrored group", viewer.recv_group()).await?;
            let Some(mut group) = group else {
                return Err("mirror ended before the cached group".into());
            };
            let frame = tilo_test_env::within("the cached mirrored frame", group.read_frame())
                .await?
                .ok_or("mirror ended before the cached keyframe")?;
            assert_eq!(frame.payload, bytes::Bytes::from_static(b"keyframe"));
            for expected in [b"reference-1".as_slice(), b"reference-2".as_slice()] {
                let frame = tilo_test_env::within("a cached reference frame", group.read_frame())
                    .await?
                    .ok_or("mirror ended before a cached reference frame")?;
                assert_eq!(frame.payload, bytes::Bytes::from_static(expected));
            }
            assert!(group.read_frame().await?.is_none());
            task.await?;
            Ok(())
        }
        .await;
        assert!(result.is_ok(), "{result:?}");
    }
}
