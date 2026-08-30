//! Interactive UI component and state-machine catalog.

#[path = "common/gallery_scene.rs"]
mod gallery_scene;
#[path = "registry.rs"]
mod registry;

registry::scenario_modules! {
    Lab;
    activity_timeline, "activity_timeline.rs", "collaborative history timeline";
    channel_browser, "channel_browser.rs", "channel and participant browser";
    conflict_picker, "conflict_picker.rs", "conflict resolution picker";
    dashboard, "dashboard.rs", "dashboard composition";
    gallery, "gallery.rs", "complete interactive component gallery";
    identity_flow_demo, "identity_flow_demo.rs", "identity onboarding and recovery flows";
    responsive_gallery, "responsive_gallery.rs", "responsive layout gallery";
    widget_showcase, "widget_showcase.rs", "legacy widget showcase";
}

fn main() {
    registry::dispatch(registry::Surface::Lab, SCENARIOS, Some("gallery"));
}
