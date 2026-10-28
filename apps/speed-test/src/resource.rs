//! Speed Test's resource: the measurements taken, which follow their owner
//! to every device. A run in flight is the surface's own; starting and
//! stopping one are effects, and a finished run is recorded as an action.

use ui::framework::{ResourceApp, ViewCx};
use ui::prelude::*;
use ui::widget::Element;

use crate::app::SpeedTestApp;

/// How many measurements are kept; the oldest go first.
pub const KEPT: usize = 20;

/// Every measurement taken, oldest first. A measurement is what a run
/// found, so only recording one adds to them and only clearing removes them;
/// nobody edits a result.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct SpeedTest {
    /// Measurements
    #[model(readonly)]
    pub results: Collection<Measurement>,
}

/// One finished run.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Measurement {
    /// When it was recorded, in milliseconds since the Unix epoch.
    pub at_ms: i64,
    /// Round trip to the server, in milliseconds.
    pub ping: f64,
    /// Variation of the round trip, in milliseconds.
    pub jitter: f64,
    /// Download speed, in megabits per second.
    pub download: f64,
    /// Upload speed, in megabits per second.
    pub upload: f64,
    /// The server that answered.
    pub server: String,
    /// The device that ran it, as its host names it.
    pub device: String,
}

#[resource(kind = "speed-test.results", label = "Internet speed test")]
impl SpeedTest {
    /// Record a finished measurement.
    #[action(external)]
    pub fn record(
        &mut self,
        cx: &mut ActionCx,
        #[param("Round trip in milliseconds")] ping: f64,
        #[param("Round-trip variation in milliseconds")] jitter: f64,
        #[param("Download speed in megabits per second")] download: f64,
        #[param("Upload speed in megabits per second")] upload: f64,
        #[param("The server that answered")] server: String,
        #[param("The device that ran it")] device: String,
    ) {
        let measurement = Measurement {
            at_ms: cx.now_ms(),
            ping,
            jitter,
            download,
            upload,
            server,
            device,
        };
        self.results.push(cx.id(), measurement);
        let excess = self.results.len().saturating_sub(KEPT);
        let oldest: Vec<_> = self.results.ids().take(excess).cloned().collect();
        for id in oldest {
            self.results.remove(&id);
        }
    }

    /// Forget every measurement.
    #[action(destructive)]
    pub fn clear(&mut self) {
        self.results = Collection::new();
    }
}

impl SpeedTest {
    /// The measurements, newest first.
    pub fn newest_first(&self) -> impl Iterator<Item = &Measurement> {
        self.results.values().rev()
    }
}

/// What anyone can do with a run: the button, the palette, an agent. A run
/// measures this device's own connection and shares nothing, so a guest in
/// a public room runs it as its owner does.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.speed-test.run")]
pub enum SpeedEffect {
    /// Run speed test.
    #[effect(job, cancellable, own_device)]
    Start,
    /// Stop speed test.
    #[effect(own_device)]
    Stop,
}

/// Framework app.
pub struct SpeedTestShell;

impl ResourceApp for SpeedTestShell {
    type Resource = SpeedTest;
    const ID: &'static str = "so.tilo.speed-test";
    const TITLE: &'static str = "Speed Test";

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    const ICON: Option<&'static str> = Some("speed-test");

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        match scenario {
            Some("complete-narrow") => (480, 820),
            Some("complete-short") => (960, 560),
            _ => (1180, 760),
        }
    }
    // Results stay on this device and the document is empty: nothing to
    // look back on.
    const DOCUMENT_HISTORY: bool = false;

    fn view(cx: &mut ViewCx<SpeedTest>) -> Element {
        cx.surface(SpeedTestApp::new())
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "idle",
            "running",
            "complete",
            "error",
            "complete-slow",
            "complete-narrow",
            "complete-short",
        ]
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<SpeedTest>> {
        let record = |ping: f64, jitter: f64, download: f64, upload: f64| {
            SpeedTest::RECORD.with((
                ping,
                jitter,
                download,
                upload,
                "Cloudflare at LHR".to_owned(),
                "capture-device".to_owned(),
            ))
        };
        match name {
            Some("idle" | "running") => Vec::new(),
            // Oldest first: the last is the run the dial shows.
            _ => vec![
                record(14.1, 2.2, 584.6, 109.3),
                record(12.4, 1.8, 618.2, 112.9),
                record(11.8, 1.6, 642.7, 118.4),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::prelude::Replica;

    #[test]
    fn measurements_are_kept_newest_last_and_bounded() {
        let mut replica = Replica::<SpeedTest>::new();
        for run in 0..KEPT + 3 {
            #[expect(clippy::cast_precision_loss, reason = "a small run count")]
            let download = run as f64;
            replica
                .apply(SpeedTest::RECORD.with((
                    10.0,
                    1.0,
                    download,
                    50.0,
                    "Cloudflare".into(),
                    "laptop".into(),
                )))
                .expect("record");
        }
        let state = replica.state().expect("state");
        assert_eq!(state.results.len(), KEPT);
        let newest = state.newest_first().next().expect("a measurement");
        #[expect(clippy::cast_precision_loss, reason = "a small run count")]
        let last = (KEPT + 2) as f64;
        assert!((newest.download - last).abs() < f64::EPSILON);
        assert!(
            state.results.values().all(|result| result.download >= 3.0),
            "the oldest runs went first"
        );

        let first = state.results.ids().next().expect("a measurement").clone();
        let fake = SpeedTest::paths().results.item(&first).download.set(9999.0);
        assert!(
            Invoke::edit(&fake)
                .ok()
                .and_then(|edit| replica.apply(edit).ok())
                .is_none(),
            "a result is never edited"
        );

        replica
            .apply_confirmed(SpeedTest::CLEAR.invoke())
            .expect("clear");
        assert!(replica.state().expect("state").results.is_empty());
    }

    #[test]
    fn history_says_what_a_run_measured() {
        let mut replica = Replica::<SpeedTest>::new();
        replica
            .apply(SpeedTest::RECORD.with((
                12.0,
                1.5,
                640.0,
                118.0,
                "Cloudflare".into(),
                "laptop".into(),
            )))
            .expect("record");
        replica
            .apply_confirmed(SpeedTest::CLEAR.invoke())
            .expect("clear");
        let catalogue =
            tilo_history::Catalogue::new("en-US", &[include_str!("../locales/en-US.ftl")]);
        let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
        let sentences: Vec<String> = replica
            .versions()
            .into_iter()
            .filter_map(|version| version.context)
            .filter(|entry| entry.what.is_some())
            .map(|entry| reader.sentence(&entry))
            .collect();
        assert_eq!(
            sentences,
            [
                "Measured 640 Mbit/s down and 118 Mbit/s up from Cloudflare",
                "Cleared the measurements"
            ]
        );
    }
}

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// Every action the app publishes is reachable by address in a drive of
    /// each of its scenarios: none is advertised yet handled only by a
    /// click. The scenarios stage fixtures; the app someone opens acts on
    /// this machine (its files, its processes), so it is not walked.
    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action() {
        for scenario in super::SpeedTestShell::scenarios() {
            let mut app = TestApp::<super::SpeedTestShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }
}
