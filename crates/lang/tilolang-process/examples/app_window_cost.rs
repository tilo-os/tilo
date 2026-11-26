//! `cargo run --release -p tilolang-process --example app_window_cost -- <package dir> [count ...]`:
//! what an open tilolang app costs, split as the app window design splits it
//! (`tilolang_tilo::instance` and `tilo_app_window::AppWindow` over the patch protocol), in one
//! process: each half measured on its own.
//!
//! - **App host**: `count` instances of the package (its runtime over a workspace document of its
//!   own, its views, what it publishes on the API), each sending its window patches. The
//!   anonymous memory they add, per instance.
//! - **app window**: a window for each, built from what its instance sent, laid out at 1280x800
//!   and drawn into a scene, with one text renderer for all (the app window's one glyph cache).
//!   The anonymous memory they add, per window; rasterizing (the window's target and buffers)
//!   is not counted.
//! - **First frame**: an instance started and its window built, laid out and drawn, cold (the
//!   first in the process) and warm (as a app window and an app host that already run).
//! - **Idle**: the threads of the process, and the context switches all of them make in five
//!   seconds with every instance and window open and nothing happening.
//!
//! Each count is cumulative: `1 10 30` opens 1, then 9 more, then 20 more. With
//! `--scenario`, each workspace holds the package's first scenario rather than nothing.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_app_window::AppWindow;
use tilo_render_protocol::patch::local::{End, pair};
use tilo_ui_base::widget::DrawChild;
use tilo_ui_base::widget::Widget as _;
use tilolang_tilo::host::Host;
use tilolang_tilo::instance::{
    Config, GlobalLocale, Instance, Locale, Mode, Platform, Publication,
};
use tilolang_tilo::publish::{Planner, Publications};
use tilolang_tilo::runtime::{Actor, NameHost};
use tilolang_tilo::tilolang::Compiled;

/// The package and what it uses, compiled once for every instance.
struct Packages(Rc<Vec<Compiled>>);

/// What a live instance publishes, as an app host publishes it.
struct Published {
    host: Host,
    planner: Arc<dyn Planner>,
    publications: RefCell<Publications>,
}

impl Publication for Published {
    fn update(&self) {
        self.publications
            .borrow_mut()
            .update(&self.host, &self.planner);
    }

    fn pending(&self) -> bool {
        // Nothing is read off the frame: the store answers every read as it is made.
        false
    }
}

/// Shares the toolkit's locale/format globals; `now` is pinned below so the instance's runtime
/// (`now`, `today`, `shown` and `sentence`) evaluates against a fixed instant.
static BENCH_LOCALE: GlobalLocale = tilo_app_window::platform_locale!(GlobalLocale);

impl Platform for Packages {
    fn load(&self, host: &Host) -> u64 {
        for compiled in self.0.iter() {
            if let Err(error) = host.load_compiled(compiled) {
                eprintln!("a package did not load: {error}");
            }
        }
        0
    }

    fn locale(&self) -> Option<&dyn Locale> {
        Some(&BENCH_LOCALE)
    }

    fn phrase(&self, id: &str, args: &[(String, String)]) -> String {
        tilo_app_window::i18n::phrase(id, args)
    }

    /// Fixed instant for the instance's runtime (`now`, `today`, `shown` and `sentence`); widget
    /// date controls on the `AppWindow` side still read `tilo_ui_base::clock`.
    fn now(&self) -> String {
        "2026-10-27T10:00:00Z".to_owned()
    }

    fn waker(&self) -> tilolang_tilo::background::Waker {
        Arc::new(|| {})
    }

    fn publish(&self, host: &Host) -> Option<Box<dyn Publication>> {
        let planner: Arc<dyn Planner> = Arc::new(host.handle());
        let mut publications = Publications::new(host);
        publications.update(host, &planner);
        Some(Box::new(Published {
            host: host.clone(),
            planner,
            publications: RefCell::new(publications),
        }))
    }
}

/// The names of a workspace nobody else is in.
struct Alone;

impl NameHost for Alone {
    fn name(&self, who: &Actor) -> String {
        who.id().to_owned()
    }
}

/// One app open: its instance over a workspace document of its own, and the window end of its
/// connection.
struct Open {
    instance: Instance,
    window: Option<End>,
}

