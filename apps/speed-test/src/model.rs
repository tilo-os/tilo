//! What the app knows about the running and finished tests, independent of
//! how it is drawn: the phase, the readings measured so far, the recent
//! results, and the dial's reading.

use std::sync::mpsc::{self, Receiver};

use ui::time::{Duration, Instant};

use crate::measure::{self, WorkerMessage};

/// How many finished tests the app remembers.
const HISTORY_LEN: usize = 5;

/// How long the dial's arc takes to close most of the distance to its target.
const RING_SETTLE: Duration = Duration::from_millis(110);

/// The largest step of one animation frame, so a stalled frame does not jump.
const MAX_FRAME: f64 = 0.05;

/// Where the dial reads full on its speed scale, in Mbps.
const SPEED_SCALE_MBPS: u32 = 1000;

/// Where the dial reads full on its latency scale, in milliseconds.
const PING_SCALE_MS: u32 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Idle,
    Ping,
    Download,
    Upload,
    Complete,
    Error,
}

impl Phase {
    pub(crate) fn is_running(self) -> bool {
        matches!(self, Self::Ping | Self::Download | Self::Upload)
    }
}

/// What the current test has measured so far. A field is `None` until its
/// stage has finished.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Readings {
    pub ping: Option<f64>,
    pub jitter: Option<f64>,
    pub download: Option<f64>,
    pub upload: Option<f64>,
    /// Bytes moved by the transfers so far.
    pub bytes: u64,
    /// Where the test server answered from, when it said.
    pub location: Option<String>,
}

/// A finished test.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Finished {
    /// Seconds since the Unix epoch.
    pub at: i64,
    pub ping: f64,
    pub jitter: f64,
    pub download: f64,
    pub upload: f64,
}

/// How good the connection is for one kind of use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Quality {
    Excellent,
    Good,
    Limited,
}

impl Quality {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Excellent => "Excellent",
            Self::Good => "Good",
            Self::Limited => "Limited",
        }
    }
}

/// What a finished test supports: video calls, 4K streaming, cloud gaming.
pub(crate) fn qualities(result: &Finished) -> [(&'static str, Quality); 3] {
    let tier = |excellent: bool, good: bool| {
        if excellent {
            Quality::Excellent
        } else if good {
            Quality::Good
        } else {
            Quality::Limited
        }
    };
    [
        (
            "Video calls",
            tier(
                result.upload >= 8.0 && result.ping <= 80.0,
                result.upload >= 3.0,
            ),
        ),
        (
            "4K streaming",
            tier(result.download >= 50.0, result.download >= 25.0),
        ),
        (
            "Cloud gaming",
            tier(
                result.ping <= 20.0 && result.jitter <= 5.0,
                result.ping <= 50.0,
            ),
        ),
    ]
}

/// The dial's reading: how far the arc runs, and the number in its middle.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DialReading {
    pub frac: f64,
    pub value: String,
    pub unit: &'static str,
}

pub(crate) struct Model {
    pub phase: Phase,
    pub readings: Readings,
    /// The reading of the stage in progress.
    live: f64,
    /// Finished tests, newest first.
    pub history: Vec<Finished>,
    pub error: Option<String>,
    /// How far the dial's arc has eased toward its target.
    ring: f64,
    last_frame: Instant,
    worker: Option<Receiver<WorkerMessage>>,
    task: Option<ui::rt::JoinHandle<()>>,
    /// Bumped whenever the shape of the screen changes: the phase, a stage's
    /// result, the history. Live readings do not bump it.
    revision: u64,
}

