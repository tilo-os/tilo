//! One-shot `PipeWire` registry custody for a call: the graph a person picks
//! a microphone from, and the exact connection `GStreamer` then runs that
//! microphone and the call's output on.
//!
//! The connection is the observer's own: it is opened, read for one registry
//! round trip and handed over, so no other reader of the same socket survives
//! and no stream reconnects by name behind the person's back.

#[cfg(target_os = "linux")]
use std::os::fd::OwnedFd;

/// What one observation saw: the audio nodes, the core's cookie, and the
/// connection a call's streams run on.
#[cfg(target_os = "linux")]
pub(crate) struct Observed {
    pub(crate) graph: media::AudioGraph,
    pub(crate) socket: OwnedFd,
}

#[cfg(not(target_os = "linux"))]
pub(crate) struct Observed;

#[cfg(not(target_os = "linux"))]
pub(crate) fn observe() -> Result<Observed, String> {
    Err("a call's audio devices require Linux".into())
}

/// Observe this device's audio graph and take its `PipeWire` connection, or
/// say why there is none: no graph, no microphones to offer.
#[cfg(target_os = "linux")]
#[allow(
    clippy::too_many_lines,
    reason = "one blocking PipeWire registry round trip whose callbacks share local state"
)]
pub(crate) fn observe() -> Result<Observed, String> {
    use pipewire as pw;
    use pw::types::ObjectType;
    use std::{
        cell::{Cell, RefCell},
        collections::BTreeMap,
        os::fd::FromRawFd as _,
        rc::Rc,
        time::{Duration, Instant},
    };
    let started = Instant::now();
    let end = started
        .checked_add(Duration::from_secs(1))
        .ok_or("PipeWire deadline overflow")?;
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|_| "PipeWire loop unavailable")?;
    let context =
        pw::context::ContextRc::new(&mainloop, None).map_err(|_| "PipeWire context unavailable")?;
    let core = context
        .connect_rc(None)
        .map_err(|_| "PipeWire observation unavailable")?;
    let registry = core
        .get_registry_rc()
        .map_err(|_| "PipeWire registry unavailable")?;
    let cookie = Rc::new(Cell::new(None));
    let failed = Rc::new(Cell::new(false));
    let completed = Rc::new(Cell::new(None));
    let nodes = Rc::new(RefCell::new(
        BTreeMap::<u32, (bool, media::AudioDevice)>::new(),
    ));
    let defaults = Rc::new(RefCell::new((None::<String>, None::<String>)));
    let metadata = Rc::new(RefCell::new(Vec::<(
        pw::metadata::Metadata,
        pw::metadata::MetadataListener,
    )>::new()));
    let core_listener = core
        .add_listener_local()
        .info({
            let cookie = cookie.clone();
            move |info| {
                cookie.set(Some(info.cookie()));
            }
        })
        .error({
            let failed = failed.clone();
            move |_, _, _, _| {
                failed.set(true);
            }
        })
        .done({
            let completed = completed.clone();
            move |id, seq| {
                if id == pw::core::PW_ID_CORE {
                    completed.set(Some(seq));
                }
            }
        })
        .register();
    let registry_listener = registry
        .add_listener_local()
        .global({
            let nodes = nodes.clone();
            let failed = failed.clone();
            let registry = registry.clone();
            let metadata = metadata.clone();
            let defaults = defaults.clone();
            move |global| {
                let Some(props) = global.props.as_ref() else {
                    return;
                };
                if global.type_ == ObjectType::Metadata
                    && props.get("metadata.name") == Some("default")
                {
                    if !metadata.borrow().is_empty() {
                        return;
                    }
                    let Ok(object) = registry.bind::<pw::metadata::Metadata, _>(global) else {
                        failed.set(true);
                        return;
                    };
                    let values = defaults.clone();
                    let listener = object
                        .add_listener_local()
                        .property(move |subject, key, _, value| {
                            if subject != 0 {
                                return 0;
                            }
                            let input = match key {
                                Some("default.audio.source") => true,
                                Some("default.audio.sink") => false,
                                _ => return 0,
                            };
                            let name = value
                                .filter(|value| value.len() <= 1024)
                                .and_then(|value| {
                                    // ignored-failure: a probe value that is not JSON names no device
                                    serde_json::from_str::<serde_json::Value>(value).ok()
                                })
                                .and_then(|value| {
                                    value
                                        .get("name")
                                        .and_then(|name| name.as_str())
                                        .filter(|name| name.len() <= 512)
                                        .map(str::to_owned)
                                });
                            let mut values = values.borrow_mut();
                            if input {
                                values.0 = name;
                            } else {
                                values.1 = name;
                            }
                            0
                        })
                        .register();
                    metadata.borrow_mut().push((object, listener));
                    return;
                }
                if global.type_ != ObjectType::Node {
                    return;
                }
                let input = match props.get("media.class") {
                    Some("Audio/Source") => true,
                    Some("Audio/Sink") => false,
                    _ => return,
                };
                let Some(serial) = props
                    .get("object.serial")
                    .and_then(|value| {
                        // ignored-failure: a probe value that does not parse names no device
                        value.parse::<u32>().ok()
                    })
                    .filter(|serial| *serial != 0)
                else {
                    return;
                };
                let Some(name) = props
                    .get("node.name")
                    .filter(|name| !name.is_empty() && name.len() <= 512)
                else {
                    return;
                };
                if name.starts_with(media::audio::ECHO_CANCEL_NODE_PREFIX) {
                    return;
                }
                let label = props.get("node.description").unwrap_or(name);
                let label: String = label
                    .chars()
                    .filter(|value| !value.is_control())
                    .take(64)
                    .collect();
                let mut nodes = nodes.borrow_mut();
                if nodes.len() >= 256 && !nodes.contains_key(&global.id) {
                    failed.set(true);
                    return;
                }
                nodes.insert(
                    global.id,
                    (
                        input,
                        media::AudioDevice {
                            id: global.id,
                            serial,
                            name: name.into(),
                            description: label,
                            is_default: false,
                            volume: None,
                            muted: None,
                            channels: 0,
                            codec: None,
                        },
                    ),
                );
            }
        })
        .global_remove({
            let nodes = nodes.clone();
            move |id| {
                nodes.borrow_mut().remove(&id);
            }
        })
        .register();
    let sync = |sequence| -> Result<(), String> {
        let pending = core
            .sync(sequence)
            .map_err(|_| "PipeWire synchronization failed")?;
        while completed.get() != Some(pending) {
            if failed.get() || Instant::now() >= end {
                return Err("PipeWire observation deadline".into());
            }
            if mainloop
                .loop_()
                .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(10)))
                < 0
            {
                return Err("PipeWire loop failed".into());
            }
        }
        if failed.get() {
            return Err("PipeWire observation failed".into());
        }
        Ok(())
    };
    sync(1)?;
    sync(2)?;
    let _ = cookie.get().ok_or("PipeWire core identity missing")?;
    let mut graph = media::AudioGraph::default();
    let selected = defaults.borrow();
    for (input, node) in nodes.borrow().values() {
        let mut node = node.clone();
        node.is_default =
            (if *input { &selected.0 } else { &selected.1 }).as_deref() == Some(node.name.as_str());
        (if *input {
            &mut graph.sources
        } else {
            &mut graph.sinks
        })
        .push(node);
    }
    drop(selected);
    if graph.sources.len() > 128 || graph.sinks.len() > 128 {
        return Err("PipeWire observation capacity".into());
    }
    drop(registry_listener);
    for (object, listener) in metadata.borrow_mut().drain(..) {
        drop(listener);
        drop(object);
    }
    drop(registry);
    sync(3)?;
    drop(core_listener);
    // SAFETY: this observer owns the connected core alone and has removed
    // every listener; PipeWire steals the protocol IO, not the allocation,
    // and the core is dropped below. This is the portal's own remote handoff.
    let raw = unsafe { pw::sys::pw_core_steal_fd(core.as_raw_ptr()) };
    if raw < 0 {
        return Err("PipeWire socket handoff unavailable".into());
    }
    // SAFETY: a successful steal returns a new owned descriptor exactly once.
    let socket = unsafe { OwnedFd::from_raw_fd(raw) };
    drop(core);
    Ok(Observed { graph, socket })
}

