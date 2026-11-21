//! CPU-cost budgets for the interactions a person feels in Settings: opening,
//! switching pages, scrolling and typing.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::manual_midpoint,
    reason = "timing statistics over bounded sample counts: microsecond casts, percentile indices and medians of a few hundred samples"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::print_stderr,
    reason = "a benchmark reports on stderr and fails loudly"
)]

use std::time::{Duration, Instant};

use settings::categories::Category;

const W: f32 = 1472.0;
const H: f32 = 920.0;

/// One measured step's start on both clocks: the wall clock, what the step
/// took, and the test thread's CPU clock, what it cost the thread that runs
/// the UI. The budgets hold the CPU figure: on a machine busy with other
/// work wall time also counts every moment other processes held the core,
/// so it reads a step that did no more work as several times its cost; CPU
/// time still grows with every instruction the step runs.
struct Step {
    wall: Instant,
    cpu: Option<Duration>,
}

impl Step {
    fn start() -> Self {
        Self {
            cpu: ui::rt::thread_cpu_time(),
            wall: Instant::now(),
        }
    }
}

#[derive(Default)]
struct Samples {
    cpu: Vec<u64>,
    wall: Vec<u64>,
}

impl Samples {
    fn push_per(&mut self, t: &Step, per: u64) {
        let wall = t.wall.elapsed();
        let cpu = t
            .cpu
            .zip(ui::rt::thread_cpu_time())
            .map_or(wall, |(start, now)| now.saturating_sub(start));
        self.cpu.push(cpu.as_micros() as u64 / per);
        self.wall.push(wall.as_micros() as u64 / per);
    }
}

fn summary(label: &str, samples: &mut Samples) -> (u64, u64) {
    let stats = |samples: &mut [u64]| {
        samples.sort_unstable();
        let at = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
        (at(0.5), at(0.99), samples[samples.len() - 1])
    };
    let (p50, p99, max) = stats(&mut samples.cpu);
    let (wall_p50, wall_p99, wall_max) = stats(&mut samples.wall);
    eprintln!(
        "perf {label}: n={} cpu p50={p50}us p99={p99}us max={max}us, wall p50={wall_p50}us p99={wall_p99}us max={wall_max}us",
        samples.cpu.len()
    );
    (p50, p99)
}

fn passes(label: &str, frames: &[ui::profiling::FrameTiming]) {
    let n = frames.len().max(1) as u64;
    let sum = |f: fn(&ui::profiling::FrameTiming) -> u64| frames.iter().map(f).sum::<u64>() / n;
    eprintln!(
        "perf {label} passes over {} frames (mean us): sync={} build={} apply={} draw={} event={} resources={}",
        frames.len(),
        sum(|f| f.sync_us),
        sum(|f| f.build_layout_us),
        sum(|f| f.apply_layout_us),
        sum(|f| f.draw_us),
        sum(|f| f.handle_event_us),
        sum(|f| f.resource_availability_us),
    );
}

fn budget(label: &str, p50: u64, limit_us: u64) {
    if !cfg!(debug_assertions) {
        assert!(
            p50 <= limit_us,
            "{label}: p50 {p50}us exceeds the {limit_us}us CPU budget"
        );
    }
}

fn mount() -> crate::support::Window {
    mount_on(Category::opening())
}

fn mount_on(category: Category) -> crate::support::Window {
    crate::support::window_on(category, (W, H))
}

fn switch(app: &mut crate::support::Window, category: Category) {
    app.effect(&settings::effects::open_pane(category))
        .expect("the pane opens");
    app.harness().relayout();
}

#[test]
fn page_switch() {
    let mut app = mount();
    let cats = [
        Category::Sound,
        Category::Displays,
        Category::Network,
        Category::Bluetooth,
        Category::DeviceMesh,
    ];
    for c in cats {
        switch(&mut app, c);
    }
    let mut samples = Samples::default();
    app.harness().begin_profiling();
    for _ in 0..4 {
        for c in cats {
            let t = Step::start();
            switch(&mut app, c);
            samples.push_per(&t, 1);
            assert_eq!(
                crate::support::selected(&mut app),
                c,
                "the switch must show the page"
            );
        }
    }
    passes("page switch", &app.harness().take_profile_samples());
    let (p50, _) = summary("settings page switch", &mut samples);
    budget("page switch", p50, 8_000);
}

