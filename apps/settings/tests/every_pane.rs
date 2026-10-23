//! Every pane of the production Settings, at the Yoga's logical size on its
//! capture fixture, through the host path a live window runs: opened from
//! its sidebar row, scrolled a notch at a time, its first switch flipped,
//! then left alone.
//!
//! What each of those must do does not depend on how fast the machine is,
//! so it holds in every build: a click shows its change in the frame the
//! click asks for, and a pane left alone asks for no frame. What they cost
//! on the UI thread is printed per pane and over every pane (p50, p99,
//! max), and held to a budget in optimised builds only:
//! `cargo test -p settings --profile fast --test it every_pane -- --nocapture --test-threads=1`.
//!
//! The UI thread's work is all of it here: input, sync, layout and scene
//! encoding. Rasterising the scene and presenting it are the renderer's
//! and the compositor's, and are not measured.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "percentile indices and microsecond counts over a few hundred samples"
)]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::print_stderr,
    reason = "a measurement reports on stderr and fails loudly"
)]

use std::time::{Duration, Instant};

use settings::SettingsShell;
use settings::categories::Category;
use ui::accesskit::{Role, Toggled};
use ui::framework::testing::{TestApp, perf};

/// What one frame may cost on the UI thread in an optimised build: a
/// fraction of a 120 Hz frame, so the renderer and the compositor have
/// the rest.
const FRAME_BUDGET: Duration = Duration::from_millis(4);
/// What a click on a sidebar row may cost before its frame is drawn: the
/// new pane is built, laid out and encoded within one 60 Hz frame.
const OPEN_BUDGET: Duration = Duration::from_millis(16);

fn optimised() -> bool {
    !cfg!(debug_assertions)
}

#[derive(Default)]
struct Samples(Vec<Duration>);

impl Samples {
    fn push(&mut self, sample: Duration) {
        self.0.push(sample);
    }

    fn at(&self, q: f64) -> Duration {
        let mut sorted = self.0.clone();
        sorted.sort_unstable();
        let last = sorted.len().saturating_sub(1);
        sorted
            .get(((last as f64) * q).round() as usize)
            .copied()
            .unwrap_or_default()
    }

    fn line(&self) -> String {
        format!(
            "n={} p50={}us p99={}us max={}us",
            self.0.len(),
            self.at(0.5).as_micros(),
            self.at(0.99).as_micros(),
            self.at(1.0).as_micros()
        )
    }
}

/// A node of `role` whose centre is inside the window, with its centre.
fn node_centre(
    app: &mut TestApp<SettingsShell>,
    keep: impl Fn(&ui::accesskit::Node) -> bool,
) -> Option<((f64, f64), ui::accesskit::Node)> {
    let (width, height) = (f64::from(perf::YOGA.0), f64::from(perf::YOGA.1));
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find_map(|(_, node)| {
            let bounds = node.bounds()?;
            let at = (
                f64::midpoint(bounds.x0, bounds.x1),
                f64::midpoint(bounds.y0, bounds.y1),
            );
            let inside = at.0 > 0.0 && at.1 > 0.0 && at.0 < width && at.1 < height;
            (inside && bounds.area() > 0.0 && keep(&node)).then_some((at, node))
        })
}

/// The right edge of the sidebar: the widest of its rows.
fn sidebar_edge(app: &mut TestApp<SettingsShell>) -> f64 {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.role() == Role::TreeItem)
        .filter_map(|(_, node)| node.bounds().map(|bounds| bounds.x1))
        .fold(0.0, f64::max)
}

/// Where the sidebar row of `category` is, scrolling the sidebar until it
/// shows the row.
fn row_of(app: &mut TestApp<SettingsShell>, category: Category, edge: f64) -> Option<(f64, f64)> {
    let label = category.label();
    for notch in 0..40 {
        if let Some((at, _)) = node_centre(app, |node| {
            node.role() == Role::TreeItem && node.label() == Some(label.as_str())
        }) && at.1 < f64::from(perf::YOGA.1) - 24.0
        {
            return Some(at);
        }
        app.harness()
            .send(&ui::synthetic_input::cursor_moved(edge / 2.0, 400.0));
        app.harness()
            .scroll(0.0, if notch < 20 { -1.0 } else { 1.0 });
        app.settle();
    }
    None
}

fn is_selected(app: &mut TestApp<SettingsShell>, category: Category) -> bool {
    let label = category.label();
    app.harness().a11y_nodes().iter().any(|(_, node)| {
        node.role() == Role::TreeItem
            && node.label() == Some(label.as_str())
            && node.is_selected() == Some(true)
    })
}