/// What a call's audio runs on: this machine's speakers and microphones, or,
/// in tests, the same pipeline ending in memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Audio {
    Devices,
    /// A tone instead of a microphone, and the played samples kept instead
    /// of reaching a speaker.
    #[cfg(test)]
    Software,
}

/// A microphone this call may send: what the person picks between.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Source {
    pub(crate) id: String,
    pub(crate) label: String,
    /// The node and its stable serial, as the observation named them.
    pub(crate) node: u32,
    pub(crate) serial: u32,
}

/// One frame this call's microphone captured.
pub(crate) struct Frame {
    pub(crate) timestamp_48k: u64,
    pub(crate) payload: Vec<u8>,
}

/// One call's audio: the microphone it sends and what it plays, on one
/// observation of the device graph.
pub(crate) struct Voice(inner::Inner);

#[cfg(target_os = "linux")]
mod inner {
    use std::sync::Arc;
    use std::time::Instant;

    use media::voice::gst::{CaptureSource, ObservedOutput, VoiceAudio};
    use media::voice::{PublicationId, ReceivedOpus};

    use super::{Audio, Frame, Observed, Source};
    use runtime_collab::native_media::Issue;

    pub(super) struct Inner {
        audio: Backing,
        /// The microphones this call may send.
        sources: Vec<Source>,
        /// The graph this call's streams were opened from: the connection its
        /// microphone and output run on.
        observed: Option<Observed>,
    }

