//! What the interactions a person feels cost an app on the UI thread:
//! scrolling, typing, clicking and switching views, each measured through
//! the production host path (real winit events, then the frames the host
//! runs after them) with the phases they ran.
//!
//! Every app measures the same way, so one app's numbers read beside
//! another's: [`open`] mounts the app at the Yoga's logical size on the
//! fixture its `--shots` capture shows, the drivers send one interaction
//! per step, and [`Report::print`] writes the median, the 99th percentile,
//! the worst step, the mean per frame pass and every named scope
//! ([`tilo_resource::profiling`]) per step.

use std::collections::BTreeMap;

use ui_core::accesskit::Role;
use ui_core::launch::LaunchError;

use super::HeadlessApp;
use crate::framework::app::ResourceApp;

/// The Yoga's logical viewport: 2944x1840 physical at scale 2.
pub const YOGA: (u32, u32) = (1472, 920);

/// One interaction measured over several steps.
pub struct Report {
    label: String,
    /// Wall time of each step, its settle included, in microseconds.
    samples: Vec<u64>,
    /// Each named scope's total time (ns) and calls over every step.
    phases: BTreeMap<&'static str, (u128, usize)>,
    /// The frame passes the steps ran.
    frames: Vec<ui_core::profiling::FrameTiming>,
}

impl Report {
    /// The median step, in microseconds.
    #[must_use]
    pub fn p50_us(&self) -> u64 {
        self.quantile(0.5)
    }

    /// The 99th-percentile step, in microseconds.
    #[must_use]
    pub fn p99_us(&self) -> u64 {
        self.quantile(0.99)
    }

    /// The steps measured.
    #[must_use]
    pub fn steps(&self) -> usize {
        self.samples.len()
    }

    /// Mean time per step of the scope `phase`, in microseconds; zero when
    /// it never ran.
    #[must_use]
    pub fn phase_us(&self, phase: &str) -> u64 {
        let steps = u128::try_from(self.samples.len().max(1)).unwrap_or(1);
        self.phases.get(phase).map_or(0, |(ns, _)| {
            u64::try_from(ns.checked_div(steps).unwrap_or(0) / 1000).unwrap_or(u64::MAX)
        })
    }

    /// Calls per step of the scope `phase`, rounded down.
    #[must_use]
    pub fn phase_calls(&self, phase: &str) -> usize {
        self.phases.get(phase).map_or(0, |(_, calls)| {
            calls.checked_div(self.samples.len()).unwrap_or(*calls)
        })
    }

    fn quantile(&self, q: f64) -> u64 {
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        let last = sorted.len().saturating_sub(1);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "a percentile index over a few hundred samples"
        )]
        let at = ((last as f64) * q).round() as usize;
        sorted.get(at.min(last)).copied().unwrap_or_default()
    }

    /// Write the report to stderr, one `perf` line per figure.
    #[allow(
        clippy::print_stderr,
        reason = "a measurement reports on stderr, as the perf tests do"
    )]
    pub fn print(&self) -> &Self {
        let max = self.samples.iter().copied().max().unwrap_or_default();
        eprintln!(
            "perf {}: n={} p50={}us p99={}us max={}us",
            self.label,
            self.samples.len(),
            self.p50_us(),
            self.p99_us(),
            max,
        );
        let frames = self.frames.len();
        let steps = u64::try_from(self.samples.len().max(1)).unwrap_or(1);
        let per_step = |pass: fn(&ui_core::profiling::FrameTiming) -> u64| {
            self.frames
                .iter()
                .map(pass)
                .sum::<u64>()
                .checked_div(steps)
                .unwrap_or(0)
        };
        eprintln!(
            "perf {} passes per step ({frames} frames): sync={}us build={}us apply={}us draw={}us event={}us resources={}us",
            self.label,
            per_step(|f| f.sync_us),
            per_step(|f| f.build_layout_us),
            per_step(|f| f.apply_layout_us),
            per_step(|f| f.draw_us),
            per_step(|f| f.handle_event_us),
            per_step(|f| f.resource_availability_us),
        );
        let mut phases: Vec<_> = self.phases.iter().collect();
        phases.sort_by_key(|(_, (ns, _))| std::cmp::Reverse(*ns));
        for (phase, (_, calls)) in phases {
            eprintln!(
                "perf {} phase {phase}: {}us per step over {} calls",
                self.label,
                self.phase_us(phase),
                calls,
            );
        }
        self
    }
}

