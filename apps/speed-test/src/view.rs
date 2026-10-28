//! The screen: the dial and its controls beside the results, which stack
//! below them in a narrow window. Built from the toolkit's composites; the
//! only thing the app draws itself is nothing.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use ui::LocalState;
use ui::TextRole;
use ui::framework::ViewCx;
use ui::taffy::JustifyContent;
use ui::theme::Theme;
use ui::tilo_actions::ActionBinding;
use ui::tokens::Rhythm;
use ui::widget::{AppModel, ModelPath};
use ui::widget::{
    Banner, Button, ButtonRole, Column, Container, Decorator, Element, ListRow, ProgressRing,
    ProgressRingSize, Property, PropertyList, Row, Scrollable, Section, StatGroup, StatTile,
    Stepper, Text, Tone, Widget,
};

use crate::model::{Model, Phase, Projection, Quality, age_label, data_label, dial, qualities};
use crate::resource::{Measurement, SpeedEffect, SpeedTest};
use crate::run::{Reading, Runs, Stage};

/// What the scroll position of the page is, kept across rebuilds.
type ScrollOffset = Rc<Cell<(f32, f32)>>;

/// The whole window: built again when the run moves to another stage or the
/// measurements kept change; the dial follows the stage's reading on its own.
pub(crate) fn page(cx: &mut ViewCx<SpeedTest>) -> Element {
    let runs = Runs::new(cx);
    runs.answer_effects(cx);
    // A screenshot holds the dial's arc still on the staged reading.
    let pinned = cx.local(None::<f64>);
    stage(cx, &runs, &pinned);
    let results = cx.value(|state: &SpeedTest| state.newest_first().cloned().collect::<Vec<_>>());
    let scroll = ScrollOffset::default();
    let dial = DialSource {
        reading: runs.reading.clone(),
        pinned,
        eased: Rc::default(),
    };
    cx.switch(&results, move |cx, results| {
        let (runs, dial, scroll, results) = (
            runs.clone(),
            dial.clone(),
            Rc::clone(&scroll),
            results.clone(),
        );
        let stage = runs.stage.clone();
        cx.switch(&stage, move |cx, stage| {
            let model = model(stage, &runs.reading.get(), &results);
            let run = cx.actions().effect(&if model.is_running() {
                SpeedEffect::Stop
            } else {
                SpeedEffect::Start
            });
            build(&model, &dial, &scroll, run)
        })
    })
}

/// What a screenshot scenario shows of a run.
fn stage(cx: &ViewCx<SpeedTest>, runs: &Runs, pinned: &LocalState<Option<f64>>) {
    let (runs, pinned, results) = (
        runs.clone(),
        pinned.clone(),
        cx.value(|state: &SpeedTest| state.results.len()),
    );
    cx.on_stage(move |scenario| match scenario {
        Some("idle") => {}
        Some("running") => {
            runs.reading.set(Reading {
                live: 386.4,
                bytes: 0,
            });
            runs.stage.set(Stage {
                phase: Phase::Download,
                ..Stage::default()
            });
            pinned.set(Some(0.58));
        }
        _ => {
            if results.get() > 0 {
                runs.stage.set(Stage {
                    phase: Phase::Complete,
                    ..Stage::default()
                });
                pinned.set(Some(0.79));
            }
        }
    });
}

/// What the page shows for the run's `stage` and the measurements kept,
/// newest first. A finished run shows the newest measurement on the dial
/// and the earlier ones below it; once they are cleared there is nothing
/// finished to show.
pub(crate) fn model(stage: &Stage, reading: &Reading, results: &[Measurement]) -> Model {
    let latest = results.first();
    let phase = match (stage.phase, latest) {
        (Phase::Complete, None) => Phase::Idle,
        (phase, _) => phase,
    };
    let current = latest.filter(|_| phase == Phase::Complete);
    let mut model = Model::new();
    model.present(Projection {
        phase,
        result: current.map(|r| (r.ping, r.jitter, r.download, r.upload)),
        live: reading.live,
        bytes: if phase == Phase::Complete {
            stage.bytes
        } else {
            reading.bytes
        },
        history: if current.is_some() {
            results.get(1..).unwrap_or_default()
        } else {
            results
        },
        current,
        error: stage.error.clone(),
        server: latest
            .map_or("Nearest Cloudflare server", |latest| latest.server.as_str())
            .to_owned(),
    });
    model
}

