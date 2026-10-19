//! CPU-cost budgets for the interactions a person feels in Settings: opening,
//! switching pages, scrolling and typing.
//!
//! Every interaction runs through the production host path (`FrameHarness`
//! sends the real winit events, then runs sync, layout and scene encoding), at
//! the Yoga's logical viewport (2944x1840 physical at scale 2). Budgets only
//! apply to optimised builds; a debug build prints the numbers and skips the
//! assertions. Run with:
//! `cargo test -p settings --release --test it perf_budget -- --nocapture`.
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

use std::time::Instant;

use settings::SettingsApp;
use settings::categories::Category;
use ui::headless::{Framed, viewport};

const W: f32 = 1472.0;
const H: f32 = 920.0;

/// p50 / p99 / max of `samples` in microseconds.
fn summary(label: &str, samples: &mut [u64]) -> (u64, u64) {
    samples.sort_unstable();
    let at = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
    let (p50, p99, max) = (at(0.5), at(0.99), samples[samples.len() - 1]);
    eprintln!(
        "perf {label}: n={} p50={p50}us p99={p99}us max={max}us",
        samples.len()
    );
    (p50, p99)
}

/// Mean per-pass cost of the frames a profiled interaction produced.
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

fn mount(config: &crate::support::ScratchConfig) -> Framed<SettingsApp> {
    mount_on(config, Category::DEFAULT)
}

fn mount_on(config: &crate::support::ScratchConfig, category: Category) -> Framed<SettingsApp> {
    Framed::new(
        ui::State::new(),
        config.app().with_category(category).with_width_hint(W),
        viewport((W, H)),
        ui::Theme::dark(),
    )
}

fn click_row(app: &mut Framed<SettingsApp>, category: Category) {
    let rect = crate::support::nav_row(&**app, &category.label())
        .unwrap_or_else(|| panic!("no row for {category:?}"))
        .rect;
    let c = rect.center();
    Framed::harness_mut(app).click_at(c.x, c.y);
}

#[test]
fn cold_start_to_first_frame() {
    let mut samples = Vec::new();
    for _ in 0..5 {
        let config = crate::support::ScratchConfig::new();
        let t = Instant::now();
        let app = mount(&config);
        samples.push(t.elapsed().as_micros() as u64);
        drop(app);
    }
    let (p50, _) = summary("settings cold start (build + first frame)", &mut samples);
    budget("cold start", p50, 150_000);
}

#[test]
fn page_switch() {
    let config = crate::support::ScratchConfig::new();
    let mut app = mount(&config);
    let cats = [
        Category::Sound,
        Category::Displays,
        Category::Network,
        Category::Bluetooth,
        Category::DeviceMesh,
    ];
    // Warm the shaping caches like a person's second visit.
    for c in cats {
        click_row(&mut app, c);
    }
    let mut samples = Vec::new();
    Framed::harness_mut(&mut app).begin_profiling();
    for _ in 0..4 {
        for c in cats {
            let t = Instant::now();
            click_row(&mut app, c);
            samples.push(t.elapsed().as_micros() as u64);
            assert_eq!(app.selected(), c, "the click must switch the page");
        }
    }
    passes(
        "page switch",
        &Framed::harness_mut(&mut app).take_profile_samples(),
    );
    let (p50, _) = summary("settings page switch", &mut samples);
    budget("page switch", p50, 8_000);
}

/// The pages with drawn views (the globe, the charge chart, the theme
/// tiles, the keyboard) switch as fast as the plain ones.
#[test]
fn drawn_pages_switch() {
    let config = crate::support::ScratchConfig::new();
    let mut app = mount(&config);
    let cats = [
        Category::TimeDate,
        Category::Power,
        Category::Appearance,
        Category::Keyboard,
        Category::Sound,
    ];
    for c in cats {
        click_row(&mut app, c);
    }
    let mut samples = Vec::new();
    Framed::harness_mut(&mut app).begin_profiling();
    for _ in 0..4 {
        for c in cats {
            let t = Instant::now();
            click_row(&mut app, c);
            samples.push(t.elapsed().as_micros() as u64);
            assert_eq!(app.selected(), c, "the click must switch the page");
        }
    }
    passes(
        "drawn page switch",
        &Framed::harness_mut(&mut app).take_profile_samples(),
    );
    let (p50, _) = summary("settings drawn page switch", &mut samples);
    budget("drawn page switch", p50, 8_000);
}