/// `A` as its `--shots` capture shows it (the fixture of `scenario`, or the
/// plain capture), at [`YOGA`], with every frame the launch queued run.
///
/// # Errors
/// Returns the capture's launch failure, or an invalid viewport when the app
/// refuses the Yoga's.
pub fn open<A: ResourceApp>(scenario: Option<&str>) -> Result<HeadlessApp<A>, LaunchError> {
    let mut app = HeadlessApp::<A>::capture_at(scenario, YOGA)?;
    app.settle();
    app.harness()
        .resize(YOGA)
        .map_err(|error| LaunchError::InvalidViewport(error.to_string()))?;
    app.settle();
    Ok(app)
}

/// Run `step` `steps` times, each followed by what the live window does
/// once input stops ([`HeadlessApp::idle`]), and measure each one with the
/// scopes and frame passes it ran.
/// `warm` steps run first, unmeasured, so caches hold what a second visit
/// finds.
pub fn measure<A: ResourceApp>(
    app: &mut HeadlessApp<A>,
    label: &str,
    (warm, steps): (usize, usize),
    mut step: impl FnMut(&mut HeadlessApp<A>, usize),
) -> Report {
    for index in 0..warm {
        step(app, index);
        app.idle();
    }
    let mut report = Report {
        label: label.to_owned(),
        samples: Vec::with_capacity(steps),
        phases: BTreeMap::new(),
        frames: Vec::new(),
    };
    app.harness().begin_profiling();
    for index in 0..steps {
        let start = ui_core::time::now();
        let ((), scopes) = tilo_resource::profiling::observe(|| {
            step(app, warm.saturating_add(index));
            app.idle();
        });
        report
            .samples
            .push(u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX));
        for scope in scopes {
            let entry = report.phases.entry(scope.phase).or_default();
            entry.0 = entry.0.saturating_add(scope.elapsed_ns);
            entry.1 = entry.1.saturating_add(scope.calls);
        }
    }
    report.frames = app.harness().take_profile_samples();
    report
}

/// What opening `A` costs on the UI thread, warm: the app opened as a
/// person opens it a second time, on a profile that already holds its
/// record, from the host preparing the launch to the first frame laid out
/// and drawn, with the named scopes it ran. One launch runs first,
/// unmeasured, to make the profile and its record; each measured launch
/// then opens the app, and its close is outside the measurement.
///
/// What the GPU, the window and the compositor add on a device is not here:
/// this is the part of a launch the app's own code decides.
///
/// # Errors
/// Returns the launch failure.
pub fn start<A: ResourceApp>(launches: usize) -> Result<Report, LaunchError> {
    let profile = tempfile::tempdir().map_err(|error| LaunchError::Operation(error.to_string()))?;
    drop(HeadlessApp::<A>::open_data_dir(profile.path())?);
    let mut report = Report {
        label: format!("{} start", A::ID),
        samples: Vec::with_capacity(launches),
        phases: BTreeMap::new(),
        frames: Vec::new(),
    };
    for _ in 0..launches {
        let start = ui_core::time::now();
        let (app, scopes) =
            tilo_resource::profiling::observe(|| HeadlessApp::<A>::open_data_dir(profile.path()));
        report
            .samples
            .push(u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX));
        for scope in scopes {
            let entry = report.phases.entry(scope.phase).or_default();
            entry.0 = entry.0.saturating_add(scope.elapsed_ns);
            entry.1 = entry.1.saturating_add(scope.calls);
        }
        drop(app?);
    }
    Ok(report)
}

