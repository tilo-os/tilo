//! A tl Playback's `outputs` Collection as the host plays it.
//!
//! A tilolang app keeps where its sound and picture come out as a `Collection<Output>` of the
//! playback package: one item per place, in the order the person put them. The app's window
//! process holds a [`PlaybackBridge`] and hands it that Collection whenever it changed, so what
//! the person added plays on the computer's own devices ([`crate::playback_sink`]) and casts to
//! a television ([`crate::playback_cast`]) through the one [`PlaybackHost`].
//!
//! The item's id in its Collection is the output's stable id: a move is the same output standing
//! somewhere else, never a second one. [`outputs_of`] reads the Collection the way the session's
//! actions write it, and [`PlaybackBridge::apply`] applies it to the host by diff, so an output
//! that only turned down is not started again.

use crate::playback::{Output, OutputKind, OutputState, PlaybackOutputs, Playout};
use crate::playback_cast::CastTransport;
use crate::playback_host::PlaybackHost;
use crate::playback_sink::{DEFAULT_SINK, SinkFactory};

/// One place a session plays, as the app's `outputs` Collection holds it: the item's id, and the
/// `Output` type's fields.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CollectionOutput {
    /// The Collection item's id: the output's id while it lasts (`<collection>/<item>`).
    pub id: String,
    /// `local`, `speaker` or `cast`, as the app names it.
    #[serde(default)]
    pub kind: String,
    /// The device, speaker or target as the system names it; empty for this device.
    #[serde(default)]
    pub device: String,
    /// What people read of it; empty for this device.
    #[serde(default)]
    pub name: String,
    /// `0.0..=1.0`, as the app keeps it.
    #[serde(default)]
    pub volume: f32,
    /// Whether it plays in step with the session's place.
    #[serde(default = "synced_by_default")]
    pub synced: bool,
}

fn synced_by_default() -> bool {
    true
}

impl CollectionOutput {
    /// What a person reads this place as: the name they gave it, else the device or target it
    /// names, else this device.
    #[must_use]
    pub fn label(&self) -> String {
        if !self.name.is_empty() {
            self.name.clone()
        } else if !self.device.is_empty() {
            self.device.clone()
        } else {
            "This device".to_owned()
        }
    }

    /// This output as the host plays it. `local` is whatever the computer plays through now,
    /// which [`SinkHost`](crate::playback_sink::SinkHost) resolves and follows; a speaker names
    /// its device (or its own description, which the audio graph resolves too); a cast output
    /// names its receiver in `device`.
    #[must_use]
    pub fn output(&self) -> Output {
        let kind = match self.kind.as_str() {
            "speaker" => OutputKind::Sink {
                device: if self.device.is_empty() {
                    self.name.clone()
                } else {
                    self.device.clone()
                },
            },
            "cast" => OutputKind::Cast {
                protocol: "Cast".to_owned(),
                target: if self.device.is_empty() {
                    self.name.clone()
                } else {
                    self.device.clone()
                },
            },
            // This device: the session names its own output `local`, and the host resolves it to
            // the computer's default, following it when it changes.
            _ => OutputKind::Sink {
                device: DEFAULT_SINK.to_owned(),
            },
        };
        Output {
            id: self.id.clone(),
            kind,
            label: if self.name.is_empty() {
                self.device.clone()
            } else {
                self.name.clone()
            },
            volume: self.volume.clamp(0.0, 1.0),
            // A cast target stands behind the speakers by its own latency, which the service
            // reports; a synced output stands with the first.
            sync_ms: 0,
            state: OutputState::Connecting,
        }
    }
}

/// The outputs `outputs` lists, in the order the Collection holds them: the item ids are the
/// outputs' ids, and each item's place is where it stands (a Collection move is the same output
/// moved, never added again).
#[must_use]
pub fn outputs_of(outputs: &[CollectionOutput]) -> PlaybackOutputs {
    let mut list = PlaybackOutputs::default();
    for (at, output) in outputs.iter().enumerate() {
        let output = output.output();
        // Each output stands where the Collection has it now: a move is the same output at
        // another place, never a second one, so dragging it in front of another makes it the one
        // that leads.
        list.move_to(output, at);
    }
    list
}

/// One playback's places, applied to the host that really plays them as the app's Collection
/// changes.
pub struct PlaybackBridge<S: SinkFactory, C: CastTransport> {
    playout: Playout<PlaybackHost<S, C>>,
}

impl<S: SinkFactory, C: CastTransport> PlaybackBridge<S, C> {
    #[must_use]
    pub fn new(sinks: S, cast: C) -> Self {
        Self {
            playout: Playout::new(PlaybackHost::new(sinks, cast)),
        }
    }

    /// Move the host to `outputs`: what the app's Collection holds now. Started places play,
    /// moved ones keep their stream, and taken-out ones stop.
    ///
    /// # Errors
    /// The places that would not start, each said with why. The others play.
    pub fn apply(&mut self, outputs: &[CollectionOutput]) -> Result<(), Vec<String>> {
        self.playout.apply(&outputs_of(outputs))
    }

    /// One piece of the playback's sound, to every place playing.
    pub fn write(&mut self, samples: &[i16]) {
        self.playout.host_mut().write(samples);
    }

    /// What changed without being asked: a speaker that stopped, a cast that dropped.
    pub fn events(&mut self) -> Vec<crate::playback_sink::HostEvent> {
        self.playout.host_mut().events()
    }

