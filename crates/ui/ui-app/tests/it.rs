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
    clippy::arithmetic_side_effects,
    clippy::items_after_statements,
    clippy::used_underscore_binding,
    clippy::redundant_closure,
    clippy::too_many_lines,
    clippy::unnecessary_literal_bound,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::disallowed_methods,
    reason = "integration tests may time and wait on the real clock; the rule guards the library"
)]
#![allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "a moved widget test opens the toolkit's names with one prelude and uses a subset"
)]
#![allow(
    clippy::doc_markdown,
    clippy::map_unwrap_or,
    clippy::ref_as_ptr,
    clippy::similar_names,
    clippy::unnecessary_wraps,
    reason = "a widget test builds its probe fixtures and scenes by hand; the shapes are the assertion, not a library a caller keeps"
)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::collapsible_if,
    clippy::default_trait_access,
    clippy::empty_line_after_doc_comments,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::large_enum_variant,
    clippy::large_stack_arrays,
    clippy::manual_assert_eq,
    clippy::manual_find,
    clippy::manual_midpoint,
    clippy::many_single_char_names,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_errors_doc,
    clippy::needless_collect,
    clippy::needless_pass_by_value,
    clippy::needless_range_loop,
    clippy::needless_raw_string_hashes,
    clippy::or_fun_call,
    clippy::panic_in_result_fn,
    clippy::redundant_clone,
    clippy::redundant_closure_for_method_calls,
    clippy::result_large_err,
    clippy::semicolon_if_nothing_returned,
    clippy::significant_drop_in_scrutinee,
    clippy::significant_drop_tightening,
    clippy::struct_excessive_bools,
    clippy::type_complexity,
    clippy::unchecked_time_subtraction,
    clippy::unicode_not_nfc,
    clippy::uninlined_format_args,
    clippy::unreadable_literal,
    clippy::unused_self,
    clippy::useless_conversion,
    reason = "the toolkit's unit tests moved here, and the crate they came from allows these"
)]

#[path = "support/mod.rs"]
mod support;

#[path = "widgets.rs"]
mod widgets;

#[path = "accessibility_geometry.rs"]
mod accessibility_geometry;
#[path = "colorpicker_hex_input.rs"]
mod colorpicker_hex_input;
#[path = "component_text_input.rs"]
mod component_text_input;
#[path = "control_label_stays_inside_its_control.rs"]
mod control_label_stays_inside_its_control;
#[path = "control_row_alignment.rs"]
mod control_row_alignment;
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
#[path = "headless_app_test.rs"]
mod headless_app_test;
#[path = "immersive_widget_geometry.rs"]
mod immersive_widget_geometry;
#[path = "incremental_render_regressions.rs"]
mod incremental_render_regressions;
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
#[path = "placed_pointers_test.rs"]
mod placed_pointers_test;
#[path = "presence_context.rs"]
mod presence_context;
#[path = "presence_experience_test.rs"]
mod presence_experience_test;
#[path = "process_entry_points.rs"]
mod process_entry_points;
#[path = "remote_cursor_presence.rs"]
mod remote_cursor_presence;
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
#[path = "toast_test.rs"]
mod toast_test;
#[path = "tooltip_render_test.rs"]
mod tooltip_render_test;
#[path = "touch_gestures_test.rs"]
mod touch_gestures_test;
#[path = "typography_visual_regression.rs"]
mod typography_visual_regression;
#[path = "undo_per_author.rs"]
mod undo_per_author;
#[path = "undo_the_agents_work.rs"]
mod undo_the_agents_work;
#[path = "wake_scope_host.rs"]
mod wake_scope_host;

#[path = "finish_render.rs"]
mod finish_render;

#[path = "component_tests.rs"]
mod component_tests;

#[path = "history_mode_readiness.rs"]
mod history_mode_readiness;

mod application_contract;

#[path = "bound_component_dispatch.rs"]
mod bound_component_dispatch;

mod instance_undo;

mod package_primary;

#[path = "resource_reservation.rs"]
mod resource_reservation;

#[path = "slide_box.rs"]
mod slide_box;

// Widget unit tests that drive the app runner: they reach the toolkit through
// its public API and its `test-support` feature.
#[path = "widgets/arrival_failure_tests.rs"]
mod arrival_failure_tests;
#[path = "widgets/button_touch_tests.rs"]
mod button_touch_tests;
#[path = "widgets/capture_studio_semantics_tests.rs"]
mod capture_studio_semantics_tests;
#[path = "widgets/context_menu_area_tests.rs"]
mod context_menu_area_tests;
#[path = "widgets/control_hand_on_tests.rs"]
mod control_hand_on_tests;
#[path = "widgets/control_touch_tests.rs"]
mod control_touch_tests;
#[path = "widgets/disclosure_band_tests.rs"]
mod disclosure_band_tests;
#[path = "widgets/docked_tests.rs"]
mod docked_tests;
#[path = "widgets/document_viewport_copy_tests.rs"]
mod document_viewport_copy_tests;
#[path = "widgets/document_viewport_layers_tests.rs"]
mod document_viewport_layers_tests;
#[path = "widgets/drop_zone_tests.rs"]
mod drop_zone_tests;
#[path = "widgets/graph_canvas_tests.rs"]
mod graph_canvas_tests;

