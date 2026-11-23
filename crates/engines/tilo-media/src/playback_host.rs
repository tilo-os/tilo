//! One playback's outputs wherever they play: the computer's own sound devices and a television
//! reached by cast, behind the one [`OutputHost`] a [`Playout`](crate::playback::Playout) applies
//! its list to.
//!
//! A song and a video share this piece: the sinks hear the PCM the playback hands to
//! [`PlaybackHost::write`], and the cast session shows the stream the projector service sends.
//! Everything an output does without being asked - a speaker that stopped, a cast that dropped -
//! arrives through [`PlaybackHost::events`].

use tilo_clock::time::Instant;

use crate::audio::AudioGraph;
use crate::playback::{Output, OutputHost, OutputKind};
pub use crate::playback_cast::{CastHost, CastTransport, Receiver};
pub use crate::playback_sink::{HostEvent, SinkFactory, SinkHost, SinkStream};

/// A playback's sound devices and cast outputs.
pub struct PlaybackHost<S: SinkFactory, C: CastTransport> {
    sinks: SinkHost<S>,
    cast: CastHost<C>,
}

impl<S: SinkFactory, C: CastTransport> PlaybackHost<S, C> {
    #[must_use]
    pub fn new(sinks: S, cast: C) -> Self {
        Self {
            sinks: SinkHost::new(sinks),
            cast: CastHost::new(cast),
        }
    }

    /// One piece of the playback's sound, to every sound device, each with its own gain and
    /// distance behind the first.
    pub fn write(&mut self, samples: &[i16]) {
        self.sinks.write(samples);
    }

    /// Apply what the cast sessions reported and start again the ones whose retry is due.
    pub fn poll(&mut self) -> bool {
        self.cast.poll()
    }

    /// [`PlaybackHost::poll`] at `now`, which tests drive.
    pub fn poll_at(&mut self, now: Instant) -> bool {
        self.cast.poll_at(now)
    }

    /// What changed without being asked: outputs that stopped, and cast stages.
    pub fn events(&mut self) -> Vec<HostEvent> {
        let mut events = self.sinks.events();
        events.extend(self.cast.events());
        events
    }

    /// The sound devices playing.
    #[must_use]
    pub fn sinks(&self) -> &SinkHost<S> {
        &self.sinks
    }

    /// The receivers a person can cast to, for the picker.
    #[must_use]
    pub fn receivers(&self) -> &[Receiver] {
        self.cast.receivers()
    }

    /// Look for receivers again.
    ///
    /// # Errors
    /// What a person reads as the reason nothing was found.
    pub fn refresh_receivers(&mut self) -> Result<Vec<Receiver>, String> {
        self.cast.refresh()
    }

    /// Drop the sound the speakers stood behind: the playback jumped somewhere.
    pub fn flush(&mut self) {
        self.sinks.flush();
    }

    /// Follow the audio graph: the default output's change, and devices that went away.
    pub fn follow(&mut self, graph: &AudioGraph) {
        self.sinks.follow(graph);
    }

    /// The cast part, for a view that shows what is being cast.
    #[must_use]
    pub fn cast(&self) -> &CastHost<C> {
        &self.cast
    }
}

impl<S: SinkFactory, C: CastTransport> OutputHost for PlaybackHost<S, C> {
    fn start(&mut self, output: &Output) -> Result<(), String> {
        match &output.kind {
            OutputKind::Sink { .. } => self.sinks.start(output),
            OutputKind::Cast { .. } => self.cast.start(output),
        }
    }

    fn update(&mut self, output: &Output) -> Result<(), String> {
        match &output.kind {
            OutputKind::Sink { .. } => self.sinks.update(output),
            OutputKind::Cast { .. } => self.cast.update(output),
        }
    }

