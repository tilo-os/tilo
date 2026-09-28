//! A session's own view of the relay: every broadcast a scoped source origin
//! announces, re-published in an origin the host owns for that one session.
//!
//! The session is served only from this view, never from the shared origin
//! directly. Dropping the [`Mirror`] aborts every track it serves, so the
//! session's subscriptions end at once: revocation cuts media to a session
//! before the transport is torn down, whatever the client does.

use std::collections::{HashMap, VecDeque};
use std::task::Poll;
use std::time::Duration;

use crate::Task;
use crate::carrier::{OriginTask, origin_config};

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
    /// and retention for the session's copy.
    pub(crate) fn start(source: moq_net::origin::Consumer, bytes: u64, retain: Duration) -> Self {
        let (origin, task) = OriginTask::produce(origin_config(bytes, retain));
        let watch = Task::spawn(watch(source, origin.clone()));
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

async fn watch(source: moq_net::origin::Consumer, target: moq_net::origin::Producer) {
    let mut announced = source.announced();
    let mut broadcasts: HashMap<String, Task> = HashMap::new();
    while let Some(update) = announced.next().await {
        let path = update.prefix.as_str().to_owned();
        broadcasts.retain(|_, task| !task.is_finished());
        if update.kind.is_active() {
            broadcasts
                .entry(path.clone())
                .or_insert_with(|| Task::spawn(broadcast(source.clone(), target.clone(), path)));
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
                tracks.push(Task::spawn(track(input.clone(), request)));
            }
            _ = input.closed() => break,
        }
    }
    drop(tracks);
    output.finish();
}

async fn track(input: moq_net::broadcast::Consumer, request: moq_net::track::Request) {
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
    copy(&mut reader, output).await;
}

enum Next {
    Datagram(moq_net::Datagram),
    Group(Box<moq_net::group::Consumer>),
    End,
}

async fn next(track: &mut moq_net::track::Subscriber) -> Next {
    moq_net::kio::wait(|waiter| {
        if let Poll::Ready(datagram) = track.poll_recv_datagram(waiter) {
            return Poll::Ready(match datagram {
                Ok(Some(datagram)) => Next::Datagram(datagram),
                _ => Next::End,
            });
        }
        if let Poll::Ready(group) = track.poll_recv_group(waiter) {
            return Poll::Ready(match group {
                Ok(Some(group)) => Next::Group(Box::new(group)),
                _ => Next::End,
            });
        }
        Poll::Pending
    })
    .await
}

async fn copy(input: &mut moq_net::track::Subscriber, mut output: moq_net::track::Producer) {
    let mut groups: VecDeque<Task> = VecDeque::new();
    loop {
        let next = tokio::select! {
            next = next(input) => next,
            _ = output.unused() => Next::End,
        };
        match next {
            Next::Datagram(datagram) => {
                let _ =
                    output.insert_datagram(datagram.sequence, datagram.timestamp, datagram.payload);
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