#[test]
fn drawn_pages_switch() {
    let mut app = mount();
    let cats = [
        Category::TimeDate,
        Category::Power,
        Category::Appearance,
        Category::Keyboard,
        Category::Sound,
    ];
    for c in cats {
        switch(&mut app, c);
    }
    let mut samples = Samples::default();
    app.harness().begin_profiling();
    for _ in 0..4 {
        for c in cats {
            let t = Step::start();
            switch(&mut app, c);
            samples.push_per(&t, 1);
            assert_eq!(
                crate::support::selected(&mut app),
                c,
                "the switch must show the page"
            );
        }
    }
    passes("drawn page switch", &app.harness().take_profile_samples());
    let (p50, _) = summary("settings drawn page switch", &mut samples);
    budget("drawn page switch", p50, 8_000);
}

#[test]
fn accessibility_tree_per_frame() {
    let mut app = crate::support::window((W as u32, H as u32));
    for category in [
        Category::Sound,
        Category::Displays,
        Category::Network,
        Category::Keyboard,
    ] {
        crate::support::click_category(&mut app, category);
        assert_eq!(crate::support::selected(&mut app), category);
        let h = app.harness();
        let mut samples = Samples::default();
        let mut nodes = 0;
        for _ in 0..10 {
            let t = Step::start();
            nodes = h.a11y_nodes().len();
            samples.push_per(&t, 1);
        }
        let p50 = summary(
            &format!("settings a11y tree ({category:?}, {nodes} nodes)"),
            &mut samples,
        )
        .0;
        budget("accessibility tree per frame", p50, 2_000);
    }
}

/// The production app: the framework host around Settings, with the published
/// action surface and accessibility export that a bare widget root lacks.
///
/// These report without a budget on purpose: a hosted page switch costs
/// several times the bare widget's (the host republishes the action surface
/// and resources after every event) and that gap is not fixed yet. Only the
/// accessibility export is held to a budget.
mod hosted {
    use super::{Samples, Step, budget, summary};
    use settings::SettingsShell;
    use ui::accesskit::Role;
    use ui::framework::testing::TestApp;

    fn hosted() -> TestApp<SettingsShell> {
        let mut app = TestApp::<SettingsShell>::new().expect("settings opens headlessly");
        app.settle();
        app.harness().resize((1472, 920)).unwrap();
        app.settle();
        app
    }

    fn tree_item_centres(app: &mut TestApp<SettingsShell>) -> Vec<(String, f64, f64)> {
        app.harness()
            .a11y_nodes()
            .into_iter()
            .filter(|(_, node)| node.role() == Role::TreeItem)
            .filter_map(|(_, node)| {
                let b = node.bounds()?;
                Some((
                    node.label()?.to_owned(),
                    (b.x0 + b.x1) / 2.0,
                    (b.y0 + b.y1) / 2.0,
                ))
            })
            .collect()
    }

    #[test]
    fn hosted_page_switch_and_tree() {
        let mut app = hosted();
        let rows = tree_item_centres(&mut app);
        assert!(rows.len() > 6, "the sidebar lists the categories");
        let (mut switch, mut tree) = (Samples::default(), Samples::default());
        for round in 0..3 {
            for (_, x, y) in rows.iter().skip(1).take(6) {
                let t = Step::start();
                app.harness().click_at(*x, *y);
                app.settle();
                if round > 0 {
                    switch.push_per(&t, 1);
                }
                let t = Step::start();
                let nodes = app.harness().a11y_nodes().len();
                if round > 0 {
                    tree.push_per(&t, 1);
                }
                let _ = nodes;
            }
        }
        let _ = summary("hosted settings page switch (click + settle)", &mut switch);
        let p50 = summary("hosted settings accessibility tree export", &mut tree).0;
        budget("hosted accessibility export", p50, 2_000);
    }
}
