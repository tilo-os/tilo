//! Deterministic screenshot and visual-regression scenario runner.

#[path = "common/chart_gallery_scene.rs"]
mod chart_gallery_scene;
#[path = "common/gallery_scene.rs"]
mod gallery_scene;
#[path = "registry.rs"]
mod registry;

registry::scenario_modules! {
    Shots;
    ctrl_k_palette, "ctrl_k_palette.rs", "unified Ctrl+K palette states";
    chart_gallery_shots, "chart_gallery_shots.rs", "responsive light and dark chart catalogue";
    content_shots, "content_shots.rs", "content composites: rows, cards, media, stats, states";
    data_table, "data_table.rs", "virtualized data-table states";
    denied_state_shots, "denied_state_shots.rs", "permission-denied control states";
    fractional_scale_shots, "fractional_scale_shots.rs", "fractional-scale gallery renders";
    floating_surface_shots, "floating_surface_shots.rs", "soft floating call and navigation surfaces";
    gallery_harness, "gallery_harness_shots.rs", "gallery in a FrameHarness, dark and light, PNG + inspection";
    gallery_shots, "gallery_shots.rs", "dark and light component gallery";
    history_cases, "history_cases.rs", "history and collaboration cases";
    nav_shell_shots, "nav_shell_shots.rs", "navigation shell states";
    perf_hud_shot, "perf_hud_shot.rs", "frame profiler HUD";
    scrollbar_shots, "scrollbar_shots.rs", "scrollbar states";
    search_showcase, "search_showcase.rs", "search-field states and results";
    soft_shadow_shots, "soft_shadow_shots.rs", "soft-shadow rendering";
    table_shots, "table_shots.rs", "editable table insertion and deletion";
    text_shots, "text_shots.rs", "typography rendering";
    textinput_ext_shots, "textinput_ext_shots.rs", "extended text input states";
    undo_controls_shots, "undo_controls_shots.rs", "undo and redo availability states";
    widget_states, "widget_states_shots.rs", "every widget in its states, dark and light, with overlays over content";
    window_chrome, "window_chrome_shots.rs", "modal, popover, status and over-media chrome, dark and light";
}

fn main() {
    registry::dispatch(registry::Surface::Shots, SCENARIOS, None);
}