    fn stop(&mut self, id: &str) {
        self.sinks.stop(id);
        self.cast.stop(id);
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_lossless,
    clippy::similar_names,
    clippy::too_many_lines,
    reason = "test fixtures: a failed step is the assertion, and their counting cannot overflow"
)]
mod tests {
    use super::*;
    use crate::playback::{PlaybackOutputs, Playout};
    use crate::playback_cast::CastEvent;
    use crate::playback_sink::DEFAULT_SINK;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Devices {
        played: Mutex<BTreeMap<String, Vec<i16>>>,
        closed: Mutex<Vec<String>>,
    }

    struct Speakers(Arc<Devices>);

    impl SinkFactory for Speakers {
        fn open(&self, device: &str) -> Result<crate::playback_sink::OpenSink, String> {
            Ok(crate::playback_sink::OpenSink {
                device: device.to_owned(),
                stream: Box::new(Speaker {
                    chart: Arc::clone(&self.0),
                    device: device.to_owned(),
                }),
            })
        }
    }

    struct Speaker {
        chart: Arc<Devices>,
        device: String,
    }

    impl SinkStream for Speaker {
        fn write(&mut self, samples: &[i16]) -> Result<(), String> {
            self.chart
                .played
                .lock()
                .unwrap()
                .entry(self.device.clone())
                .or_default()
                .extend_from_slice(samples);
            Ok(())
        }

        fn close(&mut self) {
            self.chart.closed.lock().unwrap().push(self.device.clone());
        }
    }

    #[derive(Default)]
    struct Projector {
        receivers: Vec<Receiver>,
        refuse: Option<String>,
        begun: Mutex<Vec<String>>,
        ended: Mutex<Vec<String>>,
        queue: Mutex<Vec<(String, CastEvent)>>,
    }

    struct Casts(Arc<Projector>);

    impl CastTransport for Casts {
        fn discover(&mut self) -> Result<Vec<Receiver>, String> {
            Ok(self.0.receivers.clone())
        }
        fn begin(&mut self, cast: &str, receiver: &Receiver) -> Result<(), String> {
            if self.0.refuse.as_deref() == Some(receiver.id.as_str()) {
                return Err("The television refused the connection.".to_owned());
            }
            self.0.begun.lock().unwrap().push(cast.to_owned());
            Ok(())
        }
        fn end(&mut self, cast: &str) {
            self.0.ended.lock().unwrap().push(cast.to_owned());
        }
        fn events(&mut self) -> Vec<(String, CastEvent)> {
            std::mem::take(&mut *self.0.queue.lock().unwrap())
        }
    }

    fn sink(id: &str, device: &str, volume: f32) -> Output {
        Output {
            id: id.to_owned(),
            kind: OutputKind::Sink {
                device: device.to_owned(),
            },
            label: device.to_owned(),
            volume,
            sync_ms: 0,
            state: crate::playback::OutputState::Playing,
        }
    }

    fn television(target: &str) -> Output {
        Output {
            id: "tv".to_owned(),
            kind: OutputKind::Cast {
                protocol: "Google Cast".to_owned(),
                target: target.to_owned(),
            },
            label: "Living room".to_owned(),
            volume: 1.0,
            sync_ms: 0,
            state: crate::playback::OutputState::Connecting,
        }
    }

    fn receiver() -> Receiver {
        Receiver {
            id: "cast:living-room".to_owned(),
            name: "Living room".to_owned(),
            protocol: "Google Cast".to_owned(),
            detail: "192.0.2.20".to_owned(),
        }
    }