/// The page for the model's current state.
/// `run` is what the run button invokes; without it the button is disabled.
fn build(
    model: &Model,
    dial: &DialSource,
    scroll: &ScrollOffset,
    run: Option<ActionBinding<()>>,
) -> Element {
    if nothing_measured(model) {
        return idle_page(model, dial, scroll, run);
    }
    let columns = Row::new()
        .align_start()
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_child(Box::new(test_column(model, dial, run))),
        ))
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_flex_grow()
                .with_child(Box::new(results_column(model))),
        ));
    let mut page = Column::new().with_intrinsic_height().with_no_padding();
    if let Some(error) = &model.error {
        page = page.push(Box::new(
            Container::new()
                .with_inset(Rhythm::Group)
                .with_child(Box::new(
                    Banner::error("Test interrupted").with_message(error.clone()),
                )),
        ));
    }
    let page = page.push(Box::new(TestColumns {
        row: columns,
        preferred_minimum: 0.0,
    }));
    Box::new(
        Scrollable::new_vertical()
            .with_persisted_offset(Rc::clone(scroll))
            .with_child(Box::new(page)),
    )
}

/// Whether there is nothing to show beside the test: no reading of this
/// test, no finished test.
fn nothing_measured(model: &Model) -> bool {
    let readings = &model.readings;
    readings.ping.is_none()
        && readings.download.is_none()
        && readings.upload.is_none()
        && readings.bytes == 0
        && model.history.is_empty()
        && model.latest().is_none()
}

/// Before any test: the dial, the action and where it measures, as one
/// column in the middle of the window rather than a dial in a corner and
/// an empty results column beside it.
fn idle_page(
    model: &Model,
    dial: &DialSource,
    scroll: &ScrollOffset,
    run: Option<ActionBinding<()>>,
) -> Element {
    let mut column = test_column(model, dial, run).push(centred(Box::new(
        Text::new(&server_label(model))
            .with_role(TextRole::Caption)
            .with_id("server"),
    )));
    if let Some(error) = &model.error {
        column = column.push(Box::new(
            Container::new()
                .with_no_padding()
                .readable()
                .with_child(Box::new(
                    Banner::error("Test interrupted").with_message(error.clone()),
                )),
        ));
    }
    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_content_min_viewport_height()
            .with_persisted_offset(Rc::clone(scroll))
            .with_child(Box::new(Balanced(
                Column::new().with_no_padding().push(Box::new(column)),
            ))),
    )
}

/// Where the test measures, said once.
fn server_label(model: &Model) -> String {
    match model.readings.location.as_deref() {
        // A server that answered before: "Cloudflare at AMS".
        Some(server) if !server.starts_with("Nearest") => format!("Measures against {server}"),
        _ => "Measures against the nearest Cloudflare server".to_owned(),
    }
}

/// A column that centres its children in the height it is given: the
/// scroll view stretches it to at least what it shows.
struct Balanced(Column);

impl Decorator for Balanced {
    type Inner = Column;

    fn inner(&self) -> &Column {
        &self.0
    }
    fn inner_mut(&mut self) -> &mut Column {
        &mut self.0
    }