    enum Backing {
        Devices(VoiceAudio),
        #[cfg(test)]
        Software(media::voice::gst::software::SoftwareOutput),
    }

    impl Inner {
        fn audio(&mut self) -> &mut VoiceAudio {
            match &mut self.audio {
                Backing::Devices(audio) => audio,
                #[cfg(test)]
                Backing::Software(output) => output.audio(),
            }
        }
    }

    /// The voice `sender`'s frames are played as: one publisher per device of
    /// the call, at the one carrier generation this call's lane is.
    fn publisher(sender: [u8; 32]) -> PublicationId {
        PublicationId {
            namespace: hex::encode(sender),
            carrier_generation: 1,
            incarnation: 1,
        }
    }

    /// The output the call plays through: the person's default sink, or the
    /// only one there is.
    fn sink_of(observed: &Observed) -> Option<(u32, u32)> {
        observed
            .graph
            .default_sink()
            .or_else(|| {
                (observed.graph.sinks.len() == 1)
                    .then(|| observed.graph.sinks.first())
                    .flatten()
            })
            .map(|sink| (sink.id, sink.serial))
    }

    impl super::Voice {
        pub(crate) fn open(audio: Audio) -> Result<Self, Issue> {
            match audio {
                Audio::Devices => {
                    let observed = super::observe().map_err(|_| Issue::Unavailable)?;
                    let (node, serial) = sink_of(&observed).ok_or(Issue::Unavailable)?;
                    let played = VoiceAudio::new(ObservedOutput {
                        node,
                        serial,
                        remote_fd: std::os::fd::AsFd::as_fd(&observed.socket),
                    })
                    .map_err(|_| Issue::Unavailable)?;
                    let sources = observed
                        .graph
                        .sources
                        .iter()
                        .map(|source| Source {
                            id: source.serial.to_string(),
                            label: source.description.clone(),
                            node: source.id,
                            serial: source.serial,
                        })
                        .collect();
                    Ok(Self(Inner {
                        audio: Backing::Devices(played),
                        sources,
                        observed: Some(observed),
                    }))
                }
                #[cfg(test)]
                Audio::Software => {
                    let output = media::voice::gst::software::SoftwareOutput::start()
                        .map_err(|_| Issue::Unavailable)?;
                    Ok(Self(Inner {
                        audio: Backing::Software(output),
                        sources: vec![Source {
                            id: "tone".to_owned(),
                            label: "Test tone".to_owned(),
                            node: 0,
                            serial: 0,
                        }],
                        observed: None,
                    }))
                }
            }
        }

        pub(crate) fn sources(&self) -> Vec<Source> {
            self.0.sources.clone()
        }

        pub(crate) fn start(
            &mut self,
            source: &Source,
            still_current: Arc<dyn Fn() -> bool + Send + Sync>,
        ) -> Result<(), Issue> {
            match &mut self.0.audio {
                Backing::Devices(audio) => {
                    let observed = self.0.observed.as_ref().ok_or(Issue::Unavailable)?;
                    audio
                        .start_capture(
                            CaptureSource::PipeWire {
                                node: source.node,
                                serial: source.serial,
                                remote_fd: std::os::fd::AsFd::as_fd(&observed.socket),
                            },
                            still_current,
                        )
                        .map_err(|_| Issue::CaptureFailed)
                }
                #[cfg(test)]
                Backing::Software(output) => output
                    .audio()
                    .start_software_capture(440.0, still_current)
                    .map_err(|_| Issue::CaptureFailed),
            }
        }

