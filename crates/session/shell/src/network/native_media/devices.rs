//! One independently refreshed observation table. No device enumeration runs
//! while the shell holds a capture lease or service installation lock, and
//! none at all while no call holds a [`Watch`] on the table.
use super::{Arc, AtomicBool, AtomicU64, Duration, Instant, Mutex, Ordering, observer};
use crate::portal::{CaptureBroker, CaptureKind, capture_broker::LocalCaptureHandle};
use media::AudioGraph;
use std::collections::BTreeMap;
use std::sync::atomic::AtomicUsize;
use tokio::sync::Notify;

const FRESH: Duration = Duration::from_secs(2);
pub(super) struct Observation {
    pub node: u32,
    pub serial: u32,
    pub cookie: u32,
    base: Instant,
    deadline: AtomicU64,
    alive: AtomicBool,
}
impl Observation {
    fn new(node: u32, serial: u32, cookie: u32, base: Instant) -> Self {
        Self {
            node,
            serial,
            cookie,
            base,
            deadline: AtomicU64::new(0),
            alive: AtomicBool::new(true),
        }
    }
    fn refresh(&self) {
        self.deadline.store(
            u64::try_from(
                self.base
                    .elapsed()
                    .as_nanos()
                    .saturating_add(FRESH.as_nanos()),
            )
            .unwrap_or(u64::MAX),
            Ordering::Release,
        );
    }
    pub fn current(&self) -> bool {
        let alive = self.alive.load(Ordering::Acquire);
        let fresh =
            self.base.elapsed().as_nanos() < u128::from(self.deadline.load(Ordering::Acquire));
        if !alive || !fresh {
            tracing::debug!(alive, fresh, "native audio observation fence closed");
        }
        alive && fresh
    }
}
#[derive(Default)]
struct Graph {
    sequence: u64,
    sources: BTreeMap<String, Arc<Observation>>,
    output: Option<Arc<Observation>>,
}
pub(super) struct Devices {
    broker: Arc<std::sync::Mutex<CaptureBroker>>,
    graph: Mutex<Graph>,
    base: Instant,
    stopped: AtomicBool,
    next: AtomicU64,
    task: Mutex<Option<tokio::task::AbortHandle>>,
    /// Live [`Watch`]es. The refresh loop runs only while there is one.
    watchers: AtomicUsize,
    /// Wakes the parked refresh loop when the first [`Watch`] is taken. Held
    /// apart from `Devices` so the parked loop does not keep it alive.
    watched: Arc<Notify>,
}