    fn build_layout(
        &mut self,
        tree: &mut ui::taffy::TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        let node = self.0.build_layout(tree, theme, nodes);
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn adjust_layout_style(
        &mut self,
        tree: &mut ui::taffy::TaffyTree<()>,
        node: ui::taffy::NodeId,
        text: &mut ui::TextRenderer,
    ) -> bool {
        let mut changed = self.0.adjust_layout_style(tree, node, text);
        if let Ok(style) = tree.style(node)
            && style.justify_content != Some(JustifyContent::Center)
        {
            let mut style = style.clone();
            style.justify_content = Some(JustifyContent::Center);
            changed |= tree.set_style(node, style).is_ok();
        }
        changed
    }
}

/// The preferred two-column measure is capped by the actual available row.
/// At phone widths the shared composites must see the viewport width, so
/// their own wrapping (result tiles and progress stations) can operate.
struct TestColumns {
    row: Row,
    preferred_minimum: f32,
}

impl Decorator for TestColumns {
    type Inner = Row;

    fn inner(&self) -> &Row {
        &self.row
    }
    fn inner_mut(&mut self) -> &mut Row {
        &mut self.row
    }

    fn build_layout(
        &mut self,
        tree: &mut ui::taffy::TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        self.preferred_minimum = theme.tokens.metrics.content_min_width;
        let node = self.row.build_layout(tree, theme, nodes);
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn adjust_layout_style(
        &mut self,
        tree: &mut ui::taffy::TaffyTree<()>,
        node: ui::taffy::NodeId,
        text: &mut ui::TextRenderer,
    ) -> bool {
        let Ok(layout) = tree.layout(node) else {
            return false;
        };
        // Intrinsic descendants may initially widen the row beyond its
        // Scrollable. Cap against the enclosing layout's measured viewport,
        // rather than accepting that overflow as the available space.
        let mut available = layout.size.width;
        let mut ancestor = tree.parent(node);
        while let Some(parent) = ancestor {
            if let Ok(layout) = tree.layout(parent) {
                available = available.min(layout.size.width);
            }
            ancestor = tree.parent(parent);
        }
        let minimum = ui::taffy::Dimension::length(self.preferred_minimum.min(available));
        let children = tree.children(node).unwrap_or_default();
        let mut changed = false;
        for child in children {
            let Ok(style) = tree.style(child) else {
                continue;
            };
            if style.min_size.width == minimum && style.size.width == minimum {
                continue;
            }
            let mut style = style.clone();
            style.min_size.width = minimum;
            style.size.width = minimum;
            changed |= tree.set_style(child, style).is_ok();
        }
        // This adapter stands in for the row's node; the row adjusts it too.
        self.row.adjust_layout_style(tree, node, text) | changed
    }
}

/// The dial, what it is doing, and the one action.
fn test_column(model: &Model, dial: &DialSource, run: Option<ActionBinding<()>>) -> Column {
    let status = match model.phase {
        Phase::Idle => "Ready to test",
        Phase::Ping => "Measuring latency",
        Phase::Download => "Measuring download",
        Phase::Upload => "Measuring upload",
        Phase::Complete => "Test complete",
        Phase::Error => "Ready to try again",
    };
    let (label, role) = if model.is_running() {
        ("Stop test", ButtonRole::Secondary)
    } else {
        ("Run speed test", ButtonRole::Primary)
    };
    let button = Button::new("run", label).role(role);
    let run = match run {
        Some(binding) => button.on_action(binding),
        None => button.with_enabled(false),
    };

    let mut column = Column::new()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Section)
        .push(centred(Box::new(Dial::new(dial, model))))
        .push(centred(Box::new(
            Text::new(status).with_role(TextRole::Subheading),
        )))
        .push(centred(Box::new(run)));
    if model.phase != Phase::Idle && model.phase != Phase::Error {
        column = column.push(Box::new(
            Container::new()
                .with_no_padding()
                .readable()
                .with_child(Box::new(
                    Stepper::new(
                        ["Ping", "Download", "Upload"]
                            .into_iter()
                            .map(str::to_owned)
                            .collect(),
                    )
                    .current(model.stage()),
                )),
        ));
    }
    column
}

fn centred(child: Element) -> Element {
    Box::new(Row::new().with_justify(JustifyContent::Center).push(child))
}

/// The measured numbers, the facts about the test, and the recent tests.
fn results_column(model: &Model) -> Column {
    let readings = &model.readings;
    let number = |value: Option<f64>, decimals: usize| {
        value.map_or_else(String::new, |v| format!("{v:.decimals$}"))
    };
    // The location names the server ("Cloudflare at AMS"); without one,
    // the provider alone.
    let server = readings
        .location
        .clone()
        .unwrap_or_else(|| "Cloudflare".to_owned());
    // Only what was measured is shown: before a test there are no numbers,
    // and a dash in their place reads as a value.
    let mut facts = vec![Property::new("Server", server)];
    if let Some(jitter) = readings.jitter {
        facts.push(
            Property::new("Jitter", format!("{jitter:.1} ms")).with_kind(ui::TextKind::Numeric),
        );
    }
    if readings.bytes > 0 {
        facts.push(
            Property::new("Data used", data_label(readings.bytes)).with_kind(ui::TextKind::Numeric),
        );
    }
    let facts = PropertyList::new(facts);

    let mut column = Column::new()
        .with_intrinsic_height()
        .with_rhythm(Rhythm::Section);
    if readings.ping.is_some() || readings.download.is_some() || readings.upload.is_some() {
        let tiles = StatGroup::new(vec![
            Box::new(tile("Ping", number(readings.ping, 0), readings.ping, "ms")),
            Box::new(tile(
                "Download",
                number(readings.download, 1),
                readings.download,
                "Mbps",
            )),
            Box::new(tile(
                "Upload",
                number(readings.upload, 1),
                readings.upload,
                "Mbps",
            )),
        ]);
        column = column.push(Box::new(Section::titled("Results", vec![Box::new(tiles)])));
    }
    column = column.push(Box::new(Section::titled(
        "Connection",
        vec![Box::new(facts)],
    )));

    if let Some(latest) = model.latest() {
        let rows = qualities(latest)
            .into_iter()
            .map(|(use_case, quality)| {
                Property::new(use_case, quality.label()).tone(match quality {
                    Quality::Excellent | Quality::Good => Tone::Success,
                    Quality::Limited => Tone::Warning,
                })
            })
            .collect();
        column = column.push(Box::new(Section::titled(
            "Connection quality",
            vec![Box::new(PropertyList::new(rows))],
        )));
    }

    if !model.history.is_empty() {
        let now = ui::clock::local_now().timestamp();
        let rows: Vec<Element> = model
            .history
            .iter()
            .enumerate()
            .map(|(index, result)| -> Element {
                Box::new(
                    ListRow::new(
                        format!("history-{index}"),
                        format!("{:.1} Mbps down", result.download),
                    )
                    .secondary(format!(
                        "{:.1} Mbps up, {:.0} ms ping",
                        result.upload, result.ping
                    ))
                    .meta(age_label(now, result.at)),
                )
            })
            .collect();
        column = column.push(Box::new(Section::titled("Recent tests", rows)));
    }
    column
}

/// One result: its number once measured, a dash until then.
fn tile(label: &str, value: String, measured: Option<f64>, unit: &str) -> StatTile {
    let tile = StatTile::new(label, value);
    if measured.is_some() {
        tile.unit(unit)
    } else {
        tile
    }
}

/// What the dial follows between rebuilds: the stage's reading, or the
/// arc a screenshot holds still.
#[derive(Clone)]
struct DialSource {
    reading: LocalState<Reading>,
    pinned: LocalState<Option<f64>>,
    /// How far the arc has eased toward its target, kept across rebuilds
    /// so a new stage continues the arc rather than restarting it.
    eased: Rc<Cell<f64>>,
}

/// The dial: a progress ring whose arc eases toward the stage's reading
/// every frame.
struct Dial {
    ring: ProgressRing,
    reading: LocalState<Reading>,
    pinned: LocalState<Option<f64>>,
    phase: Phase,
    /// The finished run's download, which a complete dial shows.
    download: Option<f64>,
    eased: Rc<Cell<f64>>,
    shown: crate::model::DialReading,
    last_frame: ui::time::Instant,
}

impl Dial {
    fn new(source: &DialSource, model: &Model) -> Self {
        let eased = source.pinned.get().unwrap_or_else(|| source.eased.get());
        let shown = dial(model.phase, model.live(), model.readings.download, eased);
        Self {
            ring: ProgressRing::new(shown.frac)
                .size(ProgressRingSize::Large)
                .value(shown.value.clone())
                .caption(shown.unit),
            reading: source.reading.clone(),
            pinned: source.pinned.clone(),
            phase: model.phase,
            download: model.readings.download,
            eased: Rc::clone(&source.eased),
            shown,
            last_frame: ui::time::now(),
        }
    }

