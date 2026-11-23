//! Interactive UI component and state-machine catalog.

#[path = "common/dashboard_scene.rs"]
mod dashboard_scene;
#[path = "registry.rs"]
mod registry;

registry::scenario_modules! {
    Lab;
    dashboard, "dashboard.rs", "dashboard composition";
}

fn main() {
    registry::dispatch(registry::Surface::Lab, SCENARIOS, Some("dashboard"));
}
