//! Headless performance-scenario runner — the CLI face of
//! [`ui::profiling`].
//!
//! Drives a `ui` app through a scripted interaction with **no GPU and no
//! window**, timing each of the five frame passes (`sync`, `build_layout`,
//! `apply_layout`, `draw`, `handle_event`) and printing frame-time statistics
//! (mean / p50 / p95 / max per pass, plus how many frames blew the 60 fps
//! budget). This is the objective measurement a perf fix is judged against and
//! the guard a regression trips.
//!
//! Two targets are run to show the harness works against different app shapes:
//!
//!   * `list-scroll`   — a tall scrollable list, scrolled N notches. The
//!     canonical scroll scenario: it surfaces per-frame
//!     layout + draw cost.
//!   * `gallery-steady`— the reference component gallery (`common/gallery_scene`),
//!     re-rendered N frames with no input: the steady-state
//!     redraw cost of a dense screen.
//!
//! Run:
//! ```text
//! cargo run -p ui --release --example ui-perf -- perf_scenario
//! cargo run -p ui --release --example ui-perf -- perf_scenario --count 240 --json
//! ```
//!
//! Any app author gets the same measurement over their own root with one call:
//! `ui::profiling::run_scenario_on(state, app_root(), &scenario)`.

use ui::profiling::{PerfScenario, PerfStep, run_scenario_on};
use ui::state::State;
use ui::theme::Theme;
use ui::widget::{Column, Scrollable, Text, Widget};

use super::gallery_scene;

/// A tall scrollable list — a realistic "feed / notes" app shape, and a
/// reliable scroll target (its content overflows the viewport).
fn list_app(rows: usize) -> (State, Box<dyn Widget>) {
    let state = State::new();
    let mut col = Column::new().with_intrinsic_height();
    for i in 0..rows {
        col = col.push(Box::new(Text::new(&format!(
            "Row {i:>3} — the quick brown fox jumps over the lazy dog, then does it again"
        ))));
    }
    let root = Scrollable::new_vertical().with_child(Box::new(col));
    (state, Box::new(root))
}

pub fn run(args: &[String]) {
    ui::i18n::I18nManager::init("en-US");
    let count = arg_value(args, "--count")
        .and_then(|v| v.parse().ok())
        .unwrap_or(120usize);
    let json = args.iter().any(|a| a == "--json");

    // 1) Scroll scenario against the scrollable list.
    let (state, root) = list_app(200);
    let scroll = PerfScenario::scroll("list-scroll", count);
    let report = run_scenario_on(state, root, &scroll);
    if json {
        println!("{}", report.to_json());
    } else {
        print!("{report}");
    }

    // 2) Steady-state redraw against the reference component gallery.
    let mut g_state = State::new();
    let g_root = gallery_scene::build_showcase(&mut g_state, &Theme::dark());
    let gallery = PerfScenario {
        name: "gallery-steady".to_string(),
        viewport: (1280, 800),
        warmup: 3,
        steps: vec![PerfStep::Frames(count)],
    };
    let g_report = run_scenario_on(g_state, g_root, &gallery);
    if json {
        println!("{}", g_report.to_json());
    } else {
        print!("{g_report}");
    }
}

fn arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == flag {
            return it.next().map(String::as_str);
        }
        if let Some(rest) = a.strip_prefix(&format!("{flag}=")) {
            return Some(rest);
        }
    }
    None
}