fn open(app: &str, platform: &Rc<Packages>, scenario: Option<&str>) -> Open {
    let (app_end, window_end) = pair();
    let mut instance = Instance::new(
        Config {
            app: app.to_owned(),
            me: "ada".into(),
            mode: Mode::Live,
            device: None,
            store: tilolang_tilo::resources::ResourceStore::in_memory(),
            sink: Some(Box::new(tilolang_tilo::runtime::MemorySink::new())),
            root: tilolang_tilo::host::RootView::default(),
            link_home: None,
        },
        Rc::clone(platform) as Rc<dyn Platform>,
        Rc::new(Alone),
        Box::new(app_end),
    );
    instance.sync();
    if scenario.is_some() {
        instance.stage(scenario);
        instance.sync();
    }
    instance.update();
    Open {
        instance,
        window: Some(window_end),
    }
}

/// A window built from what its instance sent, laid out and drawn into a scene.
struct Shown {
    window: AppWindow,
    scene: ui::vello::Scene,
}

fn show(end: End, text: &mut tilo_ui_base::TextRenderer, theme: &tilo_ui_base::Theme) -> Shown {
    let mut window = AppWindow::new(Box::new(end), Vec::new());
    window.receive();
    lay_out(&mut window, theme);
    let mut scene = ui::vello::Scene::new();
    window.draw_child(
        &mut scene,
        ui::kurbo::Affine::IDENTITY,
        text,
        theme,
        &tilo_ui_base::presence::PresenceCtx::default().draw_env(),
    );
    Shown { window, scene }
}

fn lay_out(window: &mut AppWindow, theme: &tilo_ui_base::Theme) {
    use ui::taffy::prelude::{AvailableSpace, Size};
    let mut taffy = ui::taffy::TaffyTree::new();
    let mut nodes = std::collections::HashMap::new();
    let node = window.build_layout(&mut taffy, theme, &mut nodes);
    let _ = taffy.compute_layout(
        node,
        Size {
            width: AvailableSpace::Definite(1280.0),
            height: AvailableSpace::Definite(800.0),
        },
    );
    window.apply_layout(&taffy, node, ui::kurbo::Point::ZERO);
}

/// A field of `/proc/self/status`, in kB or as a count.
fn status(field: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix(field))
                .and_then(|rest| rest.split_whitespace().next()?.parse().ok())
        })
        .unwrap_or(0)
}

/// Context switches every thread of the process made so far.
fn switches() -> u64 {
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
        return 0;
    };
    tasks
        .flatten()
        .filter_map(|task| std::fs::read_to_string(task.path().join("status")).ok())
        .map(|text| {
            text.lines()
                .filter(|line| line.contains("ctxt_switches"))
                .filter_map(|line| line.split_whitespace().nth(1)?.parse::<u64>().ok())
                .sum::<u64>()
        })
        .sum()
}

fn threads() -> usize {
    std::fs::read_dir("/proc/self/task").map_or(0, Iterator::count)
}

fn kib(kb: u64) -> f64 {
    f64::from(u32::try_from(kb).unwrap_or(u32::MAX)) / 1024.0
}

fn per(total_kb: u64, count: usize) -> f64 {
    kib(total_kb) / f64::from(u32::try_from(count).unwrap_or(u32::MAX))
}