        pub(crate) fn stop(&mut self) {
            self.0.audio().stop_capture();
        }

        pub(crate) fn set_deafened(&mut self, deafened: bool) {
            self.0.audio().set_deafened(deafened);
        }

        /// The frames the microphone captured since the last pump.
        pub(crate) fn pump(&mut self, now: Instant) -> Result<Vec<Frame>, Issue> {
            let frames = self
                .0
                .audio()
                .poll_capture(now)
                .map_err(|_| Issue::SourceLost)?;
            Ok(frames
                .into_iter()
                .map(|frame| Frame {
                    timestamp_48k: frame.timestamp_48k,
                    payload: frame.payload,
                })
                .collect())
        }

        pub(crate) fn playout(&mut self, now: Instant) -> Result<(), Issue> {
            self.0
                .audio()
                .playout(now)
                .map_err(|_| Issue::TransportFailed)
        }

        /// Play one frame heard from `sender`.
        pub(crate) fn receive(
            &mut self,
            sender: [u8; 32],
            sequence: u64,
            timestamp_48k: u64,
            payload: Vec<u8>,
            now: Instant,
        ) -> Result<(), Issue> {
            let lifetime = std::time::Duration::from_millis(200);
            self.0
                .audio()
                .receive(
                    ReceivedOpus {
                        publisher: publisher(sender),
                        sequence,
                        timestamp_48k,
                        duration_samples: 960,
                        payload,
                        received_at: now,
                        expires_at: now.checked_add(lifetime).unwrap_or(now),
                    },
                    now,
                )
                .map_err(|_| Issue::TransportFailed)
        }

        /// Forget what was heard from `sender`: a lane that ended, or a stream
        /// that starts over on a fresh one, is heard at its own place from its
        /// first frame on, rather than behind where the one before it got.
        pub(crate) fn forget(&mut self, sender: [u8; 32]) {
            self.0.audio().remove_publisher(&publisher(sender));
        }

        /// The samples played since this was last called, where the call
        /// plays into memory.
        #[cfg(test)]
        pub(crate) fn played(&mut self) -> Vec<f32> {
            match &mut self.0.audio {
                Backing::Devices(_) => Vec::new(),
                #[cfg(test)]
                Backing::Software(output) => output.played(),
            }
        }

        pub(crate) fn close(&mut self) {
            self.0.audio().close();
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod inner {
    use std::sync::Arc;
    use std::time::Instant;

    use super::{Audio, Frame, Source};
    use runtime_collab::native_media::Issue;

    pub(super) struct Inner;

    impl super::Voice {
        pub(crate) fn open(_audio: Audio) -> Result<Self, Issue> {
            Err(Issue::Unsupported)
        }
        pub(crate) fn sources(&self) -> Vec<Source> {
            Vec::new()
        }
        pub(crate) fn start(
            &mut self,
            _source: &Source,
            _still_current: Arc<dyn Fn() -> bool + Send + Sync>,
        ) -> Result<(), Issue> {
            Err(Issue::Unsupported)
        }
        pub(crate) fn stop(&mut self) {}
        pub(crate) fn set_deafened(&mut self, _deafened: bool) {}
        pub(crate) fn pump(&mut self, _now: Instant) -> Result<Vec<Frame>, Issue> {
            Ok(Vec::new())
        }
        pub(crate) fn playout(&mut self, _now: Instant) -> Result<(), Issue> {
            Ok(())
        }
        pub(crate) fn receive(
            &mut self,
            _sender: [u8; 32],
            _sequence: u64,
            _timestamp_48k: u64,
            _payload: Vec<u8>,
            _now: Instant,
        ) -> Result<(), Issue> {
            Ok(())
        }
        #[cfg(test)]
        pub(crate) fn played(&mut self) -> Vec<f32> {
            Vec::new()
        }
        pub(crate) fn forget(&mut self, _sender: [u8; 32]) {}
        pub(crate) fn close(&mut self) {}
    }
}