/// What launching the app binary at `exe` costs cold, the way a person's
/// launch runs: each launch is a process of its own (`exe --first-frame`), so
/// its font database, shaping caches and every other per-process cache start
/// empty, and it constructs the app on the live path and draws its first
/// frame as its window would, without a window or GPU. The profile is a
/// private directory the first, unmeasured launch makes, so each measured
/// launch opens an existing record, as a second launch does; the process's
/// environment points nowhere real (no display, bus or runtime).
///
/// Each sample is the time from the process starting to its first frame
/// drawn; the phases are the startup marks' (`APP-STARTUP`) and the named
/// scopes' (`FIRST-FRAME-SCOPE`) medians over the launches.
///
/// # Panics
/// When a launch fails or prints no startup line.
#[must_use]
#[allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a measurement fails loudly when the app binary cannot run or says nothing"
)]
pub fn cold_start(label: &str, exe: &std::path::Path, launches: usize) -> ColdStart {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let root = scratch.path();
    let launch = || {
        let output = std::process::Command::new(exe)
            .arg("--first-frame")
            .arg("--data-dir")
            .arg(root.join("profile"))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("XDG_RUNTIME_DIR", root.join("run"))
            .env("TILO_STARTUP_METRICS_PATH", root.join("metrics.json"))
            .env("LANG", "en_US.UTF-8")
            .output()
            .expect("the app binary runs");
        assert!(
            output.status.success(),
            "{label}: --first-frame failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    for dir in ["home", "config", "data", "state", "cache", "run"] {
        std::fs::create_dir_all(root.join(dir)).expect("a scratch directory");
    }
    let _ = launch();
    let mut cold = ColdStart {
        label: label.to_owned(),
        totals: Vec::new(),
        phases: BTreeMap::new(),
        marks: BTreeMap::new(),
        calls: BTreeMap::new(),
    };
    for _ in 0..launches {
        cold.record(label, &launch());
    }
    cold
}

/// [`cold_start`]'s launches.
pub struct ColdStart {
    label: String,
    /// Milliseconds from each process starting to its first frame drawn.
    totals: Vec<u64>,
    /// Each phase's microseconds, one per launch it ran in.
    phases: BTreeMap<String, Vec<u64>>,
    /// Each startup mark's microseconds since the process started, one per
    /// time it was marked.
    marks: BTreeMap<String, Vec<u64>>,
    /// The most times one launch ran each named scope, by `stage phase`.
    calls: BTreeMap<String, u64>,
}

impl ColdStart {
    /// Add what one launch printed: its total, its startup marks and its
    /// first-frame scopes.
    #[allow(
        clippy::panic,
        reason = "a measurement fails loudly when the app says nothing"
    )]
    fn record(&mut self, label: &str, printed: &str) {
        let line = printed
            .lines()
            .find(|line| line.starts_with("APP-STARTUP "))
            .unwrap_or_else(|| panic!("{label}: no startup line in {printed}"));
        for field in line.split_whitespace() {
            if let Some(total) = field.strip_prefix("total_ms=") {
                self.totals.push(total.parse().unwrap_or_default());
            } else if let Some(phases) = field.strip_prefix("phases=") {
                for phase in phases.split(',') {
                    if let Some((name, ms)) = phase.rsplit_once(':') {
                        self.phases
                            .entry(format!("mark {name}"))
                            .or_default()
                            .push(ms.parse::<u64>().unwrap_or_default().saturating_mul(1000));
                    }
                }
            }
        }
        for mark in printed
            .lines()
            .filter_map(|line| line.strip_prefix("FIRST-FRAME-MARK "))
        {
            let mut name = "";
            let mut us = 0;
            for field in mark.split_whitespace() {
                if let Some(value) = field.strip_prefix("name=") {
                    name = value;
                } else if let Some(value) = field.strip_prefix("us=") {
                    us = value.parse().unwrap_or_default();
                }
            }
            self.marks.entry(name.to_owned()).or_default().push(us);
        }
        for scope in printed
            .lines()
            .filter_map(|line| line.strip_prefix("FIRST-FRAME-SCOPE "))
        {
            let field = |key: &str| {
                scope
                    .split_whitespace()
                    .find_map(|field| field.strip_prefix(key))
                    .unwrap_or_default()
                    .to_owned()
            };
            let calls: u64 = field("calls=").parse().unwrap_or_default();
            let most = self
                .calls
                .entry(format!("{} {}", field("stage="), field("phase=")))
                .or_default();
            *most = (*most).max(calls);
            let name = format!(
                "{} {} x{}",
                field("stage="),
                field("phase="),
                field("calls=")
            );
            self.phases
                .entry(name)
                .or_default()
                .push(field("us=").parse().unwrap_or_default());
        }
    }
}

