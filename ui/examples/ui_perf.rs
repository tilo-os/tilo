//! UI frame profiling and benchmark scenario runner.

#[path = "common/gallery_scene.rs"]
mod gallery_scene;
#[path = "registry.rs"]
mod registry;

registry::scenario_modules! {
    Perf;
    dashboard_perf_bench, "dashboard_perf_bench.rs", "dashboard stress benchmark";
    perf_scenario, "perf_scenario.rs", "headless pass-level profiling";
    widget_perf_bench, "widget_perf_bench.rs", "widget construction and rendering benchmark";
}

fn main() {
    registry::dispatch(registry::Surface::Perf, SCENARIOS, Some("perf_scenario"));
}