    fn reading(&self) -> crate::model::DialReading {
        dial(
            self.phase,
            self.reading.get().live,
            self.download,
            self.eased.get(),
        )
    }

    fn target(&self) -> f64 {
        crate::model::dial_target(self.phase, self.reading.get().live, self.download)
    }
}

impl Decorator for Dial {
    type Inner = ProgressRing;

    fn inner(&self) -> &ProgressRing {
        &self.ring
    }

    fn inner_mut(&mut self) -> &mut ProgressRing {
        &mut self.ring
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let now = ui::time::now();
        let delta = now.duration_since(self.last_frame).as_secs_f64().min(0.05);
        self.last_frame = now;
        let eased = self.eased.get();
        self.eased.set(match self.pinned.get() {
            Some(pinned) => pinned,
            None => eased + (self.target() - eased) * (1.0 - (-delta * 9.0).exp()),
        });
        let dial = self.reading();
        let moved = (dial.frac - self.shown.frac).abs() > 0.0005 || dial.value != self.shown.value;
        if moved {
            self.ring.set(dial.frac, dial.value.clone());
            self.shown = dial;
        }
        Widget::update(&mut self.ring, state, path) | moved
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let moving = self.pinned.get().is_none()
            && (self.phase.is_running() || (self.eased.get() - self.target()).abs() > 0.001);
        moving.then(|| ui::time::now() + ui::time::Duration::from_millis(16))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::SpeedTestShell;
    use ui::framework::ResourceApp;
    use ui::prelude::{Replica, TestApp};
    use ui::tilo_actions::{Availability, DoorError};
    use ui::vello::kurbo::Rect;

    fn availability(app: &mut TestApp<SpeedTestShell>, id: &str) -> Availability {
        app.harness()
            .resources()
            .iter()
            .flat_map(|resource| &resource.actions)
            .find(|action| action.id == id)
            .map_or_else(
                || panic!("{id} is published"),
                |action| action.contract.availability.clone(),
            )
    }

    fn complete() -> Stage {
        Stage {
            phase: Phase::Complete,
            ..Stage::default()
        }
    }

    fn measurements(scenario: &str) -> Vec<Measurement> {
        let mut replica = Replica::<SpeedTest>::new();
        for invoke in SpeedTestShell::scenario(Some(scenario)) {
            replica.apply(invoke).expect("record");
        }
        replica
            .state()
            .expect("state")
            .newest_first()
            .cloned()
            .collect()
    }

    #[test]
    fn start_and_stop_are_published_as_the_run_stands() {
        let mut app = TestApp::<SpeedTestShell>::new().expect("the app opens");
        assert_eq!(availability(&mut app, "start"), Availability::Available);
        assert!(matches!(
            availability(&mut app, "stop"),
            Availability::Unavailable { code, .. } if code == "idle"
        ));
        let refused = app.effect(&SpeedEffect::Stop);
        assert!(
            matches!(&refused, Err(DoorError::Unavailable { code, .. }) if code == "idle"),
            "{refused:?}"
        );
        app.invoke(SpeedTest::RECORD.with((
            9.0,
            1.0,
            500.0,
            90.0,
            "Cloudflare · FRA".into(),
            "test-device".into(),
        )))
        .expect("a run is recorded");
        let app = app.reopen().expect("the app reopens");
        assert_eq!(app.state().results.len(), 1, "measurements are kept");
    }

    #[test]
    fn a_running_test_shows_only_what_it_measured_so_far() {
        let running = Stage {
            phase: Phase::Download,
            ..Stage::default()
        };
        let reading = Reading {
            live: 300.0,
            bytes: 10_000_000,
        };
        let model = model(&running, &reading, &measurements("complete"));
        assert!(model.is_running());
        assert_eq!(model.stage(), 1);
        assert_eq!(model.readings.download, None, "a running rate is no result");
        assert_eq!(model.readings.bytes, 10_000_000);
        assert_eq!(
            model.history.len(),
            3,
            "a running test leaves every run below"
        );
    }

    #[test]
    fn a_finished_run_shows_on_the_dial_and_the_earlier_ones_below_it() {
        let results = measurements("complete");
        let idle = model(&Stage::default(), &Reading::default(), &results);
        assert_eq!(idle.history.len(), 3, "an idle dial leaves every run below");
        assert_eq!(idle.readings.location.as_deref(), Some("Cloudflare at LHR"));

        let done = model(&complete(), &Reading::default(), &results);
        assert_eq!(
            done.readings.download,
            Some(642.7),
            "the newest run is on the dial"
        );
        let below: Vec<f64> = done.history.iter().map(|run| run.download).collect();
        assert_eq!(below, [618.2, 584.6]);
    }

    #[test]
    fn completed_dial_follows_record_changes_and_clearing_results() {
        let mut results = measurements("complete");
        results.insert(
            0,
            Measurement {
                download: 123.0,
                server: "Another server".into(),
                ..Measurement::default()
            },
        );
        let done = model(&complete(), &Reading::default(), &results);
        assert_eq!(done.readings.download, Some(123.0));
        assert_eq!(done.readings.location.as_deref(), Some("Another server"));
        assert_eq!(done.history.len(), 3, "only the dial's record is omitted");
        assert!(done.history[0].download.total_cmp(&642.7).is_eq());

        let cleared = model(&complete(), &Reading::default(), &[]);
        assert_eq!(cleared.phase, Phase::Idle);
        assert!(cleared.readings.download.is_none());
        assert!(cleared.history.is_empty());
    }

    #[test]
    fn metadata_and_dial_never_touch_the_primary_action() {
        fn bounds(widget: &dyn Widget, name: &str) -> Option<Rect> {
            if widget.name() == name {
                return Some(widget.get_rect());
            }
            widget
                .children()
                .into_iter()
                .find_map(|child| bounds(child, name))
        }
        for viewport in [
            (1180, 520),
            (1180, 640),
            (1180, 760),
            (1180, 900),
            (480, 820),
            (560, 820),
            (700, 820),
            (849, 820),
        ] {
            let mut app = TestApp::<SpeedTestShell>::capture_at(Some("complete"), viewport)
                .expect("the actual completed screen");
            let harness = app.harness();
            let action = *harness
                .targets()
                .get("run")
                .expect("the production primary action");
            let dial =
                bounds(harness.root_widget(), "ProgressRing").expect("the actual shared dial");
            let facts =
                bounds(harness.root_widget(), "PropertyList").expect("the actual metadata rows");
            assert!(action.width() > 0.0 && dial.width() > 0.0 && facts.width() > 0.0);
            assert!(
                action.intersect(facts).area().total_cmp(&0.0).is_eq(),
                "metadata and primary action overlap at {viewport:?}"
            );
            assert!(
                dial.y1 < action.y0,
                "dial and primary action overlap at {viewport:?}"
            );
        }
    }
}