impl Model {
    pub(crate) fn new() -> Self {
        Self {
            phase: Phase::Idle,
            readings: Readings::default(),
            live: 0.0,
            history: Vec::new(),
            error: None,
            ring: 0.0,
            last_frame: ui::time::now(),
            worker: None,
            task: None,
            revision: 0,
        }
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    pub(crate) fn is_running(&self) -> bool {
        self.phase.is_running()
    }

    pub(crate) fn start(&mut self) {
        if self.is_running() {
            return;
        }
        self.cancel();
        self.phase = Phase::Ping;
        self.readings = Readings::default();
        self.error = None;
        self.live = 0.0;
        self.last_frame = ui::time::now();
        let (tx, rx) = mpsc::channel();
        self.worker = Some(rx);
        self.task = Some(ui::rt::app().spawn(measure::run(tx)));
        self.changed();
    }

    pub(crate) fn stop(&mut self) {
        if !self.is_running() {
            return;
        }
        self.cancel();
        self.phase = Phase::Idle;
        self.readings = Readings::default();
        self.live = 0.0;
        self.changed();
    }

    pub(crate) fn toggle(&mut self) {
        if self.is_running() {
            self.stop();
        } else {
            self.start();
        }
    }

    fn cancel(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.worker = None;
    }

    /// Take in what the worker has reported. Returns whether anything changed.
    pub(crate) fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Some(message) = self.worker.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.apply(message);
            changed = true;
        }
        changed
    }

    pub(crate) fn apply(&mut self, message: WorkerMessage) {
        match message {
            WorkerMessage::Ping { sample } => self.live = sample,
            WorkerMessage::Latency {
                ping,
                jitter,
                location,
            } => {
                self.readings.ping = Some(ping);
                self.readings.jitter = Some(jitter);
                self.readings.location = location;
                self.live = 0.0;
                self.phase = Phase::Download;
                self.changed();
            }
            WorkerMessage::Download { live } | WorkerMessage::Upload { live } => self.live = live,
            WorkerMessage::DownloadDone { mbps, bytes } => {
                self.readings.download = Some(mbps);
                self.readings.bytes = self.readings.bytes.saturating_add(bytes);
                self.live = 0.0;
                self.phase = Phase::Upload;
                self.changed();
            }
            WorkerMessage::Done { mbps, bytes } => {
                self.readings.upload = Some(mbps);
                self.readings.bytes = self.readings.bytes.saturating_add(bytes);
                self.worker = None;
                self.task = None;
                self.phase = Phase::Complete;
                if let Some(result) = self.finished() {
                    self.history.insert(0, result);
                    self.history.truncate(HISTORY_LEN);
                }
                self.changed();
            }
            WorkerMessage::Failed(message) => {
                self.worker = None;
                self.task = None;
                self.error = Some(message);
                self.phase = Phase::Error;
                self.changed();
            }
        }
    }

    /// The finished test the readings add up to, once every stage is in.
    fn finished(&self) -> Option<Finished> {
        let readings = &self.readings;
        Some(Finished {
            at: ui::clock::local_now().timestamp(),
            ping: readings.ping?,
            jitter: readings.jitter?,
            download: readings.download?,
            upload: readings.upload?,
        })
    }

    /// The reading the dial's arc is heading for.
    fn target(&self) -> f64 {
        match self.phase {
            Phase::Idle | Phase::Error => 0.0,
            Phase::Ping => (self.live / f64::from(PING_SCALE_MS)).clamp(0.0, 1.0),
            Phase::Download | Phase::Upload => speed_fraction(self.live),
            Phase::Complete => speed_fraction(self.readings.download.unwrap_or(0.0)),
        }
    }

    /// Ease the arc toward its target. Returns whether it moved.
    pub(crate) fn tick(&mut self, now: Instant) -> bool {
        let delta = now
            .duration_since(self.last_frame)
            .as_secs_f64()
            .min(MAX_FRAME);
        self.last_frame = now;
        let before = self.ring;
        self.ring +=
            (self.target() - self.ring) * (1.0 - (-delta / RING_SETTLE.as_secs_f64()).exp());
        (before - self.ring).abs() > 0.0005
    }

    /// Whether frames are still needed: a test is running or the arc has
    /// not settled.
    pub(crate) fn is_animating(&self) -> bool {
        self.is_running() || (self.ring - self.target()).abs() > 0.001
    }

    pub(crate) fn dial(&self) -> DialReading {
        let (value, unit) = match self.phase {
            Phase::Idle | Phase::Error => ("—".to_owned(), ""),
            Phase::Ping => (format!("{:.0}", self.live), "ms"),
            Phase::Download | Phase::Upload => (format!("{:.1}", self.live), "Mbps"),
            Phase::Complete => (
                self.readings
                    .download
                    .map_or_else(|| "—".to_owned(), |value| format!("{value:.1}")),
                "Mbps",
            ),
        };
        DialReading {
            frac: self.ring,
            value,
            unit,
        }
    }

    /// The stage the stepper is on; all three are done once complete.
    pub(crate) fn stage(&self) -> usize {
        match self.phase {
            Phase::Idle | Phase::Error | Phase::Ping => 0,
            Phase::Download => 1,
            Phase::Upload => 2,
            Phase::Complete => 3,
        }
    }

    /// The most recent finished test, while it is the one on screen.
    pub(crate) fn latest(&self) -> Option<&Finished> {
        (self.phase == Phase::Complete)
            .then(|| self.history.first())
            .flatten()
    }

    /// Put the dial exactly where it belongs, without easing.
    pub(crate) fn settle(&mut self) {
        self.ring = self.target();
    }

    /// Stage a screenshot scenario: the state a person would have reached.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        self.cancel();
        let now = ui::clock::local_now().timestamp();
        self.readings = Readings::default();
        self.error = None;
        self.live = 0.0;
        self.history = Vec::new();
        self.phase = Phase::Idle;
        let earlier = [
            Finished {
                at: now - 2 * 3600,
                ping: 12.4,
                jitter: 1.8,
                download: 618.2,
                upload: 112.9,
            },
            Finished {
                at: now - 26 * 3600,
                ping: 14.1,
                jitter: 2.2,
                download: 584.6,
                upload: 109.3,
            },
        ];
        match scenario {
            Some("running") => {
                self.phase = Phase::Download;
                self.readings.ping = Some(11.8);
                self.readings.jitter = Some(1.6);
                self.live = 386.4;
                self.history = earlier.to_vec();
            }
            Some("error") => {
                self.phase = Phase::Error;
                self.error =
                    Some("Could not reach the test server: the connection timed out.".to_owned());
                self.history = earlier.to_vec();
            }
            Some("complete-slow") => {
                self.phase = Phase::Complete;
                self.readings = Readings {
                    ping: Some(64.0),
                    jitter: Some(9.3),
                    download: Some(18.6),
                    upload: Some(4.2),
                    bytes: 8_000_000,
                    location: Some("Frankfurt".to_owned()),
                };
                let mut history = vec![Finished {
                    at: now,
                    ping: 64.0,
                    jitter: 9.3,
                    download: 18.6,
                    upload: 4.2,
                }];
                history.extend(earlier);
                self.history = history;
            }
            Some(name) if name.starts_with("complete") => {
                self.phase = Phase::Complete;
                self.readings = Readings {
                    ping: Some(11.8),
                    jitter: Some(1.6),
                    download: Some(642.7),
                    upload: Some(118.4),
                    bytes: 12_000_000,
                    location: Some("Frankfurt".to_owned()),
                };
                let mut history = vec![Finished {
                    at: now,
                    ping: 11.8,
                    jitter: 1.6,
                    download: 642.7,
                    upload: 118.4,
                }];
                history.extend(earlier);
                self.history = history;
            }
            _ => {}
        }
        self.settle();
        self.changed();
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// How far a speed reads along the dial: logarithmic, so a slow line and a
/// fast one both move the arc.
pub(crate) fn speed_fraction(mbps: f64) -> f64 {
    ((1.0 + mbps.max(0.0)).ln() / (1.0 + f64::from(SPEED_SCALE_MBPS)).ln()).clamp(0.0, 1.0)
}