/// Dragging the globe redraws it without rebuilding the page: each drag is
/// a press, four moves and a release, reported per event.
#[test]
fn globe_drag_frame() {
    let config = crate::support::ScratchConfig::new();
    let mut app = mount_on(&config, Category::TimeDate);
    let h = Framed::harness_mut(&mut app);
    // The clock is read off the UI thread; the globe is drawn once it lands.
    let globe = tilo_test_env::wait_for("the globe is laid out", || {
        let _ = h.poll_frame();
        h.targets().get("time.timezone-map").copied()
    });
    let c = globe.center();
    let mut samples = Vec::new();
    for i in 0..40 {
        let dx = if i % 2 == 0 { 60.0 } else { -60.0 };
        let t = Instant::now();
        h.drag_at((c.x, c.y), (c.x + dx, c.y + dx / 4.0));
        samples.push(t.elapsed().as_micros() as u64 / 6);
    }
    let (p50, _) = summary("settings globe drag event", &mut samples);
    budget("globe drag event", p50, 4_000);
}

#[test]
fn scroll_frame() {
    let config = crate::support::ScratchConfig::new();
    let mut app = mount_on(&config, Category::Displays);
    let h = Framed::harness_mut(&mut app);
    h.click_at(f64::from(W) - 6.0, f64::from(H) / 2.0);
    for _ in 0..3 {
        h.scroll(0.0, -1.0);
    }
    let mut samples = Vec::new();
    for i in 0..60 {
        let t = Instant::now();
        h.scroll(0.0, if i < 30 { -1.0 } else { 1.0 });
        samples.push(t.elapsed().as_micros() as u64);
    }
    let (p50, _) = summary("settings scroll frame", &mut samples);
    budget("scroll frame", p50, 4_000);
}

#[test]
fn keystroke_in_search() {
    let config = crate::support::ScratchConfig::new();
    let mut app = mount(&config);
    let h = Framed::harness_mut(&mut app);
    h.click_named("settings.search").expect("search field");
    let mut samples = Vec::new();
    h.begin_profiling();
    for ch in "displaysoundnetwork".chars() {
        let t = Instant::now();
        h.type_text(&ch.to_string());
        samples.push(t.elapsed().as_micros() as u64);
    }
    passes("keystroke", &h.take_profile_samples());
    let (p50, _) = summary("settings keystroke", &mut samples);
    budget("keystroke", p50, 4_000);
}

/// Attribution only: where one page switch spends its time.
#[test]
fn page_switch_attribution() {
    let config = crate::support::ScratchConfig::new();
    let mut app = mount(&config);
    for c in [Category::Sound, Category::Displays, Category::Network] {
        click_row(&mut app, c);
    }
    let (mut select, mut layout) = (Vec::new(), Vec::new());
    for _ in 0..5 {
        for c in [Category::Sound, Category::Displays, Category::Network] {
            let t = Instant::now();
            Framed::split(&mut app).0.select(c);
            select.push(t.elapsed().as_micros() as u64);
            let t = Instant::now();
            Framed::harness_mut(&mut app).relayout();
            layout.push(t.elapsed().as_micros() as u64);
        }
    }
    summary("attribution select() (pane build)", &mut select);
    summary("attribution relayout after select", &mut layout);
    let h = Framed::harness_mut(&mut app);
    h.begin_profiling();
    h.click_at(700.0, 400.0);
    for f in h.take_profile_samples() {
        eprintln!("perf idle click frame: {f:?}");
    }
}

/// One drag step: the window changes width and height by a few pixels and the
/// content must be laid out and drawn again.
#[test]
fn resize_step() {
    let config = crate::support::ScratchConfig::new();
    let mut app = mount_on(&config, Category::Displays);
    let h = Framed::harness_mut(&mut app);
    for i in 0..6 {
        h.resize((1472 - i, 920 - i)).unwrap();
    }
    h.begin_profiling();
    let mut samples = Vec::new();
    for i in 0..60u32 {
        let (w, hgt) = (1300 + (i * 7) % 170, 800 + (i * 5) % 110);
        let t = Instant::now();
        h.resize((w, hgt)).unwrap();
        samples.push(t.elapsed().as_micros() as u64);
    }
    passes("resize step", &h.take_profile_samples());
    let (p50, _) = summary("settings resize step", &mut samples);
    budget("resize step", p50, 8_000);
}

