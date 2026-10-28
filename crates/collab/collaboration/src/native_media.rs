//! Bounded control lane on the same admitted channel, independent of document RPCs.
use super::{CollabEvent, InboundBridge};
use runtime_collab::native_media::{Command, PICTURES_HOLD, Request, Snapshot, WATCH_HOLD};
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

const MAX_PENDING: usize = 8;
const MAX_ROUTES: usize = 16;
const DEADLINE: Duration = Duration::from_secs(2);
/// A watch is held by the service up to [`WATCH_HOLD`]; past this it is lost.
const WATCH_DEADLINE: Duration = WATCH_HOLD.saturating_add(DEADLINE);
/// A pictures collection is held up to [`PICTURES_HOLD`]; past this it is lost.
const PICTURES_DEADLINE: Duration = PICTURES_HOLD.saturating_add(DEADLINE);
#[derive(Default)]
struct RouteFloor {
    intent: u64,
    completion: u64,
    barrier_inflight: Option<u64>,
    blocked: bool,
    failed: bool,
    must_leave: bool,
}
struct Job {
    route: String,
    request_id: u64,
    request: Request,
    completion: u64,
    priority: bool,
}
#[derive(Default)]
struct Queue {
    floors: BTreeMap<String, RouteFloor>,
    normal: VecDeque<Job>,
    stops: BTreeMap<String, Job>,
    pending: usize,
    closed: bool,
}
struct Shared {
    queue: Mutex<Queue>,
    wake: Notify,
    inbound: InboundBridge,
}
pub(super) struct Sender(Arc<Shared>);
pub(super) struct Receiver(Arc<Shared>);
#[derive(Clone, Default)]
pub(super) struct Routes(Rc<RefCell<BTreeMap<String, runtime_collab::ChannelHandle>>>);
impl Routes {
    pub(super) fn insert(&self, key: String, channel: runtime_collab::ChannelHandle) {
        let mut routes = self.0.borrow_mut();
        if routes.len() < MAX_ROUTES || routes.contains_key(&key) {
            routes.insert(key, channel);
        }
    }
}
pub(super) fn channel(inbound: InboundBridge) -> (Sender, Receiver) {
    let shared = Arc::new(Shared {
        queue: Mutex::new(Queue::default()),
        wake: Notify::new(),
        inbound,
    });
    (Sender(shared.clone()), Receiver(shared))
}
fn reply(shared: &Shared, id: u64, result: Result<Snapshot, String>) {
    if shared.queue.lock().map_or(true, |queue| queue.closed) {
        return;
    }
    shared
        .inbound
        .send_reply(CollabEvent::NativeMediaCommandResult {
            request_id: id,
            result,
        });
}
impl Sender {
    /// Sequence is assigned here, before transport scheduling can reorder arrivals.
    pub(super) fn send(&self, route: String, request_id: u64, mut request: Request) -> bool {
        if !request.valid() || route.len() > 1024 {
            return false;
        }
        let priority = matches!(
            request.command,
            Command::Leave
                | Command::SetMuted { muted: true }
                | Command::SetDeafened { deafened: true }
        );
        let effect = request.carries_intent();
        let Ok(mut queue) = self.0.queue.lock() else {
            return false;
        };
        if queue.closed || (!queue.floors.contains_key(&route) && queue.floors.len() >= MAX_ROUTES)
        {
            return false;
        }
        if !priority && queue.pending >= MAX_PENDING {
            return false;
        }
        let floor = queue.floors.entry(route.clone()).or_default();
        if effect && !priority && floor.failed {
            return false;
        }
        // Exhaustion permanently disables new effects on this retained route;
        // only the terminal Leave sentinel may cross the reserved stop lane.
        if effect {
            if let Some(next) = floor
                .intent
                .checked_add(1)
                .filter(|value| *value < u64::MAX)
            {
                request.intent = next;
            } else {
                request.command = Command::Leave;
                request.intent = u64::MAX;
            }
            floor.intent = request.intent;
        } else {
            request.intent = 0;
        }
        if effect {
            floor.completion = floor.intent;
        }
        let completion = floor.completion;
        let priority = priority || request.intent == u64::MAX;
        if priority {
            floor.blocked = true;
            floor.must_leave |= matches!(request.command, Command::Leave);
        }
        let job = Job {
            route: route.clone(),
            request_id,
            request,
            completion,
            priority,
        };
        let superseded = if priority {
            queue.stops.insert(route, job)
        } else {
            queue.pending += 1;
            queue.normal.push_back(job);
            None
        };
        drop(queue);
        if let Some(old) = superseded {
            reply(
                &self.0,
                old.request_id,
                Err("native media command superseded".into()),
            );
        }
        self.0.wake.notify_one();
        true
    }
}
impl Drop for Sender {
    fn drop(&mut self) {
        if let Ok(mut queue) = self.0.queue.lock() {
            queue.closed = true;
            queue.normal.clear();
            queue.stops.clear();
            let routes = queue.floors.keys().cloned().collect::<Vec<_>>();
            for route in routes {
                if let Some(floor) = queue.floors.get_mut(&route) {
                    floor.intent = u64::MAX;
                    floor.completion = u64::MAX;
                    floor.barrier_inflight = None;
                    floor.blocked = true;
                    floor.must_leave = true;
                }
                queue.stops.insert(
                    route.clone(),
                    Job {
                        route,
                        request_id: 0,
                        request: Request {
                            version: 1,
                            intent: u64::MAX,
                            command: Command::Leave,
                        },
                        completion: u64::MAX,
                        priority: true,
                    },
                );
            }
        }
        self.0.wake.notify_one();
    }
}
pub(super) struct Worker(tokio::task::JoinHandle<()>);
impl Drop for Worker {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct Tasks(BTreeMap<(String, u8), tokio::task::JoinHandle<()>>);
impl Drop for Tasks {
    fn drop(&mut self) {
        for task in self.0.values() {
            task.abort();
        }
    }
}
struct Pending {
    shared: Arc<Shared>,
    normal: bool,
}
impl Drop for Pending {
    fn drop(&mut self) {
        if self.normal {
            if let Ok(mut queue) = self.shared.queue.lock() {
                queue.pending = queue.pending.saturating_sub(1);
            }
        }
    }
}
struct Completion {
    shared: Arc<Shared>,
    request_id: Option<u64>,
}
impl Completion {
    fn finish(mut self, result: Result<Snapshot, String>) {
        if let Some(id) = self.request_id.take() {
            reply(&self.shared, id, result);
        }
    }
}
impl Drop for Completion {
    fn drop(&mut self) {
        if let Some(id) = self.request_id.take() {
            reply(
                &self.shared,
                id,
                Err("native media command superseded".into()),
            );
        }
    }
}
/// A stop is an effect obligation, independent of which reply is newest.
/// Cancellation/timeout keeps this route blocked until a later stop succeeds.
struct BarrierCompletion {
    shared: Arc<Shared>,
    route: String,
    intent: u64,
    leave: bool,
    finished: bool,
}
impl BarrierCompletion {
    fn finish(&mut self, succeeded: bool) {
        if self.finished {
            return;
        }
        self.finished = true;
        let retired = {
            let Ok(mut queue) = self.shared.queue.lock() else {
                return;
            };
            let pending_stop = queue.stops.contains_key(&self.route);
            let Some(floor) = queue.floors.get_mut(&self.route) else {
                return;
            };
            if floor.barrier_inflight != Some(self.intent) {
                return;
            }
            floor.barrier_inflight = None;
            floor.failed = !succeeded;
            if succeeded && self.leave {
                floor.must_leave = false;
            }
            floor.blocked = !succeeded || pending_stop;
            let mut retired = Vec::new();
            if !succeeded {
                queue.normal.retain(|job| {
                    if job.route == self.route && job.request.intent != 0 {
                        retired.push(job.request_id);
                        false
                    } else {
                        true
                    }
                });
                queue.pending = queue.pending.saturating_sub(retired.len());
            }
            retired
        };
        for id in retired {
            reply(
                &self.shared,
                id,
                Err("native media stop outcome is uncertain".into()),
            );
        }
        self.shared.wake.notify_one();
    }
}
impl Drop for BarrierCompletion {
    fn drop(&mut self) {
        self.finish(false);
    }
}

pub(super) fn start(receiver: Receiver, routes: Routes) -> Worker {
    Worker(tokio::task::spawn_local(async move {
        let shared = receiver.0;
        let mut tasks = Tasks(BTreeMap::new());
        let mut closing = false;
        loop {
            shared.wake.notified().await;
            if shared.queue.lock().map_or(true, |queue| queue.closed) {
                closing = true;
                for task in tasks.0.values() {
                    task.abort();
                }
                tasks.0.clear();
            }
            loop {
                let job = {
                    let Ok(mut queue) = shared.queue.lock() else {
                        return;
                    };
                    let stop = queue
                        .stops
                        .keys()
                        .find(|key| {
                            queue
                                .floors
                                .get(*key)
                                .is_some_and(|floor| floor.barrier_inflight.is_none())
                        })
                        .cloned();
                    if let Some(key) = stop {
                        queue.stops.remove(&key).and_then(|mut job| {
                            let floor = queue.floors.get_mut(&key)?;
                            if floor.must_leave {
                                job.request.command = Command::Leave;
                            }
                            floor.barrier_inflight = Some(job.request.intent);
                            Some(job)
                        })
                    } else {
                        let eligible = queue.normal.iter().position(|job| {
                            job.request.intent == 0
                                || queue
                                    .floors
                                    .get(&job.route)
                                    .is_some_and(|floor| !floor.blocked)
                        });
                        eligible.and_then(|index| queue.normal.remove(index))
                    }
                };
                let Some(job) = job else {
                    break;
                };
                let pending = Pending {
                    shared: shared.clone(),
                    normal: !job.priority,
                };
                let mut barrier = job.priority.then(|| BarrierCompletion {
                    shared: shared.clone(),
                    route: job.route.clone(),
                    intent: job.request.intent,
                    leave: matches!(job.request.command, Command::Leave),
                    finished: false,
                });
                let current = shared
                    .queue
                    .lock()
                    .ok()
                    .and_then(|q| q.floors.get(&job.route).map(|f| f.completion))
                    == Some(job.completion);
                if !current && !job.priority {
                    reply(
                        &shared,
                        job.request_id,
                        Err("native media command superseded".into()),
                    );
                    continue;
                }
                // A held watch has its own lane, and so has collecting
                // pictures: a status read or an effect never cancels either,
                // and a newer one of the same kind replaces it.
                let watch = matches!(job.request.command, Command::Watch { .. });
                let pictures = matches!(job.request.command, Command::Pictures { .. });
                let shows = matches!(job.request.command, Command::Show { .. });
                let task_key = (
                    job.route.clone(),
                    if job.priority {
                        2
                    } else if watch {
                        3
                    } else if pictures {
                        4
                    } else if shows {
                        5
                    } else {
                        u8::from(job.request.intent != 0)
                    },
                );
                let deadline = if watch {
                    WATCH_DEADLINE
                } else if pictures {
                    PICTURES_DEADLINE
                } else {
                    DEADLINE
                };
                if job.priority
                    && let Some(old) = tasks.0.remove(&(job.route.clone(), 1))
                {
                    old.abort();
                }
                if let Some(old) = tasks.0.remove(&task_key) {
                    old.abort();
                }
                let Some(channel) = routes.0.borrow().get(&job.route).cloned() else {
                    reply(
                        &shared,
                        job.request_id,
                        Err("native media route is not admitted".into()),
                    );
                    continue;
                };
                let task_shared = shared.clone();
                let completion = Completion {
                    shared: shared.clone(),
                    request_id: (!closing).then_some(job.request_id),
                };

                let task = tokio::task::spawn_local(async move {
                    let _pending = pending;
                    let result =
                        tokio::time::timeout(deadline, channel.native_media_command(&job.request))
                            .await
                            .map_err(|_| "native media command timed out".to_string())
                            .and_then(|result| {
                                result.map_err(|_| "native media command refused".to_string())
                            });
                    if let Some(barrier) = barrier.as_mut() {
                        barrier.finish(result.is_ok());
                    }
                    let current = task_shared
                        .queue
                        .lock()
                        .ok()
                        .and_then(|q| q.floors.get(&job.route).map(|f| f.completion))
                        == Some(job.completion);
                    completion.finish(if current {
                        result
                    } else {
                        Err("native media command superseded".into())
                    });
                });
                tasks.0.insert(task_key, task);
            }
            if closing {
                while let Some((_, task)) = tasks.0.pop_first() {
                    let _ = task.await;
                }
                return;
            }
        }
    }))
}