/// The first frame, cold (the first instance and window of the process) and
/// warm (as a app window and an app host that already run).
fn first_frame(
    app: &str,
    platform: &Rc<Packages>,
    scenario: Option<&str>,
    text: &mut tilo_ui_base::TextRenderer,
    theme: &tilo_ui_base::Theme,
) -> Result<(), String> {
    let started = Instant::now();
    let mut first = open(app, platform, scenario);
    let opened = started.elapsed();
    let shown = show(first.window.take().ok_or("no window")?, text, theme);
    let cold = started.elapsed();
    println!(
        "a window shows {} nodes, {} rows ({} paths drawn)",
        shown
            .window
            .composed()
            .map_or(0, tilo_app_window::UiNode::count),
        shown
            .window
            .lists()
            .iter()
            .filter_map(|list| shown.window.rows_of(list))
            .map(|rows| rows.borrow().rendered().len())
            .sum::<usize>(),
        shown.scene.encoding().n_paths
    );
    drop((shown, first));
    // And warm, as a app window and an app host that already run.
    let mut warm = Vec::new();
    for _ in 0..5 {
        let started = Instant::now();
        let mut again = open(app, platform, scenario);
        let opened = started.elapsed();
        let shown = show(again.window.take().ok_or("no window")?, text, theme);
        warm.push((opened, started.elapsed()));
        drop((shown, again));
    }
    warm.sort();
    let (warm_open, warm_frame) = warm.get(warm.len() / 2).copied().ok_or("no warm frame")?;
    println!(
        "first frame: cold {:.1} ms (instance {:.1} ms), warm {:.1} ms (instance {:.1} ms)",
        cold.as_secs_f64() * 1000.0,
        opened.as_secs_f64() * 1000.0,
        warm_frame.as_secs_f64() * 1000.0,
        warm_open.as_secs_f64() * 1000.0,
    );
    Ok(())
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (scenario, args) = match args.split_first() {
        Some((flag, rest)) if flag == "--scenario" => (true, rest.to_vec()),
        _ => (false, args),
    };
    let (dir, counts) = args
        .split_first()
        .ok_or("usage: app_window_cost [--scenario] <package dir> [count ...]")?;
    let counts: Vec<usize> = if counts.is_empty() {
        vec![1, 10, 30]
    } else {
        counts
            .iter()
            .map(|count| count.parse().map_err(|_| format!("`{count}` is no count")))
            .collect::<Result<_, _>>()?
    };
    let dir = std::path::PathBuf::from(dir);
    let dirs = tilolang::source::dependency_order(&dir, &[])
        .map_err(|why| format!("the package's sources resolve: {why}"))?;
    let packages = tilolang::source::compile_dirs(&dirs, &[], &mut tilolang::OsEntropy::default())
        .map_err(|why| format!("the package compiles: {why}"))?
        .into_iter()
        .map(|built| built.compiled)
        .collect::<Vec<_>>();
    let app = packages
        .last()
        .map(|main| main.meta.app.clone())
        .ok_or("no package")?;
    // With `--scenario`, each instance's workspace holds the package's first scenario.
    let scenario = scenario
        .then(|| {
            packages
                .last()?
                .meta
                .scenarios
                .first()
                .map(|s| s.name.clone())
        })
        .flatten();
    let platform = Rc::new(Packages(Rc::new(packages)));
    tilo_app_window::i18n::ensure();
    let theme = tilo_ui_base::Theme::dark();
    let mut text = tilo_ui_base::TextRenderer::new();

    first_frame(&app, &platform, scenario.as_deref(), &mut text, &theme)?;

    let mut apps: Vec<Open> = Vec::new();
    let mut windows: Vec<Shown> = Vec::new();
    let base = status("RssAnon:");
    let mut instances_kb: u64 = 0;
    let mut windows_kb: u64 = 0;
    println!("open  app-process-total  per-instance  app-window-total  per-window  threads");
    for count in counts {
        while apps.len() < count {
            let before = status("RssAnon:");
            apps.push(open(&app, &platform, scenario.as_deref()));
            instances_kb = instances_kb.saturating_add(status("RssAnon:").saturating_sub(before));
        }
        for opened in &mut apps {
            if let Some(end) = opened.window.take() {
                let before = status("RssAnon:");
                windows.push(show(end, &mut text, &theme));
                windows_kb = windows_kb.saturating_add(status("RssAnon:").saturating_sub(before));
            }
        }
        println!(
            "{count:>4}  {:>11.2} MiB  {:>9.2} MiB  {:>10.2} MiB  {:>7.2} MiB  {:>7}",
            kib(instances_kb),
            per(instances_kb, count),
            kib(windows_kb),
            per(windows_kb, count),
            threads(),
        );
    }
    println!(
        "process anonymous memory: {:.1} MiB before the counted apps, {:.1} MiB with them",
        kib(base),
        kib(status("RssAnon:"))
    );

    // Idle: every app open, nothing happening.
    let before = switches();
    std::thread::sleep(Duration::from_secs(5));
    let idle = switches().saturating_sub(before);
    println!(
        "idle: {} threads, {idle} context switches in 5 s with {} apps open",
        threads(),
        apps.len()
    );
    let refused = windows
        .iter()
        .filter(|shown| shown.window.failed().is_some())
        .count();
    let paths: u32 = windows
        .iter()
        .map(|shown| shown.scene.encoding().n_paths)
        .sum();
    println!(
        "{} windows of {} drawn ({paths} paths), {refused} refused what their instance sent",
        windows.len(),
        apps.first().map_or("", |opened| opened.instance.app()),
    );
    Ok(())
}