/// What a connected screen reader makes every frame cost: the live host builds
/// the whole accessibility tree after each painted frame when an assistive
/// technology is attached.
#[test]
fn accessibility_tree_per_frame() {
    let config = crate::support::ScratchConfig::new();
    let mut app = mount(&config);
    for category in [
        Category::Sound,
        Category::Displays,
        Category::Network,
        Category::Keyboard,
    ] {
        click_row(&mut app, category);
        let h = Framed::harness_mut(&mut app);
        let mut samples = Vec::new();
        let mut nodes = 0;
        for _ in 0..10 {
            let t = Instant::now();
            nodes = h.a11y_nodes().len();
            samples.push(t.elapsed().as_micros() as u64);
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
    use super::{Instant, budget, summary};
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
        let (mut switch, mut tree) = (Vec::new(), Vec::new());
        for round in 0..3 {
            for (_, x, y) in rows.iter().skip(1).take(6) {
                let t = Instant::now();
                app.harness().click_at(*x, *y);
                app.settle();
                if round > 0 {
                    switch.push(t.elapsed().as_micros() as u64);
                }
                let t = Instant::now();
                let nodes = app.harness().a11y_nodes().len();
                if round > 0 {
                    tree.push(t.elapsed().as_micros() as u64);
                }
                let _ = nodes;
            }
        }
        let _ = summary("hosted settings page switch (click + settle)", &mut switch);
        let p50 = summary("hosted settings accessibility tree export", &mut tree).0;
        budget("hosted accessibility export", p50, 2_000);
    }

    #[test]
    fn hosted_scroll_and_keystroke() {
        let mut app = hosted();
        let rows = tree_item_centres(&mut app);
        let (_, x, y) = rows[3].clone();
        app.harness().click_at(x, y);
        app.settle();
        app.harness().click_at(1460.0, 460.0);
        let mut scroll = Vec::new();
        let mut phases: std::collections::BTreeMap<&'static str, (u128, usize)> =
            std::collections::BTreeMap::new();
        for i in 0..60 {
            let t = Instant::now();
            let ((), samples) = ui::tilo_resource::profiling::observe(|| {
                app.harness().scroll(0.0, if i < 30 { -1.0 } else { 1.0 });
                app.settle();
            });
            scroll.push(t.elapsed().as_micros() as u64);
            for sample in samples {
                let entry = phases.entry(sample.phase).or_default();
                entry.0 += sample.elapsed_ns;
                entry.1 += sample.calls;
            }
        }
        for (phase, (ns, calls)) in &phases {
            eprintln!(
                "perf hosted scroll phase {phase}: {}us per step over {calls} calls",
                ns / 60 / 1000
            );
        }
        let _ = summary("hosted settings scroll step", &mut scroll);
        app.harness()
            .click_named("settings.search")
            .expect("search");
        let mut keys = Vec::new();
        for ch in "displaysound".chars() {
            let t = Instant::now();
            app.harness().type_text(&ch.to_string());
            app.settle();
            keys.push(t.elapsed().as_micros() as u64);
        }
        let _ = summary("hosted settings keystroke", &mut keys);
    }

    /// The action surface Settings publishes on a page, measured the way the
    /// bridge sees it: the resource tree the host builds, surveyed into
    /// actions, converted to the wire's exported form and serialised.
    /// (The size is the serialised JSON length, the measure the Notes surface
    /// volume test uses.) No budget: this reports what a publication costs.
    #[test]
    fn hosted_action_surface_cost() {
        let mut app = hosted();
        let rows = tree_item_centres(&mut app);
        for (label, x, y) in rows.iter().skip(1).take(5).cloned() {
            app.harness().click_at(x, y);
            app.settle();
            let (mut resources, mut survey, mut encode) = (Vec::new(), Vec::new(), Vec::new());
            let (mut bytes, mut actions) = (0, 0);
            for _ in 0..8 {
                let t = Instant::now();
                let tree = app.harness().resources();
                resources.push(t.elapsed().as_micros() as u64);
                let t = Instant::now();
                let surveyed = ui::tilo_actions::door::survey(&tree);
                let exported: Vec<ui::collaboration::ExportedAction> = surveyed
                    .iter()
                    .map(ui::collaboration::ExportedAction::from)
                    .collect();
                survey.push(t.elapsed().as_micros() as u64);
                let t = Instant::now();
                bytes = exported
                    .iter()
                    .map(|action| serde_json::to_vec(action).unwrap().len())
                    .sum::<usize>();
                encode.push(t.elapsed().as_micros() as u64);
                actions = exported.len();
            }
            eprintln!("surface [{label}]: {actions} actions, {bytes} bytes per publish");
            summary(&format!("surface resource tree [{label}]"), &mut resources);
            summary(&format!("surface survey+export [{label}]"), &mut survey);
            summary(&format!("surface encode [{label}]"), &mut encode);
        }
    }
}
