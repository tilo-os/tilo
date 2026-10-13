//! Interactive UI component and state-machine catalog.

#[path = "common/chart_gallery_scene.rs"]
mod chart_gallery_scene;
#[path = "common/gallery_scene.rs"]
mod gallery_scene;
#[path = "registry.rs"]
mod registry;

registry::scenario_modules! {
    Lab;
    activity_timeline, "activity_timeline.rs", "collaborative history timeline";
    chart_gallery, "chart_gallery.rs", "interactive responsive chart catalogue";
    dashboard, "dashboard.rs", "dashboard composition";
    gallery, "gallery.rs", "complete interactive component gallery";
    responsive_gallery, "responsive_gallery.rs", "responsive layout gallery";
}

fn main() {
    registry::dispatch(registry::Surface::Lab, SCENARIOS, Some("gallery"));
}
