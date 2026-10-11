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
    channel_browser, "channel_browser.rs", "channel and participant browser";
    chart_gallery, "chart_gallery.rs", "interactive responsive chart catalogue";
    collaboration_overview, "collaboration_overview.rs", "live presence and away recap";
    conflict_picker, "conflict_picker.rs", "conflict resolution picker";
    dashboard, "dashboard.rs", "dashboard composition";
    gallery, "gallery.rs", "complete interactive component gallery";
    responsive_gallery, "responsive_gallery.rs", "responsive layout gallery";
}

fn main() {
    registry::dispatch(registry::Surface::Lab, SCENARIOS, Some("gallery"));
}
