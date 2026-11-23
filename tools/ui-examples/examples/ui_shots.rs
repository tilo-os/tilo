//! Deterministic screenshot and visual-regression scenario runner.

#[path = "common/dashboard_scene.rs"]
mod dashboard_scene;
#[path = "registry.rs"]
mod registry;

registry::scenario_modules! {
    Shots;
    perf_hud_shot, "perf_hud_shot.rs", "frame profiler HUD";
}

fn main() {
    registry::dispatch(registry::Surface::Shots, SCENARIOS, None);
}
