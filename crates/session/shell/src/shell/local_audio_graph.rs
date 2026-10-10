//! The local `PipeWire` graph as the Devices pane and the now-playing card
//! read it, kept current off the compositor thread.
//!
//! Enumerating the graph runs `pw-dump`, a subprocess bounded at 5 s whose
//! output can reach 8 MiB. The chrome asks for its device view every few
//! hundred milliseconds while Quick Settings or the now-playing card is
//! open, so building the view inline forked and parsed `pw-dump` on the
//! compositor thread at that rate. Here one thread refreshes a cached graph
//! while somebody is reading it, and parks otherwise; output and input edits
//! run on the same thread, each followed by a fresh read.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use media::audio::AudioGraph;

/// How often a graph that is being read is read again.
const REFRESH_INTERVAL: Duration = Duration::from_millis(1_500);

/// How long after the last read the refresh keeps running.
const DEMAND_WINDOW: Duration = Duration::from_secs(5);

/// Edits waiting at once; a person cannot click faster than this drains.
const MAX_QUEUED_EDITS: usize = 16;

/// One change to the local graph the Devices pane can ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LocalAudioEdit {
    /// Make this output the default and move every playback stream to it.
    RouteOutput(u32),
    /// Make this input the default and move every capture stream to it.
    SelectInput(u32),
    /// Step this input's level up or down.
    StepInput { node_id: u32, raise: bool },
}

/// What the graph thread drives. Production is [`SystemLocalAudio`].
pub(crate) trait LocalAudioBackend: Send + 'static {
    fn enumerate(&mut self) -> AudioGraph;
    fn apply(&mut self, edit: LocalAudioEdit) -> Result<(), String>;
}

/// `pw-dump` and `wpctl`, through `tilo-media`.
pub(crate) struct SystemLocalAudio;

impl LocalAudioBackend for SystemLocalAudio {
    fn enumerate(&mut self) -> AudioGraph {
        media::audio::enumerate()
    }

    fn apply(&mut self, edit: LocalAudioEdit) -> Result<(), String> {
        match edit {
            LocalAudioEdit::RouteOutput(node_id) => {
                crate::shell::device_fabric_runtime::route_local_audio(node_id)
            }
            LocalAudioEdit::SelectInput(node_id) => {
                crate::shell::device_fabric_runtime::select_local_input(node_id)
            }
            LocalAudioEdit::StepInput { node_id, raise } => {
                crate::shell::device_fabric_runtime::step_input_level(node_id, raise)
            }
        }
    }
}

#[derive(Default)]
struct GraphState {
    graph: Option<AudioGraph>,
    /// Bumped whenever a read finds the graph changed.
    revision: u64,
    read: Option<Instant>,
    edits: VecDeque<LocalAudioEdit>,
    closed: bool,
}

struct Shared {
    state: Mutex<GraphState>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, GraphState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The cached local graph and the thread that keeps it.
pub(crate) struct LocalAudioGraph {
    shared: Arc<Shared>,
}

impl LocalAudioGraph {
    /// Read `PipeWire`; `repaint` asks for a frame whenever the graph
    /// somebody is looking at changes.
    pub(crate) fn system(repaint: crate::session_runtime::Wake) -> Self {
        Self::new(SystemLocalAudio, repaint)
    }

    pub(crate) fn new(
        backend: impl LocalAudioBackend,
        repaint: crate::session_runtime::Wake,
    ) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(GraphState::default()),
            wake: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        if let Err(error) = std::thread::Builder::new()
            .name("tilo-audio-graph".into())
            .spawn(move || {
                let mut backend = backend;
                run(&worker, &mut backend, &*repaint);
            })
        {
            tracing::warn!(%error, "could not start the local audio graph reader");
        }
        Self { shared }
    }

    /// The graph as last read, empty until the first read lands. Reading
    /// keeps the refresh running for a few seconds.
    pub(crate) fn graph(&self) -> AudioGraph {
        let mut state = self.shared.lock();
        state.read = Some(Instant::now());
        let graph = state.graph.clone().unwrap_or_default();
        drop(state);
        self.shared.wake.notify_one();
        graph
    }

    /// Changes each time the cached graph does, so a view built from it
    /// knows when it is stale.
    pub(crate) fn revision(&self) -> u64 {
        self.shared.lock().revision
    }

    /// Queue `edit` once the node it names is in the graph as last read.
    ///
    /// # Errors
    ///
    /// Returns the pane's message when the node is gone or too many edits
    /// are already waiting.
    pub(crate) fn submit(&self, edit: LocalAudioEdit) -> Result<(), String> {
        let mut state = self.shared.lock();
        let graph = state.graph.as_ref();
        let known = match edit {
            LocalAudioEdit::RouteOutput(node_id) => graph
                .and_then(|graph| graph.sink_by_id(node_id))
                .ok_or("device-fabric-error-output-gone"),
            LocalAudioEdit::SelectInput(node_id) | LocalAudioEdit::StepInput { node_id, .. } => {
                graph
                    .and_then(|graph| graph.sources.iter().find(|source| source.id == node_id))
                    .ok_or("device-fabric-error-input-gone")
            }
        };
        if let Err(key) = known {
            return Err(crate::shell::i18n::t(key));
        }
        if state.edits.len() >= MAX_QUEUED_EDITS {
            return Err(crate::shell::i18n::t("device-fabric-error-audio-busy"));
        }
        state.edits.push_back(edit);
        drop(state);
        self.shared.wake.notify_one();
        Ok(())
    }
}

