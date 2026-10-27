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
use tilo_lock::MutexRecover as _;

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
            LocalAudioEdit::RouteOutput(node_id) => route_output(
                &media::audio::enumerate(),
                media::audio::real_control().as_ref(),
                node_id,
            ),
            LocalAudioEdit::SelectInput(node_id) => select_input(
                &media::audio::enumerate(),
                media::audio::real_control().as_ref(),
                node_id,
            ),
            LocalAudioEdit::StepInput { node_id, raise } => {
                crate::shell::device_fabric_runtime::step_input_level(node_id, raise)
            }
        }
    }
}

/// Play everything on one output from now on, including a connected
/// Bluetooth sink: make it the default and let every playback stream follow
/// the default. The node is resolved against `graph`, read just before.
///
/// Streams are released to the default, never pinned to the chosen device:
/// a stream with an explicit target stays on it whatever the default does
/// later, and `WirePlumber` remembers that target for the app's next stream.
/// Pinned, every app playing when an output was chosen here kept playing
/// there after a headset was plugged in or another output was chosen in
/// Sound, and kept going back to it when it next played.
fn route_output(
    graph: &AudioGraph,
    control: &dyn media::audio::GraphControl,
    node_id: u32,
) -> Result<(), String> {
    graph
        .sink_by_id(node_id)
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-output-gone"))?;
    control.set_default_sink(node_id)?;
    for stream in &graph.playback_streams {
        control.unroute_stream(stream.id)?;
    }
    Ok(())
}

/// Record from one input from now on: make it the default source and let
/// every capture stream follow the default, as [`route_output`] releases
/// playback.
fn select_input(
    graph: &AudioGraph,
    control: &dyn media::audio::GraphControl,
    node_id: u32,
) -> Result<(), String> {
    graph
        .sources
        .iter()
        .find(|source| source.id == node_id)
        .ok_or_else(|| crate::shell::i18n::t("device-fabric-error-input-gone"))?;
    control.set_default_source(node_id)?;
    for stream in &graph.record_streams {
        control.unroute_stream(stream.id)?;
    }
    Ok(())
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
        self.state.lock_recover()
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
        tilo_test_env::wait_until("the first graph read", || !cache.graph().sinks.is_empty());
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
            tilo_test_env::recv("the queued edit", &applied),
            LocalAudioEdit::RouteOutput(41)
        );
        tilo_test_env::wait_until("the graph to be read again after the edit", || {
            reads.load(Ordering::SeqCst) > before
        });
    }
}

#[cfg(test)]
mod route_tests {
    use super::{route_output, select_input};
    use media::audio::{AudioDevice, AudioGraph, AudioStream, GraphControl};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorded(Mutex<Vec<String>>);

    impl Recorded {
        fn push(&self, call: String) {
            self.0.lock().unwrap().push(call);
        }
        fn calls(&self) -> Vec<String> {
            self.0.lock().unwrap().clone()
        }
    }

    impl GraphControl for Recorded {
        fn set_default_sink(&self, node_id: u32) -> Result<(), String> {
            self.push(format!("default-sink {node_id}"));
            Ok(())
        }
        fn set_default_source(&self, node_id: u32) -> Result<(), String> {
            self.push(format!("default-source {node_id}"));
            Ok(())
        }
        fn route_stream(&self, stream_id: u32, target_serial: u32) -> Result<(), String> {
            self.push(format!("pin {stream_id} to {target_serial}"));
            Ok(())
        }
        fn unroute_stream(&self, stream_id: u32) -> Result<(), String> {
            self.push(format!("follow-default {stream_id}"));
            Ok(())
        }
        fn set_node_volume(&self, node_id: u32, _: f32) -> Result<(), String> {
            self.push(format!("volume {node_id}"));
            Ok(())
        }
        fn set_node_mute(&self, node_id: u32, _: bool) -> Result<(), String> {
            self.push(format!("mute {node_id}"));
            Ok(())
        }
        fn set_device_profile(&self, device_id: u32, _: u32) -> Result<(), String> {
            self.push(format!("profile {device_id}"));
            Ok(())
        }
    }

    fn device(id: u32, name: &str, is_default: bool) -> AudioDevice {
        AudioDevice {
            id,
            serial: id.saturating_add(1000),
            name: name.into(),
            description: name.into(),
            is_default,
            volume: None,
            muted: None,
            channels: 2,
            codec: None,
        }
    }

    fn stream(id: u32, app: &str, playback: bool, target_id: u32) -> AudioStream {
        AudioStream {
            id,
            serial: id.saturating_add(1000),
            app: app.into(),
            media_name: None,
            playback,
            target_id: Some(target_id),
            volume: None,
            muted: None,
        }
    }

    fn graph() -> AudioGraph {
        AudioGraph {
            sinks: vec![
                device(40, "alsa_output.speakers", true),
                device(41, "bluez_output.AA_BB.1", false),
            ],
            sources: vec![
                device(50, "alsa_input.internal-mic", true),
                device(51, "alsa_input.usb-headset", false),
            ],
            playback_streams: vec![stream(70, "Music", true, 40), stream(71, "Web", true, 40)],
            record_streams: vec![stream(80, "Call", false, 50)],
            hardware: Vec::new(),
        }
    }

    #[test]
    fn choosing_an_output_or_input_leaves_every_stream_following_the_default() {
        let control = Recorded::default();
        route_output(&graph(), &control, 41).expect("a listed output");
        assert_eq!(
            control.calls(),
            ["default-sink 41", "follow-default 70", "follow-default 71"],
            "a stream pinned to the chosen output would stay there when a headset is \
             plugged in or another output is chosen, and the app would go back to it next time"
        );

        let control = Recorded::default();
        select_input(&graph(), &control, 51).expect("a listed input");
        assert_eq!(control.calls(), ["default-source 51", "follow-default 80"]);
    }

    #[test]
    fn a_device_that_left_is_not_chosen() {
        let control = Recorded::default();
        assert!(route_output(&graph(), &control, 7).is_err());
        assert!(select_input(&graph(), &control, 7).is_err());
        assert!(
            control.calls().is_empty(),
            "nothing moved: {:?}",
            control.calls()
        );
    }
}
