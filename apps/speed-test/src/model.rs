//! What the app knows about the running and finished tests, independent of
//! how it is drawn: the phase, the readings measured so far, the recent
//! results, and the dial's reading.

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
    current: Option<Finished>,
    pub error: Option<String>,
    /// How far the dial's arc has eased toward its target.
    ring: f64,
}

/// The canonical runtime's readings and recorded measurements for this frame.
pub(crate) struct Projection<'a> {
    pub phase: Phase,
    pub result: Option<(f64, f64, f64, f64)>,
    pub live: f64,
    pub bytes: u64,
    pub ring: f64,
    pub history: &'a [crate::resource::Measurement],
    pub current: Option<&'a crate::resource::Measurement>,
    pub error: Option<String>,
    pub server: String,
}

fn finished(r: &crate::resource::Measurement) -> Finished {
    Finished {
        at: r.at_ms / 1000,
        ping: r.ping,
        jitter: r.jitter,
        download: r.download,
        upload: r.upload,
    }
}

impl Model {
    /// Read-only projection of the canonical resource/runtime, never starts work or records a result.
    pub(crate) fn present(&mut self, projection: Projection<'_>) {
        let Projection {
            phase,
            result,
            live,
            bytes,
            ring,
            history,
            current,
            error,
            server,
        } = projection;
        self.phase = phase;
        self.live = live;
        self.ring = ring;
        self.error = error;
        self.current = current.map(finished);
        self.history = history
            .iter()
            .map(|r| Finished {
                at: r.at_ms / 1000,
                ping: r.ping,
                jitter: r.jitter,
                download: r.download,
                upload: r.upload,
            })
            .collect();
        self.readings = match result {
            Some((ping, jitter, download, upload)) => Readings {
                ping: Some(ping),
                jitter: Some(jitter),
                download: Some(download),
                upload: Some(upload),
                location: Some(server),
                bytes,
            },
            None => Readings {
                location: Some(server),
                bytes,
                ..Readings::default()
            },
        };
    }

    pub(crate) fn new() -> Self {
        Self {
            phase: Phase::Idle,
            readings: Readings::default(),
            live: 0.0,
            history: Vec::new(),
            current: None,
            error: None,
            ring: 0.0,
        }
    }

    pub(crate) fn is_running(&self) -> bool {
        self.phase.is_running()
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
            .then(|| self.current.as_ref().or_else(|| self.history.first()))
            .flatten()
    }
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
    ui::format::capitalize_first(&ui::format::relative(now.saturating_sub(at)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(phase: Phase, history: &[crate::resource::Measurement]) -> Projection<'_> {
        Projection {
            phase,
            result: None,
            live: 0.0,
            bytes: 0,
            ring: 0.0,
            history,
            current: None,
            error: None,
            server: "Cloudflare".to_owned(),
        }
    }

    #[test]
    fn idle_dial_is_empty_with_no_arc() {
        let mut model = Model::new();
        model.present(project(Phase::Idle, &[]));
        let dial = model.dial();
        assert_eq!(dial.value, "—");
        assert!(dial.frac.abs() < f64::EPSILON, "no stub arc at idle");
        assert!(!model.is_running());
    }

    #[test]
    fn a_running_projection_keeps_only_measurements_known_so_far() {
        let mut model = Model::new();
        let mut projection = project(Phase::Ping, &[]);
        projection.live = 13.0;
        model.present(projection);
        assert_eq!(model.readings.ping, None, "a sample is not yet the result");
        assert_eq!(model.dial().value, "13");
        assert_eq!(model.dial().unit, "ms");
        let mut projection = project(Phase::Download, &[]);
        projection.live = 300.0;
        projection.bytes = 10_000_000;
        model.present(projection);
        assert!(model.is_running());
        assert_eq!(model.stage(), 1);
        assert_eq!(
            model.readings.download, None,
            "running rate is not a final result"
        );
        assert_eq!(model.dial().value, "300.0");
    }

    #[test]
    fn a_full_run_shows_the_canonical_history_and_measured_transfer_total() {
        let measurement = crate::resource::Measurement {
            ping: 12.0,
            jitter: 1.5,
            download: 300.0,
            upload: 40.0,
            ..Default::default()
        };
        let history = [measurement];
        let mut projection = project(Phase::Complete, &history);
        projection.current = history.first();
        projection.result = Some((12.0, 1.5, 300.0, 40.0));
        projection.bytes = 12_000_000;
        let mut model = Model::new();
        model.present(projection);
        assert_eq!(model.phase, Phase::Complete);
        assert_eq!(model.stage(), 3);
        assert_eq!(model.readings.ping, Some(12.0));
        assert_eq!(model.readings.jitter, Some(1.5));
        assert_eq!(model.readings.upload, Some(40.0));
        assert_eq!(model.readings.bytes, 12_000_000);
        assert_eq!(model.history.len(), 1);
        assert_eq!(model.latest().map(|r| r.download), Some(300.0));
    }

    #[test]
    fn failure_keeps_the_reason_and_clears_the_dial() {
        let mut projection = project(Phase::Error, &[]);
        projection.error = Some("offline".to_owned());
        let mut model = Model::new();
        model.present(projection);
        assert_eq!(model.phase, Phase::Error);
        assert_eq!(model.error.as_deref(), Some("offline"));
        assert_eq!(model.dial().value, "—");
    }

    #[test]
    fn history_keeps_the_newest_canonical_limit() {
        use crate::resource::{KEPT, SpeedTest};
        use ui::prelude::Replica;
        let mut replica = Replica::<SpeedTest>::new();
        for index in 0..(KEPT + 3) {
            replica
                .apply(SpeedTest::RECORD.with((
                    12.0,
                    1.5,
                    f64::from(u32::try_from(index).unwrap()),
                    40.0,
                    "Cloudflare".to_owned(),
                    "test-device".to_owned(),
                )))
                .expect("the published record action succeeds");
        }
        let state = replica.state().expect("canonical measurements");
        let history: Vec<_> = state.newest_first().cloned().collect();
        let mut model = Model::new();
        model.present(project(Phase::Idle, &history));
        assert_eq!(model.history.len(), KEPT);
        assert_eq!(
            model.history.first().map(|r| r.download),
            Some(f64::from(u32::try_from(KEPT + 2).unwrap()))
        );
        assert_eq!(model.history.last().map(|r| r.download), Some(3.0));
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
        assert_eq!(age_label(10_000, 10_000 - 2 * 3600), "2 hr ago");
        assert_eq!(age_label(200_000, 200_000 - 26 * 3600), "Yesterday");
        assert_eq!(age_label(1_000_000, 1_000_000 - 3 * 86_400), "3 days ago");
    }

    #[test]
    fn data_is_whole_megabytes() {
        assert_eq!(data_label(12_000_000), "12 MB");
    }
}