/// What one input took to show: the frames the window ran for it before
/// its change was on screen (`None` when eight frames passed without it),
/// the time on the UI thread for those, and whether it was shown at all
/// within the hang bound.
struct Shown {
    frames: Option<usize>,
    busy: Duration,
    eventually: Option<Duration>,
}

/// Run `input`, then the frames the window runs for it, until `shown`
/// says its change is on screen. `shown`'s own reading of the frame is
/// left out of the time.
fn input_to_shown(
    app: &mut TestApp<SettingsShell>,
    input: impl FnOnce(&mut TestApp<SettingsShell>),
    mut shown: impl FnMut(&mut TestApp<SettingsShell>) -> bool,
) -> Shown {
    let start = Instant::now();
    input(app);
    let mut busy = start.elapsed();
    let mut frames = 0;
    let bound = start + tilo_test_env::hang_bound();
    loop {
        let frame = Instant::now();
        let ran = app.frame();
        if ran {
            frames += 1;
        }
        if frames <= 8 {
            busy += frame.elapsed();
        }
        if shown(app) {
            return Shown {
                frames: (frames <= 8).then_some(frames),
                busy,
                eventually: Some(start.elapsed()),
            };
        }
        if Instant::now() > bound {
            return Shown {
                frames: None,
                busy,
                eventually: None,
            };
        }
        if !ran {
            let _ = ui::rt::sleep_thread(Duration::from_millis(4));
        }
    }
}

/// Frames the window ran over `period` of being left alone, polled at
/// 120 Hz as a display would, with what asked for each.
fn idle_frames(app: &mut TestApp<SettingsShell>, period: Duration) -> Vec<String> {
    let mut asked = Vec::new();
    let until = Instant::now() + period;
    while Instant::now() < until {
        let requesters = app.harness().frame_requesters();
        let queued = app.harness().queued_work_sources();
        if app.frame() {
            asked.push(format!("{requesters:?} {queued:?}"));
        }
        let _ = ui::rt::sleep_thread(Duration::from_millis(8));
    }
    asked
}

/// What happened in one pane.
#[derive(Default)]
struct Pane {
    open_frames: Option<usize>,
    switch: Option<(String, Option<usize>, Option<Duration>)>,
    idle: Vec<String>,
    problems: Vec<String>,
}

#[derive(Default)]
struct All {
    open: Samples,
    ready: Samples,
    scroll: Samples,
    scroll_frames: usize,
    scroll_notches: usize,
    switch: Samples,
}

