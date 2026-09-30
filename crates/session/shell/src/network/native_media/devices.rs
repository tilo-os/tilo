//! One independently refreshed observation table. No device enumeration runs
//! while the shell holds a capture lease or service installation lock.
use super::{
    Arc, AtomicBool, AtomicU64, BTreeMap, Duration, Instant, Mutex, Ordering, media_wire, observer,
};
use crate::portal::{CaptureBroker, CaptureKind, capture_broker::LocalCaptureHandle};
use media::AudioGraph;

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
        self.alive.load(Ordering::Acquire)
            && self.base.elapsed().as_nanos() < u128::from(self.deadline.load(Ordering::Acquire))
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
}
impl Devices {
    pub fn start(
        broker: Arc<std::sync::Mutex<CaptureBroker>>,
        runtime: &tokio::runtime::Handle,
    ) -> Arc<Self> {
        let value = Arc::new(Self {
            broker,
            graph: Mutex::new(Graph::default()),
            base: Instant::now(),
            stopped: AtomicBool::new(false),
            next: AtomicU64::new(0),
            task: Mutex::new(None),
        });
        let weak = Arc::downgrade(&value);
        let task = runtime.spawn(async move {
            loop {
                let Some(owner) = weak.upgrade() else {
                    return;
                };
                let Ok(sequence) = owner.sequence() else {
                    return;
                };
                drop(owner);
                let mut read = tokio::task::spawn_blocking(observer::observe);
                let observed = match tokio::time::timeout(FRESH, &mut read).await {
                    Ok(Ok(Ok(observed))) => Some(observed),
                    _ => None,
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
                let Ok(nonce) = media_wire::fresh_nonce() else {
                    continue;
                };
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
        let output = observed.and_then(AudioGraph::default_sink);
        if !graph.output.as_ref().is_some_and(|old| {
            cookie == Some(old.cookie)
                && output.is_some_and(|new| old.node == new.id && old.serial == new.serial)
        }) {
            if let Some(old) = graph.output.take() {
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
    fn unchanged_observation_preserves_lease_but_node_reuse_revokes_before_return() {
        let broker = Arc::new(std::sync::Mutex::new(CaptureBroker::new()));
        let devices = Devices {
            broker: broker.clone(),
            graph: Mutex::new(Graph::default()),
            base: Instant::now(),
            stopped: AtomicBool::new(false),
            next: AtomicU64::new(0),
            task: Mutex::new(None),
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
}