    /// Apply what the cast sessions reported and start again the ones whose retry is due.
    pub fn poll(&mut self) -> bool {
        self.playout.host_mut().poll()
    }

    /// The outputs the host holds now.
    #[must_use]
    pub fn current(&self) -> &PlaybackOutputs {
        self.playout.current()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::similar_names,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed step is the assertion, and their counting cannot overflow"
)]
mod tests {
    use super::*;
    use crate::playback_cast::{CastEvent, Receiver};
    use crate::playback_sink::{OpenSink, SinkStream};
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Devices {
        played: Mutex<BTreeMap<String, Vec<i16>>>,
        closed: Mutex<Vec<String>>,
    }

    struct Speakers(Arc<Devices>);

    impl SinkFactory for Speakers {
        fn open(&self, device: &str) -> Result<OpenSink, String> {
            Ok(OpenSink {
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
    }

    struct Casts(Arc<Projector>);

    impl CastTransport for Casts {
        fn discover(&mut self) -> Result<Vec<Receiver>, String> {
            Ok(self.0.receivers.clone())
        }
        fn begin(&mut self, _cast: &str, receiver: &Receiver) -> Result<(), String> {
            if self.0.refuse.as_deref() == Some(receiver.id.as_str()) {
                return Err("The television refused the connection.".to_owned());
            }
            Ok(())
        }
        fn end(&mut self, _cast: &str) {}
        fn events(&mut self) -> Vec<(String, CastEvent)> {
            Vec::new()
        }
    }

    fn speaker(id: &str, device: &str, volume: f32) -> CollectionOutput {
        CollectionOutput {
            id: id.to_owned(),
            kind: "speaker".to_owned(),
            device: device.to_owned(),
            name: device.to_owned(),
            volume,
            synced: true,
        }
    }

    fn local() -> CollectionOutput {
        CollectionOutput {
            id: "outputs/1".to_owned(),
            kind: "local".to_owned(),
            device: String::new(),
            name: String::new(),
            volume: 1.0,
            synced: true,
        }
    }

    fn television() -> CollectionOutput {
        CollectionOutput {
            id: "outputs/2".to_owned(),
            kind: "cast".to_owned(),
            device: "cast:living-room".to_owned(),
            name: "Living room".to_owned(),
            volume: 1.0,
            synced: false,
        }
    }

    #[test]
    fn two_speakers_both_receive_and_a_move_keeps_them_the_same_ones() {
        let devices = Arc::new(Devices::default());
        let mut bridge = PlaybackBridge::new(
            Speakers(Arc::clone(&devices)),
            Casts(Arc::new(Projector::default())),
        );
        let frames = vec![900_i16; 960 * 2];
        let mut outputs = vec![
            speaker("outputs/1", "Built-in", 1.0),
            speaker("outputs/2", "Desk", 0.5),
        ];
        bridge.apply(&outputs).expect("both speakers start");
        bridge.write(&frames);
        assert_eq!(devices.played.lock().unwrap()["Built-in"], frames);
        assert_eq!(
            devices.played.lock().unwrap()["Desk"],
            vec![450_i16; frames.len()]
        );

        // The person drags the desk speaker in front of the built-in one: the same two outputs,
        // each is moved rather than started again, and both still play.
        outputs.reverse();
        bridge.apply(&outputs).expect("the moved list applies");
        assert_eq!(
            bridge
                .current()
                .outputs()
                .iter()
                .map(|output| output.id.as_str())
                .collect::<Vec<_>>(),
            ["outputs/2", "outputs/1"]
        );
        bridge.write(&frames);
        assert_eq!(
            devices.played.lock().unwrap()["Built-in"],
            vec![900_i16; frames.len() * 2]
        );
        assert_eq!(
            devices.played.lock().unwrap().get("Desk").map(Vec::len),
            Some(frames.len() * 2)
        );
    }

    #[test]
    fn a_refused_cast_is_said_while_the_speakers_keep_playing() {
        let devices = Arc::new(Devices::default());
        let projector = Arc::new(Projector {
            receivers: vec![Receiver {
                id: "cast:living-room".to_owned(),
                name: "Living room".to_owned(),
                protocol: "Google Cast".to_owned(),
                detail: "192.0.2.20".to_owned(),
            }],
            refuse: Some("cast:living-room".to_owned()),
        });
        let mut bridge = PlaybackBridge::new(Speakers(Arc::clone(&devices)), Casts(projector));
        let outputs = vec![local(), television()];
        let failed = bridge.apply(&outputs).expect_err("the television refuses");
        assert_eq!(
            failed,
            ["outputs/2: The television refused the connection."]
        );

        bridge.write(&[300_i16; 8]);
        assert_eq!(devices.played.lock().unwrap()["default"], vec![300_i16; 8]);
    }

    #[test]
    fn a_taken_out_place_stops_and_the_rest_keep_playing() {
        let devices = Arc::new(Devices::default());
        let mut bridge = PlaybackBridge::new(
            Speakers(Arc::clone(&devices)),
            Casts(Arc::new(Projector::default())),
        );
        bridge
            .apply(&[local(), speaker("outputs/2", "Desk", 1.0)])
            .expect("both start");
        bridge.apply(&[local()]).expect("the desk speaker goes");
        let mut closed = devices.closed.lock().unwrap().clone();
        closed.sort();
        assert_eq!(closed, ["Desk"]);
        bridge.write(&[1_i16; 4]);
        assert_eq!(devices.played.lock().unwrap()["default"], vec![1_i16; 4]);
    }
}