// The toolkit's own tests that drive it through the app runner above it.
#[path = "a11y_child_route.rs"]
mod a11y_child_route;
#[path = "action_contract.rs"]
mod action_contract;
#[cfg(unix)]
#[path = "collaboration_app.rs"]
mod collaboration_app;
#[path = "component_controls.rs"]
mod component_controls;
#[path = "component_coverage.rs"]
mod component_coverage;
#[path = "component_data.rs"]
mod component_data;
#[path = "component_feedback.rs"]
mod component_feedback;
#[path = "component_media.rs"]
mod component_media;
#[path = "component_part_fill.rs"]
mod component_part_fill;
#[path = "composition_page.rs"]
mod composition_page;
#[path = "control_hints_component.rs"]
mod control_hints_component;
#[path = "focus_navigation.rs"]
mod focus_navigation;
#[path = "history_mode_focus.rs"]
mod history_mode_focus;
#[path = "history_mode_host.rs"]
mod history_mode_host;
#[path = "history_mode_integration.rs"]
mod history_mode_integration;
#[path = "history_mode_isolation.rs"]
mod history_mode_isolation;
#[cfg(target_os = "linux")]
#[path = "host_chrome_header_bar.rs"]
mod host_chrome_header_bar;
#[cfg(target_os = "macos")]
#[path = "host_chrome_macos.rs"]
mod host_chrome_macos;
#[path = "host_chrome_menus.rs"]
mod host_chrome_menus;
#[path = "host_chrome_windows_items.rs"]
mod host_chrome_windows_items;
#[path = "live_pictures.rs"]
mod live_pictures;
#[path = "native_scenario.rs"]
mod native_scenario;
#[path = "pointer_replay.rs"]
mod pointer_replay;
#[path = "pointer_replay_carry.rs"]
mod pointer_replay_carry;
#[path = "presence_draft.rs"]
mod presence_draft;
#[path = "ui_graph.rs"]
mod ui_graph;
#[path = "view.rs"]
mod view;

#[path = "widgets/activity_timeline_edges.rs"]
mod activity_timeline_edges;
#[path = "widgets/avatar_name_refresh.rs"]
mod avatar_name_refresh;
#[path = "widgets/badge_text_width.rs"]
mod badge_text_width;
#[path = "widgets/banner_tests.rs"]
mod banner_tests;
#[path = "widgets/board_component_tests.rs"]
mod board_component_tests;
#[path = "widgets/board_tests.rs"]
mod board_tests;
#[path = "widgets/button_runner_tests.rs"]
mod button_runner_tests;
#[path = "widgets/call_tile_ratio.rs"]
mod call_tile_ratio;
#[path = "widgets/canvas_place_tests.rs"]
mod canvas_place_tests;
#[path = "widgets/card_parts_tests.rs"]
mod card_parts_tests;
#[path = "widgets/chat_reactions_tests.rs"]
mod chat_reactions_tests;
#[path = "widgets/chip_activation_tests.rs"]
mod chip_activation_tests;
#[path = "widgets/context_menu_nested_tests.rs"]
mod context_menu_nested_tests;
#[path = "widgets/data_table_checkbox_tests.rs"]
mod data_table_checkbox_tests;
#[path = "widgets/date_picker_tests.rs"]
mod date_picker_tests;
#[path = "widgets/document_viewport_tests.rs"]
mod document_viewport_tests;
#[path = "widgets/drag_run_tests.rs"]
mod drag_run_tests;
#[path = "widgets/dropdown_tests.rs"]
mod dropdown_tests;
#[path = "widgets/empty_state_action_tests.rs"]
mod empty_state_action_tests;
#[path = "widgets/find_bar_hosted_tests.rs"]
mod find_bar_hosted_tests;
#[path = "widgets/form_composition_tests.rs"]
mod form_composition_tests;
#[path = "widgets/form_labels_tests.rs"]
mod form_labels_tests;
#[path = "widgets/image_bounds.rs"]
mod image_bounds;
#[path = "widgets/list_row_actions_tests.rs"]
mod list_row_actions_tests;
#[path = "widgets/magnify_tests.rs"]
mod magnify_tests;
#[path = "widgets/map_motion_tests.rs"]
mod map_motion_tests;
#[path = "widgets/nav_list_tests.rs"]
mod nav_list_tests;
#[path = "widgets/nav_shell_tests.rs"]
mod nav_shell_tests;
#[path = "widgets/page_edges_land_on_device_pixels.rs"]
mod page_edges_land_on_device_pixels;
#[path = "widgets/scrollable_snap_tests.rs"]
mod scrollable_snap_tests;
#[path = "widgets/text_tests.rs"]
mod text_tests;
#[path = "widgets/tile_host_tests.rs"]
mod tile_host_tests;
#[path = "widgets/toolbar_framed_tests.rs"]
mod toolbar_framed_tests;
#[path = "widgets/tree_component_tests.rs"]
mod tree_component_tests;
#[path = "widgets/virtual_list_component_tests.rs"]
mod virtual_list_component_tests;
#[path = "widgets/virtual_list_copy_tests.rs"]
mod virtual_list_copy_tests;

#[path = "profiling_tests.rs"]
mod profiling_tests;