impl ColdStart {
    /// The most times one launch ran the named scope `phase` in `stage`
    /// (`construction` or `frame`): how often the first frame did a piece of
    /// work it should do once.
    #[must_use]
    pub fn calls(&self, stage: &str, phase: &str) -> u64 {
        self.calls
            .get(&format!("{stage} {phase}"))
            .copied()
            .unwrap_or_default()
    }

    /// The median launch, in milliseconds.
    #[must_use]
    pub fn p50_ms(&self) -> u64 {
        median(&self.totals)
    }

    /// Write the launches to stderr: the median and worst launch, then each
    /// phase's median, largest first.
    #[allow(
        clippy::print_stderr,
        reason = "a measurement reports on stderr, as the perf tests do"
    )]
    pub fn print(&self) -> &Self {
        eprintln!(
            "perf {} cold start: n={} p50={}ms max={}ms",
            self.label,
            self.totals.len(),
            self.p50_ms(),
            self.totals.iter().copied().max().unwrap_or_default()
        );
        let mut phases: Vec<_> = self
            .phases
            .iter()
            .map(|(name, samples)| (name, median(samples)))
            .collect();
        phases.sort_by_key(|phase| std::cmp::Reverse(phase.1));
        for (name, us) in phases.into_iter().filter(|(_, us)| *us >= 100) {
            eprintln!("perf {} cold start phase {name}: {us}us", self.label);
        }
        // When each mark came, in the order they came: the timeline of a
        // launch, its threads interleaved.
        let mut marks: Vec<_> = self
            .marks
            .iter()
            .map(|(name, samples)| (median(samples), name))
            .collect();
        marks.sort();
        for (us, name) in marks {
            eprintln!("perf {} cold start at {us}us: {name}", self.label);
        }
        self
    }
}

fn median(samples: &[u64]) -> u64 {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    sorted.get(sorted.len() / 2).copied().unwrap_or_default()
}

/// An accessibility node a person can point at: its label and the centre
/// of its bounds, in logical pixels.
#[derive(Clone, Debug)]
pub struct Target {
    /// What assistive technology reads for it.
    pub label: String,
    /// The centre of its bounds.
    pub at: (f64, f64),
}

/// Every node of one of `roles` with bounds inside the viewport, in tree
/// order: the rows of a sidebar ([`Role::TreeItem`]), tabs, list items.
pub fn targets<A: ResourceApp>(app: &mut HeadlessApp<A>, roles: &[Role]) -> Vec<Target> {
    let (width, height) = YOGA;
    app.harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| roles.contains(&node.role()))
        .filter_map(|(_, node)| {
            let bounds = node.bounds()?;
            let at = (
                f64::midpoint(bounds.x0, bounds.x1),
                f64::midpoint(bounds.y0, bounds.y1),
            );
            let inside = at.0 > 0.0
                && at.1 > 0.0
                && at.0 < f64::from(width)
                && at.1 < f64::from(height)
                && bounds.x1 > bounds.x0
                && bounds.y1 > bounds.y0;
            inside.then(|| Target {
                label: node
                    .label()
                    .or_else(|| node.value())
                    .unwrap_or_default()
                    .to_owned(),
                at,
            })
        })
        .collect()
}

/// Wheel steps at `at`: the pointer moves there once, then each step is one
/// notch, `steps / 2` down and the rest back up, so the content moves at
/// every step that has room to.
pub fn scroll<A: ResourceApp>(app: &mut HeadlessApp<A>, at: (f64, f64), steps: usize) -> Report {
    app.harness()
        .send(&ui_core::synthetic_input::cursor_moved(at.0, at.1));
    app.settle();
    let half = steps / 2;
    measure(app, "scroll step", (4, steps), |app, index| {
        // The warm notches go down, so the measured ones start mid-way.
        let down = index < half.saturating_add(4);
        app.harness().scroll(0.0, if down { -1.0 } else { 1.0 });
    })
}