/// "12 MB", from a byte count.
#[allow(
    clippy::cast_precision_loss,
    reason = "a data total of a few megabytes is exact in f64"
)]
pub(crate) fn data_label(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / 1_000_000.0)
}

/// How long ago a test finished.
pub(crate) fn age_label(now: i64, at: i64) -> String {
    let seconds = (now - at).max(0);
    match seconds {
        0..60 => "Just now".to_owned(),
        60..3600 => format!("{} min ago", seconds / 60),
        3600..86_400 => format!("{} h ago", seconds / 3600),
        86_400..172_800 => "Yesterday".to_owned(),
        _ => format!("{} days ago", seconds / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latency() -> WorkerMessage {
        WorkerMessage::Latency {
            ping: 12.0,
            jitter: 1.5,
            location: Some("Frankfurt".to_owned()),
        }
    }

    #[test]
    fn idle_dial_is_empty_with_no_arc() {
        let model = Model::new();
        let dial = model.dial();
        assert_eq!(dial.value, "—");
        assert!(dial.frac.abs() < f64::EPSILON, "no stub arc at idle");
        assert!(!model.is_animating());
    }

    #[test]
    fn ping_is_known_as_soon_as_latency_finishes() {
        let mut model = Model::new();
        model.phase = Phase::Ping;
        model.apply(WorkerMessage::Ping { sample: 13.0 });
        assert_eq!(model.readings.ping, None, "a sample is not yet the result");
        model.apply(latency());
        assert_eq!(model.phase, Phase::Download);
        assert_eq!(model.readings.ping, Some(12.0));
        assert_eq!(model.readings.jitter, Some(1.5));
        assert_eq!(model.readings.download, None);
    }

    #[test]
    fn a_full_run_lands_in_the_history() {
        let mut model = Model::new();
        model.phase = Phase::Ping;
        model.apply(latency());
        model.apply(WorkerMessage::DownloadDone {
            mbps: 300.0,
            bytes: 10_000_000,
        });
        assert_eq!(model.phase, Phase::Upload);
        assert_eq!(model.stage(), 2);
        model.apply(WorkerMessage::Done {
            mbps: 40.0,
            bytes: 2_000_000,
        });
        assert_eq!(model.phase, Phase::Complete);
        assert_eq!(model.readings.bytes, 12_000_000);
        assert_eq!(model.history.len(), 1);
        assert_eq!(model.latest().map(|r| r.download), Some(300.0));
    }

    #[test]
    fn failure_keeps_the_reason_and_clears_the_dial() {
        let mut model = Model::new();
        model.phase = Phase::Ping;
        model.apply(WorkerMessage::Failed("offline".to_owned()));
        assert_eq!(model.phase, Phase::Error);
        assert_eq!(model.error.as_deref(), Some("offline"));
        assert_eq!(model.dial().value, "—");
    }

    #[test]
    fn history_keeps_the_newest_few() {
        let mut model = Model::new();
        for index in 0..8 {
            model.phase = Phase::Ping;
            model.readings = Readings::default();
            model.apply(latency());
            model.apply(WorkerMessage::DownloadDone {
                mbps: f64::from(index),
                bytes: 1,
            });
            model.apply(WorkerMessage::Done {
                mbps: 1.0,
                bytes: 1,
            });
        }
        assert_eq!(model.history.len(), HISTORY_LEN);
        assert_eq!(model.history.first().map(|r| r.download), Some(7.0));
    }

    #[test]
    fn the_speed_scale_is_monotonic_and_bounded() {
        assert!(speed_fraction(0.0).abs() < f64::EPSILON);
        assert!(speed_fraction(10.0) < speed_fraction(100.0));
        assert!(speed_fraction(100.0) < speed_fraction(900.0));
        assert!((speed_fraction(50_000.0) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn quality_follows_the_measured_numbers() {
        let fast = Finished {
            at: 0,
            ping: 11.0,
            jitter: 1.0,
            download: 600.0,
            upload: 100.0,
        };
        assert!(
            qualities(&fast)
                .iter()
                .all(|(_, q)| *q == Quality::Excellent)
        );
        let slow = Finished {
            at: 0,
            ping: 120.0,
            jitter: 20.0,
            download: 10.0,
            upload: 1.0,
        };
        assert!(qualities(&slow).iter().all(|(_, q)| *q == Quality::Limited));
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(age_label(1000, 990), "Just now");
        assert_eq!(age_label(1000 + 600, 1000), "10 min ago");
        assert_eq!(age_label(10_000, 10_000 - 2 * 3600), "2 h ago");
        assert_eq!(age_label(200_000, 200_000 - 26 * 3600), "Yesterday");
        assert_eq!(age_label(1_000_000, 1_000_000 - 3 * 86_400), "3 days ago");
    }

    #[test]
    fn data_is_whole_megabytes() {
        assert_eq!(data_label(12_000_000), "12 MB");
    }
}