/// Keeps the observation table fresh while held: a call holds one from the
/// moment it starts preparing until it ends.
///
/// An observation is current for [`FRESH`] after it was last seen, and only a
/// call reads one (its output, its microphone, the inputs it offers). With no
/// call there is nothing to keep fresh, so the refresh loop, a blocking
/// `PipeWire` registry round trip each second, parks instead of waking the
/// network runtime for nothing.
pub(super) struct Watch(Arc<Devices>);
impl Drop for Watch {
    fn drop(&mut self) {
        self.0.watchers.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Devices {
    pub fn start(
        broker: Arc<std::sync::Mutex<CaptureBroker>>,
        runtime: &tokio::runtime::Handle,
    ) -> Arc<Self> {
        let watched = Arc::new(Notify::new());
        let value = Arc::new(Self {
            broker,
            graph: Mutex::new(Graph::default()),
            base: Instant::now(),
            stopped: AtomicBool::new(false),
            next: AtomicU64::new(0),
            task: Mutex::new(None),
            watchers: AtomicUsize::new(0),
            watched: Arc::clone(&watched),
        });
        let weak = Arc::downgrade(&value);
        let task = runtime.spawn(async move {
            loop {
                let Some(owner) = weak.upgrade() else {
                    return;
                };
                if owner.watchers.load(Ordering::Acquire) == 0 {
                    drop(owner);
                    // `notify_one` leaves a permit when nothing waits yet, so
                    // a watch taken since the check above is not missed.
                    watched.notified().await;
                    continue;
                }
                let Ok(sequence) = owner.sequence() else {
                    return;
                };
                drop(owner);
                let mut read = tokio::task::spawn_blocking(observer::observe);
                let observed = match tokio::time::timeout(FRESH, &mut read).await {
                    Ok(Ok(Ok(observed))) => Some(observed),
                    Ok(Ok(Err(error))) => {
                        tracing::debug!(%error, "native audio registry refresh failed");
                        None
                    }
                    _ => {
                        tracing::debug!("native audio registry refresh worker missed its deadline");
                        None
                    }
                };
                let Some(owner) = weak.upgrade() else {
                    return;
                };
                if owner.stopped.load(Ordering::Acquire) {
                    return;
                }
                owner.update(
                    sequence,
                    observed.as_ref().map(|value| (value.cookie, &value.graph)),
                );
                drop(owner);
                // At most one independent registry worker per refresh loop.
                // Late observations cannot overwrite a newer publication.
                if observed.is_none() && !read.is_finished() {
                    let _ = read.await;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
        *value.task.lock() = Some(task.abort_handle());
        value
    }
    /// Keep the table fresh until the returned [`Watch`] drops.
    pub fn watch(self: &Arc<Self>) -> Watch {
        self.watchers.fetch_add(1, Ordering::AcqRel);
        self.watched.notify_one();
        Watch(Arc::clone(self))
    }
    fn sequence(&self) -> Result<u64, String> {
        self.next
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |next| {
                next.checked_add(1)
            })
            .ok()
            .and_then(|prior| prior.checked_add(1))
            .ok_or_else(|| "native observation sequence exhausted".into())
    }
    #[allow(
        clippy::suspicious_operation_groupings,
        reason = "an observation's `node` is the PipeWire object `id` of the graph entry"
    )]
    fn update(&self, sequence: u64, observed: Option<(u32, &AudioGraph)>) {
        let Ok(mut broker) = self.broker.lock() else {
            self.invalidate();
            return;
        };
        // The same broker -> graph order as start_capture. No inverse path.
        let mut graph = self.graph.lock();
        if sequence <= graph.sequence {
            return;
        }
        graph.sequence = sequence;
        let observed =
            observed.filter(|(_, graph)| graph.sources.len() <= 128 && graph.sinks.len() <= 128);
        let cookie = observed.map(|(cookie, _)| cookie);
        let observed = observed.map(|(_, graph)| graph);
        graph.sources.retain(|id, old| {
            let present = cookie == Some(old.cookie)
                && observed.is_some_and(|graph| {
                    graph
                        .sources
                        .iter()
                        .any(|source| source.id == old.node && source.serial == old.serial)
                });
            if present {
                old.refresh();
            } else {
                old.alive.store(false, Ordering::Release);
                broker.remove_source(id);
            }
            present
        });
        if let (Some(observed), Some(cookie)) = (observed, cookie) {
            for source in &observed.sources {
                if graph
                    .sources
                    .values()
                    .any(|old| old.node == source.id && old.serial == source.serial)
                {
                    continue;
                }
                let mut nonce = [0_u8; 16];
                if getrandom::fill(&mut nonce).is_err() {
                    continue;
                }
                let id = format!("voice-{}", hex::encode(nonce));
                let label: String = source
                    .description
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(64)
                    .collect();
                let label = if label.is_empty() {
                    "Microphone"
                } else {
                    label.as_str()
                };
                if broker
                    .register_source(
                        &id,
                        label,
                        CaptureKind::Microphone,
                        LocalCaptureHandle::PipeWireNode(source.id),
                    )
                    .is_ok()
                {
                    let entry = Arc::new(Observation::new(
                        source.id,
                        source.serial,
                        cookie,
                        self.base,
                    ));
                    entry.refresh();
                    graph.sources.insert(id, entry);
                }
            }
        }
        let output = observed
            .and_then(|observed| selected_output(observed, graph.output.as_deref(), cookie));
        if !graph.output.as_ref().is_some_and(|old| {
            cookie == Some(old.cookie)
                && output.is_some_and(|new| old.node == new.id && old.serial == new.serial)
        }) {
            if let Some(old) = graph.output.take() {
                tracing::debug!(
                    observed = observed.is_some(),
                    output_present = output.is_some(),
                    cookie_current = cookie == Some(old.cookie),
                    node_current = output.is_some_and(|new| old.node == new.id),
                    serial_current = output.is_some_and(|new| old.serial == new.serial),
                    "native audio output identity retired"
                );
                old.alive.store(false, Ordering::Release);
            }
            // An output exists only for an observed graph, which carries its cookie.
            graph.output = output.zip(cookie).map(|(new, cookie)| {
                Arc::new(Observation::new(new.id, new.serial, cookie, self.base))
            });
        }
        if let Some(output) = &graph.output {
            output.refresh();
        }
    }
    fn invalidate(&self) {
        let graph = self.graph.lock();
        for entry in graph.sources.values().chain(graph.output.iter()) {
            entry.alive.store(false, Ordering::Release);
        }
    }
    pub fn source(&self, id: &str, node: u32) -> Option<Arc<Observation>> {
        self.graph
            .lock()
            .sources
            .get(id)
            .filter(|source| source.node == node && source.current())
            .cloned()
    }
    pub fn output(&self) -> Option<Arc<Observation>> {
        self.graph
            .lock()
            .output
            .as_ref()
            .filter(|output| output.current())
            .cloned()
    }
    pub async fn prepare(&self) -> Result<observer::Observed, String> {
        let sequence = self.sequence()?;
        let result = tokio::time::timeout(FRESH, tokio::task::spawn_blocking(observer::observe))
            .await
            .map_err(|_| "native observation deadline".to_string())?
            .map_err(|_| "native observer stopped".to_string())?;
        self.update(
            sequence,
            result
                .as_ref()
                .ok()
                .map(|value| (value.cookie, &value.graph)),
        );
        result
    }
}
// Default metadata may be withdrawn between policy updates while the exact
// bound sink remains observed. Never choose another sink by label or node alone.
fn selected_output<'a>(
    observed: &'a AudioGraph,
    current: Option<&Observation>,
    cookie: Option<u32>,
) -> Option<&'a media::AudioDevice> {
    observed.default_sink().or_else(|| {
        current
            .filter(|old| cookie == Some(old.cookie))
            .and_then(|old| {
                observed
                    .sinks
                    .iter()
                    .find(|sink| (sink.id, sink.serial) == (old.node, old.serial))
            })
    })
}
impl Drop for Devices {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        let task = self.task.lock().take();
        if let Some(task) = task {
            task.abort();
        }
        self.update(u64::MAX, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portal::CaptureSessionBinding;
    use crate::shell::realtime_session::VerifiedAppRegistration;

    /// Long enough that a random 32-digit hex ID never contains its decimal or hex form by
    /// chance, so the check below only fails for an ID derived from the serial.
    const SERIAL: u32 = 3_987_654_321;
    const NEXT_SERIAL: u32 = 3_987_654_322;

    fn graph(serial: u32) -> AudioGraph {
        AudioGraph {
            sources: vec![media::AudioDevice {
                id: 7,
                serial,
                name: "observed".into(),
                description: "Observed microphone".into(),
                is_default: true,
                volume: None,
                muted: None,
                channels: 1,
                codec: None,
            }],
            ..Default::default()
        }
    }
    #[test]
    fn output_metadata_gap_preserves_exact_sink_but_reuse_or_default_change_retires_it() {
        let devices = Devices {
            broker: Arc::new(std::sync::Mutex::new(CaptureBroker::new())),
            graph: Mutex::new(Graph::default()),
            base: Instant::now(),
            stopped: AtomicBool::new(false),
            next: AtomicU64::new(0),
            task: Mutex::new(None),
            watchers: AtomicUsize::new(0),
            watched: Arc::new(Notify::new()),
        };
        let mut observed = graph(SERIAL);
        observed.sinks = observed.sources.clone();
        observed.sources.clear();
        observed.sinks[0].is_default = true;
        devices.update(devices.sequence().unwrap(), Some((11, &observed)));
        let output = devices.output().unwrap();
        observed.sinks[0].is_default = false;
        devices.update(devices.sequence().unwrap(), Some((11, &observed)));
        assert!(Arc::ptr_eq(&output, &devices.output().unwrap()));
        assert!(output.current());
        // Reusing the same node cannot preserve the retired socket authority.
        observed.sinks[0].serial = NEXT_SERIAL;
        devices.update(devices.sequence().unwrap(), Some((11, &observed)));
        assert!(!output.current());
        assert!(devices.output().is_none());
        observed.sinks[0].is_default = true;
        devices.update(devices.sequence().unwrap(), Some((11, &observed)));
        let output = devices.output().unwrap();
        let mut replacement = observed.sinks[0].clone();
        replacement.id = 8;
        replacement.serial = SERIAL;
        observed.sinks[0].is_default = false;
        observed.sinks.push(replacement);
        devices.update(devices.sequence().unwrap(), Some((11, &observed)));
        assert!(!output.current());
        assert_eq!(devices.output().unwrap().node, 8);
        let output = devices.output().unwrap();
        devices.update(devices.sequence().unwrap(), Some((12, &observed)));
        assert!(!output.current());
        assert_eq!(devices.output().unwrap().cookie, 12);
        let output = devices.output().unwrap();
        observed.sinks.clear();
        devices.update(devices.sequence().unwrap(), Some((12, &observed)));
        assert!(!output.current());
        assert!(devices.output().is_none());
    }
    #[test]
    fn unchanged_observation_preserves_lease_but_node_reuse_revokes_before_return() {
        let broker = Arc::new(std::sync::Mutex::new(CaptureBroker::new()));
        let devices = Devices {
            broker: broker.clone(),
            graph: Mutex::new(Graph::default()),
            base: Instant::now(),
            stopped: AtomicBool::new(false),
            next: AtomicU64::new(0),
            task: Mutex::new(None),
            watchers: AtomicUsize::new(0),
            watched: Arc::new(Notify::new()),
        };
        devices.update(devices.sequence().unwrap(), Some((11, &graph(SERIAL))));
        let id = broker.lock().unwrap().enumerate(CaptureKind::Microphone)[0]
            .id
            .clone();
        let old = devices.source(&id, 7).unwrap();
        assert!(
            !id.contains(&SERIAL.to_string()) && !id.contains(&format!("{SERIAL:x}")),
            "public ID {id} does not expose device serial"
        );
        let app = VerifiedAppRegistration::new_verified("so.tilo.chat", "room").unwrap();
        let binding = CaptureSessionBinding::from_verified(&app, "call", "device").unwrap();
        let now = Instant::now();
        let lease = {
            let mut broker = broker.lock().unwrap();
            let authorization = broker
                .authorize(&binding, &id, CaptureKind::Microphone, now)
                .unwrap();
            broker
                .acquire(authorization, &binding, &id, CaptureKind::Microphone, now)
                .unwrap()
        };
        let revoked = Arc::new(AtomicBool::new(false));
        let flag = revoked.clone();
        broker
            .lock()
            .unwrap()
            .stop_on_revoke(
                lease,
                &binding,
                now,
                Arc::new(move || {
                    flag.store(true, Ordering::SeqCst);
                }),
            )
            .unwrap();
        devices.update(devices.sequence().unwrap(), Some((11, &graph(SERIAL))));
        assert!(Arc::ptr_eq(&old, &devices.source(&id, 7).unwrap()));
        assert!(broker.lock().unwrap().resolve(lease, &binding, now).is_ok());
        assert!(!revoked.load(Ordering::SeqCst));
        devices.update(devices.sequence().unwrap(), Some((11, &graph(NEXT_SERIAL))));
        assert!(revoked.load(Ordering::SeqCst));
        assert!(!old.current());
        assert!(devices.source(&id, 7).is_none());
        assert!(
            broker
                .lock()
                .unwrap()
                .resolve(lease, &binding, now)
                .is_err()
        );
        assert_ne!(
            broker.lock().unwrap().enumerate(CaptureKind::Microphone)[0].id,
            id
        );
        let replacement_id = broker.lock().unwrap().enumerate(CaptureKind::Microphone)[0]
            .id
            .clone();
        let replacement = devices.source(&replacement_id, 7).unwrap();
        // A new daemon may reuse both node and serial. Its identity still
        // retires every previously observed selection and lease.
        devices.update(devices.sequence().unwrap(), Some((12, &graph(NEXT_SERIAL))));
        assert!(!replacement.current());
        assert!(devices.source(&replacement_id, 7).is_none());
        assert_ne!(
            broker.lock().unwrap().enumerate(CaptureKind::Microphone)[0].id,
            replacement_id
        );
        devices.update(devices.sequence().unwrap(), None);
        assert!(
            broker
                .lock()
                .unwrap()
                .enumerate(CaptureKind::Microphone)
                .is_empty()
        );
    }

    /// Refreshes the loop has started so far.
    fn refreshes(devices: &Devices) -> u64 {
        devices.next.load(Ordering::SeqCst)
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now().checked_add(Duration::from_secs(10)).unwrap();
        while !done() {
            assert!(Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// #4074: with no call nothing reads an observation, so the loop does
    /// not run its once-a-second registry round trip; a call's watch starts
    /// it and its end parks it again.
    #[test]
    fn the_refresh_loop_runs_only_while_a_call_watches() {
        let runtime = tilo_rt::daemon("devices-test").workers(1).build().unwrap();
        let devices = Devices::start(
            Arc::new(std::sync::Mutex::new(CaptureBroker::new())),
            runtime.handle(),
        );
        std::thread::sleep(Duration::from_millis(1500));
        assert_eq!(refreshes(&devices), 0, "no call, yet the table refreshed");

        let watch = devices.watch();
        wait_for("a watched table never refreshed twice", || {
            refreshes(&devices) >= 2
        });

        drop(watch);
        // The refresh under way finishes (its observation bounded by
        // `FRESH`, then its one-second pause) and the loop parks.
        std::thread::sleep(FRESH.saturating_add(Duration::from_millis(1500)));
        let parked = refreshes(&devices);
        std::thread::sleep(Duration::from_millis(1500));
        assert_eq!(
            refreshes(&devices),
            parked,
            "the table kept refreshing after the call ended"
        );

        let _watch = devices.watch();
        wait_for("a second watch never woke the loop", || {
            refreshes(&devices) > parked
        });
    }
}