/// The frames a touchpad fling at `at` glides on: two fingers move the
/// content and lift while moving, then each measured step is one frame the
/// host runs because the glide asks for it
/// ([`ui_core::widget::Widget::needs_animation_frame`]), as the live
/// window runs them at the display's rate. Flings until `frames` glide
/// frames are measured or `flings` gestures stopped gliding.
pub fn glide<A: ResourceApp>(app: &mut HeadlessApp<A>, at: (f64, f64), frames: usize) -> Report {
    use ui_core::winit::event::{MouseScrollDelta, TouchPhase, WindowEvent};
    let wheel = |phase: TouchPhase, dy: f64| WindowEvent::MouseWheel {
        device_id: ui_core::winit::event::DeviceId::dummy(),
        delta: MouseScrollDelta::PixelDelta(ui_core::winit::dpi::PhysicalPosition::new(0.0, dy)),
        phase,
    };
    app.harness()
        .send(&ui_core::synthetic_input::cursor_moved(at.0, at.1));
    app.settle();
    let mut report = Report {
        label: "glide frame".to_owned(),
        samples: Vec::with_capacity(frames),
        phases: BTreeMap::new(),
        frames: Vec::new(),
    };
    app.harness().begin_profiling();
    let mut down = true;
    for _ in 0..8 {
        let dy = if down { -30.0 } else { 30.0 };
        down = !down;
        app.harness().send(&wheel(TouchPhase::Started, 0.0));
        for _ in 0..6 {
            app.harness().send(&wheel(TouchPhase::Moved, dy));
        }
        app.harness().send(&wheel(TouchPhase::Ended, 0.0));
        while report.samples.len() < frames {
            let start = ui_core::time::now();
            let (ran, scopes) = tilo_resource::profiling::observe(|| app.frame());
            if !ran {
                break;
            }
            report
                .samples
                .push(u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX));
            for scope in scopes {
                let entry = report.phases.entry(scope.phase).or_default();
                entry.0 = entry.0.saturating_add(scope.elapsed_ns);
                entry.1 = entry.1.saturating_add(scope.calls);
            }
            // The glide advances on the toolkit's clock: a frame's worth.
            std::thread::sleep(std::time::Duration::from_millis(4));
        }
        if report.samples.len() >= frames {
            break;
        }
    }
    report.frames = app.harness().take_profile_samples();
    report
}

/// Keystrokes into the field at `field`: one click focuses it, then each
/// step types one character of `text`, cycling.
///
/// # Panics
/// When `text` is empty.
pub fn keystrokes<A: ResourceApp>(
    app: &mut HeadlessApp<A>,
    field: (f64, f64),
    text: &str,
    steps: usize,
) -> Report {
    let chars: Vec<char> = text.chars().collect();
    assert!(!chars.is_empty(), "keystrokes need text to type");
    app.harness().click_at(field.0, field.1);
    app.settle();
    measure(app, "keystroke", (2, steps), |app, index| {
        let ch = chars
            .get(index.checked_rem(chars.len()).unwrap_or(0))
            .copied()
            .unwrap_or(' ');
        app.harness().type_text(&ch.to_string());
    })
}

/// View switches: each step clicks the next of `targets`, cycling, after
/// one unmeasured round so each view has been shown once.
///
/// # Panics
/// With fewer than two targets: a switch needs somewhere to go.
pub fn switch_views<A: ResourceApp>(
    app: &mut HeadlessApp<A>,
    label: &str,
    targets: &[Target],
    rounds: usize,
) -> Report {
    assert!(
        targets.len() >= 2,
        "a view switch needs two views, found {targets:?}"
    );
    let at: Vec<(f64, f64)> = targets.iter().map(|target| target.at).collect();
    measure(
        app,
        label,
        (at.len(), at.len().saturating_mul(rounds)),
        |app, index| {
            if let Some(&(x, y)) = at.get(index.checked_rem(at.len()).unwrap_or(0)) {
                app.harness().click_at(x, y);
            }
        },
    )
}