impl Drop for LocalAudioGraph {
    fn drop(&mut self) {
        self.shared.lock().closed = true;
        self.shared.wake.notify_one();
    }
}

/// What the thread found to do; either way the graph is read afterwards.
enum Work {
    Edits(Vec<LocalAudioEdit>),
    Refresh,
}

fn run(shared: &Shared, backend: &mut impl LocalAudioBackend, repaint: &(dyn Fn() + Send + Sync)) {
    let mut refreshed: Option<Instant> = None;
    loop {
        let Some(work) = next_work(shared, refreshed) else {
            return;
        };
        if let Work::Edits(edits) = work {
            for edit in edits {
                if let Err(error) = backend.apply(edit) {
                    tracing::warn!(target: "tilo::media", %error, ?edit, "local audio edit failed");
                }
            }
        }
        let graph = backend.enumerate();
        let changed = {
            let mut state = shared.lock();
            let changed = state.graph.as_ref() != Some(&graph);
            if changed {
                state.graph = Some(graph);
                state.revision = state.revision.wrapping_add(1);
            }
            changed
        };
        refreshed = Some(Instant::now());
        if changed {
            repaint();
        }
    }
}

/// Wait until there is an edit to run, or a graph somebody is reading is
/// due a refresh; `None` once the cache is dropped.
fn next_work(shared: &Shared, refreshed: Option<Instant>) -> Option<Work> {
    let mut state = shared.lock();
    loop {
        if state.closed {
            return None;
        }
        if !state.edits.is_empty() {
            return Some(Work::Edits(std::mem::take(&mut state.edits).into()));
        }
        let now = Instant::now();
        let demanded = state
            .read
            .is_some_and(|read| now.saturating_duration_since(read) < DEMAND_WINDOW);
        if !demanded {
            state = shared
                .wake
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
            continue;
        }
        let due_in = refreshed.map_or(Duration::ZERO, |refreshed| {
            REFRESH_INTERVAL.saturating_sub(now.saturating_duration_since(refreshed))
        });
        if due_in.is_zero() {
            return Some(Work::Refresh);
        }
        state = shared
            .wake
            .wait_timeout(state, due_in)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    struct Scripted {
        reads: Arc<AtomicUsize>,
        edits: mpsc::Sender<LocalAudioEdit>,
        graph: AudioGraph,
    }

    impl LocalAudioBackend for Scripted {
        fn enumerate(&mut self) -> AudioGraph {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.graph.clone()
        }

        fn apply(&mut self, edit: LocalAudioEdit) -> Result<(), String> {
            self.edits.send(edit).unwrap();
            Ok(())
        }
    }

    fn sink(id: u32) -> media::audio::AudioDevice {
        media::audio::AudioDevice {
            id,
            serial: id,
            name: format!("alsa_output.{id}"),
            description: format!("Output {id}"),
            is_default: false,
            volume: None,
            muted: None,
            channels: 2,
            codec: None,
        }
    }

    fn wait_until(condition: impl Fn() -> bool) -> bool {
        let started = Instant::now();
        while !condition() {
            if started.elapsed() > Duration::from_secs(10) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }

    #[test]
    fn device_view_reads_the_cached_graph_and_edits_run_off_thread() {
        let reads = Arc::new(AtomicUsize::new(0));
        let (edits, applied) = mpsc::channel();
        let repaints = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&repaints);
        let cache = LocalAudioGraph::new(
            Scripted {
                reads: Arc::clone(&reads),
                edits,
                graph: AudioGraph {
                    sinks: vec![sink(41)],
                    ..AudioGraph::default()
                },
            },
            Arc::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
            }),
        );
        assert_eq!(
            reads.load(Ordering::SeqCst),
            0,
            "nothing is read before anybody looks"
        );
        assert_eq!(
            cache.graph(),
            AudioGraph::default(),
            "the first look never waits"
        );
        assert!(wait_until(|| !cache.graph().sinks.is_empty()));
        assert!(
            repaints.load(Ordering::SeqCst) >= 1,
            "the first read asks for a frame"
        );
        assert_eq!(cache.revision(), 1, "a view built before the read is stale");

        assert!(
            cache.submit(LocalAudioEdit::RouteOutput(7)).is_err(),
            "an output the pane no longer lists is refused at once"
        );
        let before = reads.load(Ordering::SeqCst);
        cache
            .submit(LocalAudioEdit::RouteOutput(41))
            .expect("a listed output is queued");
        assert_eq!(
            applied.recv_timeout(Duration::from_secs(10)),
            Ok(LocalAudioEdit::RouteOutput(41))
        );
        assert!(
            wait_until(|| reads.load(Ordering::SeqCst) > before),
            "the graph is read again after the edit"
        );
    }
}
