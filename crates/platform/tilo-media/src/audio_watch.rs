//! Event-driven notice that the audio *device* graph changed.
//!
//! A registry listener on its own `PipeWire` loop thread reports when an
//! audio device or device node (a sink, a source, a sound card) appears or
//! disappears, or when the default sink or source moves. Nothing else counts:
//! streams, clients, ports and links come and go every time anything plays a
//! sound or a tool like `pw-dump` connects, and a watcher that woke on them
//! would never go quiet. Between changes the thread blocks in the loop's
//! `epoll` with no timer at all.
//!
//! The watch reports *that* the graph changed, not what it now is: consumers
//! re-read the graph they need ([`crate::audio::enumerate`]). A burst of
//! changes — a headset arriving brings a card, a sink, a source and a default
//! switch — is reported once, [`DEBOUNCE`] after the first of them, so the
//! reaction time is bounded by the window rather than stretched by the burst.
//!
//! This is the change source the session's single audio-graph owner can grow
//! into: today it notifies, a graph reducer behind it can later publish the
//! graph itself.
//!
//! [`watch_output_levels_and_capture`] also follows capture streams (a
//! microphone or camera recording) for the privacy indicator: it reports how
//! many are running each time that number changes, from the streams' own
//! state on the same connection. Nothing is started to find out, and an idle
//! graph reports nothing.

use std::collections::BTreeSet;
use std::time::Duration;

/// How long after the first change of a burst the watch reports it.
pub const DEBOUNCE: Duration = Duration::from_millis(50);

/// What the watch reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphWatchEvent {
    /// The device graph changed. Also sent once as soon as the watch has
    /// read the initial registry, as the baseline to read the graph against.
    Changed,
    /// The connection to `PipeWire` ended. Nothing further is reported; the
    /// consumer observes some other way and watches again later.
    Lost,
}

/// The kind of a registry global, as far as the device graph cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobalKind {
    Node,
    Device,
    Other,
}

/// The registry ids that belong to the device graph, so their removal is a
/// change and everyone else's is not.
#[derive(Debug, Default)]
pub struct DeviceGlobals {
    tracked: BTreeSet<u32>,
}

impl DeviceGlobals {
    /// Whether global `id` is part of the device graph. It is tracked if so,
    /// so that its removal is seen as a change too.
    pub fn added(&mut self, id: u32, kind: GlobalKind, media_class: Option<&str>) -> bool {
        let device = match kind {
            GlobalKind::Node | GlobalKind::Device => media_class.is_some_and(is_device_class),
            GlobalKind::Other => false,
        };
        if device {
            self.tracked.insert(id);
        }
        device
    }

    /// Whether the removed global `id` was part of the device graph.
    pub fn removed(&mut self, id: u32) -> bool {
        self.tracked.remove(&id)
    }
}

/// Sinks, sources, duplex nodes and sound cards; not `Stream/…/Audio`, the
/// per-application streams.
fn is_device_class(media_class: &str) -> bool {
    media_class.starts_with("Audio/")
}

/// The `default` metadata keys whose change moves where sound goes.
pub fn is_default_device_key(key: &str) -> bool {
    matches!(key, "default.audio.sink" | "default.audio.source")
}

/// Whether `media_class` is an output device node, whose level and mute
/// [`watch_output_levels`] follows.
pub fn is_output_class(media_class: &str) -> bool {
    media_class == "Audio/Sink"
}

/// Whether `media_class` is a capture stream the privacy indicator
/// attributes: an app recording a microphone, or a camera stream. The same
/// classes `os_actions::active_media_uses` reads.
pub fn is_capture_class(media_class: &str) -> bool {
    matches!(
        media_class,
        "Stream/Input/Audio" | "Stream/Input/Video" | "Stream/Output/Video"
    )
}

/// Which capture streams are running, so that only a change in that set is
/// reported: a stream that is created, paused or resumed without changing
/// whether it records is not news.
#[derive(Debug, Default)]
pub struct CaptureStreams {
    running: BTreeSet<u32>,
}

impl CaptureStreams {
    /// Capture stream `id` reported its state. Whether the running set
    /// changed.
    pub fn state(&mut self, id: u32, running: bool) -> bool {
        if running {
            self.running.insert(id)
        } else {
            self.running.remove(&id)
        }
    }

    /// Global `id` went away. Whether it was a running capture stream.
    pub fn removed(&mut self, id: u32) -> bool {
        self.running.remove(&id)
    }

