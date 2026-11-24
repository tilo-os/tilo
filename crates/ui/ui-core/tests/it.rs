// One test binary instead of one per file: each `tests/*.rs` linked the whole
// crate separately (52 binaries, 14.1 GB, 112 s). Files stay where they are;
// #[path] pulls them in as modules. nextest still runs each test in its own
// process, so tests that touch process state stay isolated.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    clippy::float_cmp,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::disallowed_methods,
    reason = "integration tests may time and wait on the real clock; the rule guards the library"
)]

#[path = "support/mod.rs"]
mod support;

#[path = "accessibility_geometry.rs"]
mod accessibility_geometry;
#[path = "colorpicker_hex_input.rs"]
mod colorpicker_hex_input;
#[path = "component_text_input.rs"]
mod component_text_input;
#[path = "container_update_forwarding.rs"]
mod container_update_forwarding;
#[path = "control_label_stays_inside_its_control.rs"]
mod control_label_stays_inside_its_control;
#[path = "control_row_alignment.rs"]
mod control_row_alignment;
#[path = "controls_out_of_view_rest.rs"]
mod controls_out_of_view_rest;
#[path = "data_table_test.rs"]
mod data_table_test;
#[path = "data_widget_text_alignment.rs"]
mod data_widget_text_alignment;
#[cfg(unix)]
#[path = "diagnostics_outlive_a_closed_log_pipe.rs"]
mod diagnostics_outlive_a_closed_log_pipe;
#[path = "document_content_route.rs"]
mod document_content_route;
#[path = "file_picker_render_test.rs"]
mod file_picker_render_test;
#[path = "filled_control_labels_are_readable.rs"]
mod filled_control_labels_are_readable;
#[path = "filling_child_keeps_its_share.rs"]
mod filling_child_keeps_its_share;
#[path = "fractional_scale_render.rs"]
mod fractional_scale_render;
#[path = "fullscreen_way_out.rs"]
mod fullscreen_way_out;
#[path = "ghost_cursor_render_test.rs"]
mod ghost_cursor_render_test;
#[path = "group_row_visual_journey.rs"]
mod group_row_visual_journey;
#[path = "headless_app_test.rs"]
mod headless_app_test;
#[path = "immersive_widget_geometry.rs"]
mod immersive_widget_geometry;
#[path = "incremental_render_regressions.rs"]
mod incremental_render_regressions;
#[path = "layout_walk_lists_each_child_once.rs"]
mod layout_walk_lists_each_child_once;
#[path = "local_identity_is_a_person.rs"]
mod local_identity_is_a_person;
#[path = "menu_highlight_render.rs"]
mod menu_highlight_render;
#[path = "menu_material_contrast.rs"]
mod menu_material_contrast;
#[path = "mouse_navigation_test.rs"]
mod mouse_navigation_test;
#[path = "multi_user_session.rs"]
mod multi_user_session;
#[path = "overlay_test.rs"]
mod overlay_test;
#[path = "painter_aot.rs"]
mod painter_aot;
#[path = "permission_context.rs"]
mod permission_context;
#[path = "placed_pointers_test.rs"]
mod placed_pointers_test;
#[path = "presence_context.rs"]
mod presence_context;
#[path = "presence_experience_test.rs"]
mod presence_experience_test;
#[path = "process_entry_points.rs"]
mod process_entry_points;
#[path = "profiling.rs"]
mod profiling;
#[path = "remote_cursor_presence.rs"]
mod remote_cursor_presence;
#[path = "resize_reflow_test.rs"]
mod resize_reflow_test;
#[path = "screenshot_test.rs"]
mod screenshot_test;
#[path = "scrollable_test.rs"]
mod scrollable_test;
#[path = "scrollbar_render_test.rs"]
mod scrollbar_render_test;
#[path = "tab_strip_test.rs"]
mod tab_strip_test;
#[path = "text_scaled_once.rs"]
mod text_scaled_once;
#[path = "textinput_local_mode.rs"]
mod textinput_local_mode;
#[path = "toast_test.rs"]
mod toast_test;
#[path = "tokens_test.rs"]
mod tokens_test;
#[path = "tooltip_render_test.rs"]
mod tooltip_render_test;
#[path = "touch_gestures_test.rs"]
mod touch_gestures_test;
#[path = "typography_golden_geometry.rs"]
mod typography_golden_geometry;
#[path = "typography_visual_regression.rs"]
mod typography_visual_regression;
#[path = "undo_per_author.rs"]
mod undo_per_author;
#[path = "undo_the_agents_work.rs"]
mod undo_the_agents_work;
#[path = "wake_scope_host.rs"]
mod wake_scope_host;
#[path = "window_group_test.rs"]
mod window_group_test;
#[path = "window_group_visual_journey.rs"]
mod window_group_visual_journey;

#[path = "finish_render.rs"]
mod finish_render;

mod application_contract;

#[path = "bound_component_dispatch.rs"]
mod bound_component_dispatch;

mod instance_undo;

mod package_primary;

#[path = "resource_reservation.rs"]
mod resource_reservation;

#[path = "slide_box.rs"]
mod slide_box;