fn visit(app: &mut TestApp<SettingsShell>, category: Category, edge: f64, all: &mut All) -> Pane {
    let mut pane = Pane::default();
    let Some(row) = row_of(app, category, edge) else {
        pane.problems
            .push("the sidebar never shows its row".to_owned());
        return pane;
    };
    let opened = input_to_shown(
        app,
        |app| app.harness().click_at(row.0, row.1),
        |app| is_selected(app, category),
    );
    pane.open_frames = opened.frames;
    let open = opened.busy;
    all.open.push(open);
    if opened.eventually.is_none() {
        pane.problems
            .push("a click on its row never selected it".to_owned());
        return pane;
    }

    // What the pane reads lands off the UI thread, and the pane fills in.
    let reading = settings::i18n::t("settings-pane-reading");
    let started = Instant::now();
    let filled = app
        .harness()
        .wait_until(tilo_test_env::hang_bound(), |harness| {
            harness
                .inspection_json()
                .is_ok_and(|frame| !frame.contains(&reading))
        });
    if !filled {
        pane.problems
            .push("what the pane reads was never shown".to_owned());
    }
    let ready = started.elapsed();
    all.ready.push(ready);
    app.settle();

    // A notch at a time down the content, then back up.
    let content = (
        f64::midpoint(edge, f64::from(perf::YOGA.0)),
        f64::from(perf::YOGA.1) / 2.0,
    );
    app.harness()
        .send(&ui::synthetic_input::cursor_moved(content.0, content.1));
    app.settle();
    let mut scroll = Samples::default();
    app.harness().begin_profiling();
    for notch in 0..24 {
        let start = Instant::now();
        app.harness()
            .scroll(0.0, if notch < 12 { -1.0 } else { 1.0 });
        app.idle();
        scroll.push(start.elapsed());
    }
    all.scroll_frames += app.harness().take_profile_samples().len();
    all.scroll_notches += scroll.0.len();
    app.settle();

    // The first switch in the content, flipped and flipped back.
    let switch = |node: &ui::accesskit::Node| {
        node.role() == Role::Switch
            && node.toggled().is_some()
            && node.bounds().is_some_and(|bounds| bounds.x0 > edge)
    };
    if let Some((at, node)) = node_centre(app, switch) {
        let before = node.toggled();
        let label = node.label().map(str::to_owned);
        let flipped = |app: &mut TestApp<SettingsShell>, want: Option<Toggled>| {
            node_centre(app, |node| {
                switch(node) && node.label().map(str::to_owned) == label
            })
            .is_some_and(|(_, node)| node.toggled() == want)
        };
        let after = Some(if before == Some(Toggled::True) {
            Toggled::False
        } else {
            Toggled::True
        });
        let there = input_to_shown(
            app,
            |app| app.harness().click_at(at.0, at.1),
            |app| flipped(app, after),
        );
        all.switch.push(there.busy);
        pane.switch = Some((
            label.clone().unwrap_or_default(),
            there.frames,
            there.eventually,
        ));
        app.settle();
        if there.eventually.is_some() {
            let back = input_to_shown(
                app,
                |app| app.harness().click_at(at.0, at.1),
                |app| flipped(app, before),
            );
            all.switch.push(back.busy);
        }
    }

    // A switch that asks before it changes leaves its question open.
    app.harness().key("Escape");

    // Left alone: the pane's motions have ended, and nothing asks for more.
    app.settle();
    let _ = idle_frames(app, Duration::from_millis(300));
    pane.idle = idle_frames(app, Duration::from_millis(500));

    eprintln!(
        "perf pane {category:?}: open {}us in {:?} frame(s), ready {}us, scroll {}, switch (label, frames, shown after) {:?}, idle frames {} {:?}",
        open.as_micros(),
        pane.open_frames,
        ready.as_micros(),
        scroll.line(),
        pane.switch,
        pane.idle.len(),
        pane.idle,
    );
    all.scroll.0.extend(scroll.0);
    pane
}

#[test]
fn every_pane_shows_a_click_on_the_next_frame_scrolls_in_budget_and_idles_without_frames() {
    let mut app = perf::open::<SettingsShell>(None).expect("settings opens its capture");
    let edge = sidebar_edge(&mut app);
    assert!(edge > 0.0, "the sidebar lists the categories");
    let mut all = All::default();
    let mut panes = Vec::new();
    // Each pane once to warm the caches a second visit finds, then measured.
    for &category in Category::ALL {
        let _ = visit(&mut app, category, edge, &mut All::default());
    }
    for &category in Category::ALL {
        panes.push((category, visit(&mut app, category, edge, &mut all)));
    }
    eprintln!(
        "perf every pane open (click to frame shown): {}",
        all.open.line()
    );
    eprintln!("perf every pane ready (reads landed): {}", all.ready.line());
    eprintln!(
        "perf every pane scroll notch: {} ({} frames over {} notches)",
        all.scroll.line(),
        all.scroll_frames,
        all.scroll_notches
    );
    eprintln!("perf every pane switch flip: {}", all.switch.line());

    let mut failures = Vec::new();
    for (category, pane) in &panes {
        failures.extend(
            pane.problems
                .iter()
                .map(|problem| format!("{category:?}: {problem}")),
        );
        if pane.open_frames.is_none_or(|frames| frames > 1) {
            failures.push(format!(
                "{category:?}: a click on its row is shown on the frame after it, not after {:?} frames",
                pane.open_frames
            ));
        }
        if let Some((label, frames, _)) = &pane.switch
            && frames.is_none_or(|frames| frames > 1)
        {
            failures.push(format!(
                "{category:?}: the switch {label:?} shows its flip on the frame after the click, not after {frames:?} frames"
            ));
        }
        if !pane.idle.is_empty() {
            failures.push(format!(
                "{category:?}: left alone it drew {} frames, asked for by {:?}",
                pane.idle.len(),
                pane.idle
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    if optimised() {
        assert!(
            all.open.at(0.99) <= OPEN_BUDGET,
            "opening a pane: {}",
            all.open.line()
        );
        assert!(
            all.scroll.at(0.99) <= FRAME_BUDGET,
            "a scroll notch: {}",
            all.scroll.line()
        );
        assert!(
            all.switch.at(0.99) <= FRAME_BUDGET,
            "flipping a switch: {}",
            all.switch.line()
        );
    }
}