    /// How many capture streams are running.
    pub fn running(&self) -> usize {
        self.running.len()
    }
}

/// What a watch follows besides the device graph.
#[cfg_attr(
    not(all(feature = "pipewire", target_os = "linux")),
    expect(dead_code, reason = "only the native watch reads what to follow")
)]
struct Follow {
    output_levels: bool,
    /// Told how many capture streams run, on each change of that number.
    capture: Option<Box<dyn Fn(usize) + Send>>,
}

/// A running watch. Dropping it stops and joins the loop thread.
pub struct AudioGraphWatch {
    #[cfg(all(feature = "pipewire", target_os = "linux"))]
    _native: native::Watch,
}

impl std::fmt::Debug for AudioGraphWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioGraphWatch").finish_non_exhaustive()
    }
}

/// Start watching the device graph. `notify` runs on the watch's thread; it
/// should only hand the news on.
///
/// # Errors
///
/// Returns a diagnostic when `PipeWire` cannot be reached or this build has
/// no native `PipeWire` support. The caller falls back to a slower way of
/// observing — never to not observing.
pub fn watch(notify: impl Fn(GraphWatchEvent) + Send + 'static) -> Result<AudioGraphWatch, String> {
    spawn(
        notify,
        Follow {
            output_levels: false,
            capture: None,
        },
    )
}

/// Like [`watch`], and also reports a change of any output's volume or
/// mute (its `Props`), from whoever made it: a volume key, a slider, another
/// app, `wpctl` in a terminal. An output's level changes only when someone
/// changes it, so this goes quiet as the device graph does.
///
/// # Errors
///
/// As [`watch`].
pub fn watch_output_levels(
    notify: impl Fn(GraphWatchEvent) + Send + 'static,
) -> Result<AudioGraphWatch, String> {
    spawn(
        notify,
        Follow {
            output_levels: true,
            capture: None,
        },
    )
}

/// Like [`watch_output_levels`], and also tells `capture` how many capture
/// streams ([`is_capture_class`]) are running: once with the baseline, then
/// [`DEBOUNCE`] after the first change of a burst that moved the number of
/// running streams. One connection serves both, so a status reader needs no
/// second client, thread or process to keep the privacy indicator true.
///
/// # Errors
///
/// As [`watch`].
pub fn watch_output_levels_and_capture(
    notify: impl Fn(GraphWatchEvent) + Send + 'static,
    capture: impl Fn(usize) + Send + 'static,
) -> Result<AudioGraphWatch, String> {
    spawn(
        notify,
        Follow {
            output_levels: true,
            capture: Some(Box::new(capture)),
        },
    )
}

fn spawn(
    notify: impl Fn(GraphWatchEvent) + Send + 'static,
    follow: Follow,
) -> Result<AudioGraphWatch, String> {
    #[cfg(all(feature = "pipewire", target_os = "linux"))]
    {
        native::spawn(notify, follow).map(|native| AudioGraphWatch { _native: native })
    }
    #[cfg(not(all(feature = "pipewire", target_os = "linux")))]
    {
        drop((notify, follow));
        Err("native PipeWire support is not compiled in".into())
    }
}

#[cfg(all(feature = "pipewire", target_os = "linux"))]
mod native {
    //! The registry listener and its loop thread.

    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;
    use std::rc::Rc;
    use std::sync::mpsc::{SyncSender, sync_channel};
    use std::thread::JoinHandle;
    use std::time::Instant;

    use pipewire as pw;
    use pw::types::ObjectType;

    use super::{
        CaptureStreams, DEBOUNCE, DeviceGlobals, Follow, GlobalKind, GraphWatchEvent,
        is_capture_class, is_default_device_key, is_output_class,
    };

    pub(super) struct Watch {
        stop: pw::channel::Sender<()>,
        thread: Option<JoinHandle<()>>,
    }

    impl Drop for Watch {
        fn drop(&mut self) {
            // The thread may already have ended on a lost connection, taking
            // the receiver with it; then there is nothing to stop.
            let _ = self.stop.send(());
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    pub(super) fn spawn(
        notify: impl Fn(GraphWatchEvent) + Send + 'static,
        follow: Follow,
    ) -> Result<Watch, String> {
        let (ready_tx, ready_rx) = sync_channel(1);
        let (stop, stop_rx) = pw::channel::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("tilo-audio-graph".into())
            .spawn(move || run(&notify, &follow, stop_rx, &ready_tx))
            .map_err(|error| format!("start audio graph watch: {error}"))?;
        let watch = Watch {
            stop,
            thread: Some(thread),
        };
        // The thread answers before it blocks: connecting is synchronous and
        // fails at once when there is no daemon to connect to.
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(watch),
            Ok(Err(error)) => Err(error),
            Err(_) => Err("the audio graph watch ended while connecting".into()),
        }
    }