    #[test]
    fn a_playback_plays_on_two_speakers_and_a_television_and_a_moved_one_reaches_the_leader() {
        let devices = Arc::new(Devices::default());
        let frames = vec![900_i16; 960 * 2];
        let projector = Arc::new(Projector {
            receivers: vec![receiver()],
            ..Projector::default()
        });
        let mut playout = Playout::new(PlaybackHost::new(
            Speakers(Arc::clone(&devices)),
            Casts(Arc::clone(&projector)),
        ));
        let mut outputs = PlaybackOutputs::default();
        outputs.add(sink("speaker", "Built-in", 1.0));
        outputs.add(sink("desk", "Desk", 0.5));
        outputs.add(television("cast:living-room"));
        playout
            .apply(&outputs)
            .expect("two speakers and a television");

        playout.host_mut().write(&frames);
        assert_eq!(devices.played.lock().unwrap()["Built-in"], frames);
        assert_eq!(
            devices.played.lock().unwrap()["Desk"],
            vec![450_i16; frames.len()]
        );
        assert_eq!(projector.begun.lock().unwrap().clone(), ["tv"]);

        // The desk speaker becomes the one the others follow: the built-in one waits for it.
        let mut next = outputs.clone();
        let desk = next.get("desk").unwrap().clone();
        next.move_to(desk, 0);
        next.set_sync("speaker", 10);
        let mut patio = sink("patio", "Patio", 1.0);
        patio.sync_ms = 0;
        next.add(patio);
        playout.apply(&next).expect("the patio speaker starts");

        playout.host_mut().write(&[900_i16; 960 * 2]);
        let built_in = devices.played.lock().unwrap()["Built-in"].clone();
        let behind = audio_output_frames(10);
        assert_eq!(
            built_in[frames.len()..frames.len() + behind],
            vec![0_i16; behind],
            "the built-in speaker now stands behind the desk one"
        );
        assert_eq!(
            built_in[frames.len() + behind..],
            vec![900_i16; frames.len() - behind]
        );
        assert_eq!(
            devices.played.lock().unwrap().get("Patio").cloned(),
            Some(vec![900_i16; 960 * 2])
        );
    }

    #[test]
    fn a_refused_cast_is_said_while_the_speakers_play() {
        let devices = Arc::new(Devices::default());
        let projector = Arc::new(Projector {
            receivers: vec![receiver()],
            refuse: Some("cast:living-room".to_owned()),
            ..Projector::default()
        });
        let mut playout = Playout::new(PlaybackHost::new(
            Speakers(Arc::clone(&devices)),
            Casts(Arc::clone(&projector)),
        ));
        let mut outputs = PlaybackOutputs::default();
        outputs.add(sink("speaker", "Built-in", 1.0));
        outputs.add(television("cast:living-room"));
        let failed = playout.apply(&outputs).expect_err("the television refuses");
        assert_eq!(failed, ["tv: The television refused the connection."]);

        playout.host_mut().write(&[300_i16; 8]);
        assert_eq!(devices.played.lock().unwrap()["Built-in"], vec![300_i16; 8]);
    }

    #[test]
    fn ending_a_playback_closes_the_speakers_and_stops_casting() {
        let devices = Arc::new(Devices::default());
        let projector = Arc::new(Projector {
            receivers: vec![receiver()],
            ..Projector::default()
        });
        let mut playout = Playout::new(PlaybackHost::new(
            Speakers(Arc::clone(&devices)),
            Casts(Arc::clone(&projector)),
        ));
        let mut outputs = PlaybackOutputs::default();
        outputs.add(sink("speaker", "Built-in", 1.0));
        outputs.add(sink("desk", "Desk", 1.0));
        outputs.add(television("cast:living-room"));
        playout.apply(&outputs).expect("every output starts");
        drop(playout);

        let mut closed = devices.closed.lock().unwrap().clone();
        closed.sort();
        assert_eq!(closed, ["Built-in", "Desk"]);
        assert_eq!(projector.ended.lock().unwrap().clone(), ["tv"]);
    }

    #[test]
    fn an_output_named_for_the_default_speaker_is_a_sound_device() {
        let devices = Arc::new(Devices::default());
        let mut playout = Playout::new(PlaybackHost::new(
            Speakers(Arc::clone(&devices)),
            Casts(Arc::new(Projector::default())),
        ));
        let mut outputs = PlaybackOutputs::default();
        outputs.add(sink("sound", DEFAULT_SINK, 1.0));
        playout.apply(&outputs).expect("the default output opens");
        assert_eq!(playout.host().sinks().playing(), [("sound", DEFAULT_SINK)]);
    }

    /// The samples `ms` of stereo sound take at the crate's rate.
    fn audio_output_frames(ms: usize) -> usize {
        (crate::audio_output::RATE as usize / 1_000 * ms) * 2
    }
}