/// Clicks at `at` that change nothing an app shows: an empty spot. What
/// every click costs before it does anything.
pub fn idle_clicks<A: ResourceApp>(
    app: &mut HeadlessApp<A>,
    at: (f64, f64),
    steps: usize,
) -> Report {
    measure(app, "idle click", (2, steps), |app, _| {
        app.harness().click_at(at.0, at.1);
    })
}

/// The interactions [`profile`] measured; `None` where the app's capture
/// offers nothing of that kind to drive.
pub struct Profile {
    /// Wheel notches over the largest list or table, else mid-window.
    pub scroll: Report,
    /// The frames a touchpad fling glides on, at the same place.
    pub glide: Report,
    /// Keystrokes into the first text field.
    pub keystroke: Option<Report>,
    /// Clicks through the sidebar rows or tabs.
    pub view_switch: Option<Report>,
    /// Clicks on list items: selecting a message, a file, a row.
    pub item_click: Option<Report>,
}

/// Every interaction [`profile`] finds in `A`'s capture of `scenario`,
/// measured and printed with the app's id: what scrolling, typing,
/// switching views and selecting items cost on the UI thread.
///
/// # Errors
/// Returns the capture's launch failure.
pub fn profile<A: ResourceApp>(scenario: Option<&str>) -> Result<Profile, LaunchError> {
    let id = A::ID;
    let fresh = || open::<A>(scenario);
    let mut app = fresh()?;
    let lists = targets_with_area(&mut app, &[Role::List, Role::Table, Role::Tree, Role::Grid]);
    let scroll_at = lists.iter().max_by(|a, b| a.1.total_cmp(&b.1)).map_or_else(
        || (f64::from(YOGA.0) * 0.6, f64::from(YOGA.1) * 0.5),
        |(target, _)| target.at,
    );
    let mut scroll = scroll(&mut app, scroll_at, 40);
    scroll.label = format!("{id} scroll step");
    scroll.print();
    let mut glide = glide(&mut app, scroll_at, 60);
    glide.label = format!("{id} glide frame");
    glide.print();

    let mut app = fresh()?;
    let views = {
        let rows = targets(&mut app, &[Role::TreeItem, Role::Tab]);
        rows.into_iter().take(6).collect::<Vec<_>>()
    };
    let view_switch = (views.len() >= 2).then(|| {
        let report = switch_views(&mut app, &format!("{id} view switch"), &views, 3);
        report.print();
        report
    });

    let mut app = fresh()?;
    let items: Vec<Target> = targets(&mut app, &[Role::ListItem, Role::Row])
        .into_iter()
        .take(6)
        .collect();
    let item_click = (items.len() >= 2).then(|| {
        let report = switch_views(&mut app, &format!("{id} item click"), &items, 3);
        report.print();
        report
    });

    let mut app = fresh()?;
    let field = targets(
        &mut app,
        &[Role::SearchInput, Role::TextInput, Role::MultilineTextInput],
    )
    .into_iter()
    .next();
    let keystroke = field.map(|field| {
        let mut report = keystrokes(&mut app, field.at, "the quick brown fox", 24);
        report.label = format!("{id} keystroke");
        report.print();
        report
    });
    Ok(Profile {
        scroll,
        glide,
        keystroke,
        view_switch,
        item_click,
    })
}

/// [`targets`] of `roles` with the area of each one's bounds.
fn targets_with_area<A: ResourceApp>(
    app: &mut HeadlessApp<A>,
    roles: &[Role],
) -> Vec<(Target, f64)> {
    let (width, height) = (f64::from(YOGA.0), f64::from(YOGA.1));
    app.harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| roles.contains(&node.role()))
        .filter_map(|(_, node)| {
            let bounds = node.bounds()?;
            // The part of it inside the window.
            let (x0, y0) = (bounds.x0.max(0.0), bounds.y0.max(0.0));
            let (x1, y1) = (bounds.x1.min(width), bounds.y1.min(height));
            (x1 > x0 && y1 > y0).then(|| {
                (
                    Target {
                        label: node.label().unwrap_or_default().to_owned(),
                        at: (f64::midpoint(x0, x1), f64::midpoint(y0, y1)),
                    },
                    (x1 - x0) * (y1 - y0),
                )
            })
        })
        .collect()
}