    /// The bound `default` metadata: its global id, listener and proxy — in
    /// that order, so the listener drops before the proxy it listens on.
    type Bound = (u32, pw::metadata::MetadataListener, pw::metadata::Metadata);

    /// A bound node: its listener and proxy, listener first. An output's
    /// listener follows its `Props`, a capture stream's its state.
    type Output = (pw::node::NodeListener, pw::node::Node);

    type Connected = (
        pw::main_loop::MainLoopRc,
        pw::context::ContextRc,
        pw::core::CoreRc,
        pw::registry::RegistryRc,
    );

    fn connect() -> Result<Connected, String> {
        let unavailable = |what: &str| format!("PipeWire {what} unavailable");
        let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|_| unavailable("loop"))?;
        let context =
            pw::context::ContextRc::new(&mainloop, None).map_err(|_| unavailable("context"))?;
        let core = context
            .connect_rc(None)
            .map_err(|_| unavailable("connection"))?;
        let registry = core
            .get_registry_rc()
            .map_err(|_| unavailable("registry"))?;
        Ok((mainloop, context, core, registry))
    }

    /// Where the initial registry read stands: two round trips, one for the
    /// globals and the metadata bind they cause, one for the metadata's
    /// properties, then the baseline.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Phase {
        Globals,
        Properties,
        Live,
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one function owns the loop, the registry listener and the callbacks' shared state"
    )]
    fn run(
        notify: &dyn Fn(GraphWatchEvent),
        follow: &Follow,
        stop_rx: pw::channel::Receiver<()>,
        ready: &SyncSender<Result<(), String>>,
    ) {
        let output_levels = follow.output_levels;
        let follow_capture = follow.capture.is_some();
        let (mainloop, _context, core, registry) = match connect() {
            Ok(connected) => connected,
            Err(error) => {
                let _ = ready.send(Err(error));
                return;
            }
        };

        let stopped = Rc::new(Cell::new(false));
        let lost = Rc::new(Cell::new(false));
        let live = Rc::new(Cell::new(false));
        // When the first change not yet reported happened.
        let dirty = Rc::new(Cell::new(None::<Instant>));
        let completed = Rc::new(Cell::new(None));
        let globals = Rc::new(RefCell::new(DeviceGlobals::default()));
        let metadata = Rc::new(RefCell::new(None::<Bound>));
        let outputs = Rc::new(RefCell::new(BTreeMap::<u32, Output>::new()));
        // When the first change to the running capture streams not yet
        // reported happened, those streams, and their bound nodes.
        let capture_dirty = Rc::new(Cell::new(None::<Instant>));
        let capture = Rc::new(RefCell::new(CaptureStreams::default()));
        let streams = Rc::new(RefCell::new(BTreeMap::<u32, Output>::new()));

        let marker = |dirty: &Rc<Cell<Option<Instant>>>| {
            let dirty = Rc::clone(dirty);
            let live = Rc::clone(&live);
            move || {
                if live.get() && dirty.get().is_none() {
                    dirty.set(Some(Instant::now()));
                }
            }
        };
        let mark = marker(&dirty);
        let mark_capture = marker(&capture_dirty);

        let _stop = stop_rx.attach(mainloop.loop_(), {
            let stopped = Rc::clone(&stopped);
            move |()| stopped.set(true)
        });
        let _core_listener = core
            .add_listener_local()
            .error({
                let lost = Rc::clone(&lost);
                move |id, _, _, _| {
                    // A failed bind is about one object; an error on the core
                    // itself is the connection going away.
                    if id == pw::core::PW_ID_CORE {
                        lost.set(true);
                    }
                }
            })
            .done({
                let completed = Rc::clone(&completed);
                move |id, seq| {
                    if id == pw::core::PW_ID_CORE {
                        completed.set(Some(seq));
                    }
                }
            })
            .register();
        let _registry_listener = registry
            .add_listener_local()
            .global({
                let registry = registry.clone();
                let globals = Rc::clone(&globals);
                let metadata = Rc::clone(&metadata);
                let outputs = Rc::clone(&outputs);
                let capture = Rc::clone(&capture);
                let streams = Rc::clone(&streams);
                let mark = mark.clone();
                let mark_capture = mark_capture.clone();
                move |global| {
                    let props = global.props.as_ref();
                    let property = |key: &str| props.and_then(|props| props.get(key));
                    if global.type_ == ObjectType::Metadata
                        && property("metadata.name") == Some("default")
                    {
                        if metadata.borrow().is_some() {
                            return;
                        }
                        let Ok(object) = registry.bind::<pw::metadata::Metadata, _>(global) else {
                            return;
                        };
                        let listener = object
                            .add_listener_local()
                            .property({
                                let mark = mark.clone();
                                move |subject, key, _, _| {
                                    if subject == 0 && key.is_some_and(is_default_device_key) {
                                        mark();
                                    }
                                    0
                                }
                            })
                            .register();
                        *metadata.borrow_mut() = Some((global.id, listener, object));
                        mark();
                        return;
                    }
                    let kind = match global.type_ {
                        ObjectType::Node => GlobalKind::Node,
                        ObjectType::Device => GlobalKind::Device,
                        _ => GlobalKind::Other,
                    };
                    if globals
                        .borrow_mut()
                        .added(global.id, kind, property("media.class"))
                    {
                        mark();
                    }
                    if output_levels
                        && kind == GlobalKind::Node
                        && property("media.class").is_some_and(is_output_class)
                        && let Ok(node) = registry.bind::<pw::node::Node, _>(global)
                    {
                        let listener = node
                            .add_listener_local()
                            .param({
                                let mark = mark.clone();
                                move |_, _, _, _, _| mark()
                            })
                            .register();
                        node.subscribe_params(&[pw::spa::param::ParamType::Props]);
                        outputs.borrow_mut().insert(global.id, (listener, node));
                    }
                    if follow_capture
                        && kind == GlobalKind::Node
                        && property("media.class").is_some_and(is_capture_class)
                        && let Ok(node) = registry.bind::<pw::node::Node, _>(global)
                    {
                        // Bound, the node reports its whole info, state
                        // included, and then each change of it.
                        let id = global.id;
                        let listener = node
                            .add_listener_local()
                            .info({
                                let capture = Rc::clone(&capture);
                                let mark_capture = mark_capture.clone();
                                move |info| {
                                    // The raw state: `NodeInfoRef::state`
                                    // panics on an error without a message.
                                    let running = info.as_raw().state
                                        == pw::sys::pw_node_state_PW_NODE_STATE_RUNNING;
                                    if capture.borrow_mut().state(id, running) {
                                        mark_capture();
                                    }
                                }
                            })
                            .register();
                        streams.borrow_mut().insert(id, (listener, node));
                    }
                }
            })
            .global_remove({
                let globals = Rc::clone(&globals);
                let metadata = Rc::clone(&metadata);
                let outputs = Rc::clone(&outputs);
                let capture = Rc::clone(&capture);
                let streams = Rc::clone(&streams);
                move |id| {
                    let output = outputs.borrow_mut().remove(&id);
                    drop(output);
                    let stream = streams.borrow_mut().remove(&id);
                    drop(stream);
                    if capture.borrow_mut().removed(id) {
                        mark_capture();
                    }
                    let unbound = metadata
                        .borrow()
                        .as_ref()
                        .is_some_and(|(bound, _, _)| *bound == id);
                    if unbound {
                        // A restarted session manager publishes a new one to
                        // bind; this one goes, listener before proxy.
                        let unbound = metadata.borrow_mut().take();
                        drop(unbound);
                        mark();
                    } else if globals.borrow_mut().removed(id) {
                        mark();
                    }
                }
            })
            .register();

        let mut phase = Phase::Globals;
        let Ok(mut pending) = core.sync(0) else {
            let _ = ready.send(Err("PipeWire synchronization failed".into()));
            return;
        };
        if ready.send(Ok(())).is_err() {
            return;
        }
        let report_capture = || {
            if let Some(report) = &follow.capture {
                report(capture.borrow().running());
            }
        };
        loop {
            // The first change of either kind still to report.
            let first = match (dirty.get(), capture_dirty.get()) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            let timeout = match first {
                // Rounded up: the loop counts milliseconds, and rounding a
                // remainder down to zero would spin until it elapsed.
                Some(since) => pw::loop_::Timeout::Finite(
                    DEBOUNCE
                        .saturating_sub(since.elapsed())
                        .saturating_add(std::time::Duration::from_micros(999)),
                ),
                None => pw::loop_::Timeout::Infinite,
            };
            let result = mainloop.loop_().iterate(timeout);
            if result < 0 && result != -libc::EINTR {
                lost.set(true);
            }
            if stopped.get() {
                return;
            }
            if lost.get() {
                notify(GraphWatchEvent::Lost);
                return;
            }
            if phase != Phase::Live && completed.get() == Some(pending) {
                if phase == Phase::Globals {
                    phase = Phase::Properties;
                    let Ok(next) = core.sync(1) else {
                        notify(GraphWatchEvent::Lost);
                        return;
                    };
                    pending = next;
                } else {
                    phase = Phase::Live;
                    live.set(true);
                    dirty.set(None);
                    capture_dirty.set(None);
                    notify(GraphWatchEvent::Changed);
                    report_capture();
                }
            }
            if let Some(since) = dirty.get()
                && since.elapsed() >= DEBOUNCE
            {
                dirty.set(None);
                notify(GraphWatchEvent::Changed);
            }
            if let Some(since) = capture_dirty.get()
                && since.elapsed() >= DEBOUNCE
            {
                capture_dirty.set(None);
                report_capture();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_devices_and_device_nodes_are_the_graph() {
        let mut globals = DeviceGlobals::default();
        assert!(globals.added(1, GlobalKind::Node, Some("Audio/Sink")));
        assert!(globals.added(2, GlobalKind::Node, Some("Audio/Source")));
        assert!(globals.added(3, GlobalKind::Device, Some("Audio/Device")));
        assert!(
            !globals.added(4, GlobalKind::Node, Some("Stream/Output/Audio")),
            "an app playing a sound is not a device change"
        );
        assert!(
            !globals.added(5, GlobalKind::Other, Some("Audio/Sink")),
            "pw-dump's own client, ports and links are not the graph"
        );
        assert!(!globals.added(6, GlobalKind::Node, None));
        assert!(!globals.added(7, GlobalKind::Device, Some("Video/Device")));
    }

    #[test]
    fn removing_a_device_is_a_change_and_removing_anything_else_is_not() {
        let mut globals = DeviceGlobals::default();
        globals.added(1, GlobalKind::Node, Some("Audio/Sink"));
        globals.added(4, GlobalKind::Node, Some("Stream/Output/Audio"));
        assert!(!globals.removed(4), "a stream ending");
        assert!(!globals.removed(9), "a client disconnecting");
        assert!(globals.removed(1), "headphones unplugged");
        assert!(!globals.removed(1), "an id is removed once");
    }

    #[test]
    fn only_the_default_device_keys_move_sound() {
        assert!(is_default_device_key("default.audio.sink"));
        assert!(is_default_device_key("default.audio.source"));
        assert!(!is_default_device_key("default.video.source"));
        assert!(!is_default_device_key("default.configured.audio.sink"));
    }

    #[test]
    fn only_recording_streams_are_capture() {
        assert!(is_capture_class("Stream/Input/Audio"), "a microphone");
        assert!(is_capture_class("Stream/Input/Video"), "a camera");
        assert!(!is_capture_class("Stream/Output/Audio"), "playing a sound");
        assert!(!is_capture_class("Audio/Source"), "the microphone device");
    }

    #[test]
    fn only_a_change_in_which_capture_streams_run_is_news() {
        let mut streams = CaptureStreams::default();
        assert!(!streams.state(7, false), "a stream created paused");
        assert!(streams.state(7, true), "it starts recording");
        assert!(!streams.state(7, true), "the same state reported again");
        assert!(streams.state(8, true), "a second app records");
        assert_eq!(streams.running(), 2);
        assert!(streams.state(8, false), "it pauses");
        assert!(!streams.removed(8), "a paused stream ending");
        assert!(!streams.removed(9), "a client disconnecting");
        assert!(streams.removed(7), "a recording app quits");
        assert_eq!(streams.running(), 0);
    }

    #[cfg(not(all(feature = "pipewire", target_os = "linux")))]
    #[test]
    fn a_build_without_pipewire_reports_the_watch_unavailable() {
        assert!(watch(|_| {}).is_err());
    }
}
