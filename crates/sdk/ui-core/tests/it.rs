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

#[cfg(unix)]
#[path = "agent_edit_is_its_own_undo_boundary.rs"]
mod agent_edit_is_its_own_undo_boundary;
#[path = "app_widget.rs"]
mod app_widget;
#[path = "block_editor_markdown.rs"]
mod block_editor_markdown;
#[path = "block_editor_slash_menu_integration.rs"]
mod block_editor_slash_menu_integration;
#[path = "block_editor_wrapping.rs"]
mod block_editor_wrapping;
#[cfg(unix)]
#[path = "coedit_keystone.rs"]
mod coedit_keystone;
#[path = "colorpicker_hex_input.rs"]
mod colorpicker_hex_input;
#[path = "command_palette_test.rs"]
mod command_palette_test;
#[path = "component_text_input.rs"]
mod component_text_input;
#[path = "container_update_forwarding.rs"]
mod container_update_forwarding;
#[path = "context_menu_test.rs"]
mod context_menu_test;
#[path = "control_row_alignment.rs"]
mod control_row_alignment;
#[path = "copy_audit.rs"]
mod copy_audit;
#[cfg(unix)]
#[path = "cross_process_presence_pump.rs"]
mod cross_process_presence_pump;
#[path = "data_table_test.rs"]
mod data_table_test;
#[path = "data_widget_text_alignment.rs"]
mod data_widget_text_alignment;
#[path = "design_token_audit.rs"]
mod design_token_audit;
#[cfg(unix)]
#[path = "diagnostics_outlive_a_closed_log_pipe.rs"]
mod diagnostics_outlive_a_closed_log_pipe;
#[path = "file_picker_render_test.rs"]
mod file_picker_render_test;
#[path = "filled_control_labels_are_readable.rs"]
mod filled_control_labels_are_readable;
#[path = "fractional_scale_render.rs"]
mod fractional_scale_render;
#[path = "ghost_cursor_render_test.rs"]
mod ghost_cursor_render_test;
#[path = "group_row_visual_journey.rs"]
mod group_row_visual_journey;
#[path = "host_entry_points.rs"]
mod host_entry_points;
#[path = "immersive_widget_geometry.rs"]
mod immersive_widget_geometry;
#[path = "incremental_render_regressions.rs"]
mod incremental_render_regressions;
#[cfg(unix)]
#[path = "large_document_transfer.rs"]
mod large_document_transfer;
#[cfg(unix)]
#[path = "large_frame_reassembly.rs"]
mod large_frame_reassembly;
#[path = "link_popover_renders.rs"]
mod link_popover_renders;
#[cfg(unix)]
#[path = "live_text_sync.rs"]
mod live_text_sync;
#[path = "local_identity_is_a_person.rs"]
mod local_identity_is_a_person;
#[path = "menu_highlight_render.rs"]
mod menu_highlight_render;
#[path = "mouse_navigation_test.rs"]
mod mouse_navigation_test;
#[path = "multi_user_session.rs"]
mod multi_user_session;
#[path = "overlay_test.rs"]
mod overlay_test;
#[path = "permission_context.rs"]
mod permission_context;
#[path = "presence_context.rs"]
mod presence_context;
#[path = "presence_experience_test.rs"]
mod presence_experience_test;
#[path = "presence_remote_cursors_test.rs"]
mod presence_remote_cursors_test;
#[path = "profiling.rs"]
mod profiling;
#[cfg(unix)]
#[path = "real_socket_n_peer.rs"]
mod real_socket_n_peer;
#[cfg(unix)]
#[path = "real_socket_two_peer.rs"]
mod real_socket_two_peer;
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
#[path = "spacing_token_audit.rs"]
mod spacing_token_audit;
#[path = "strip_view_test.rs"]
mod strip_view_test;
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[path = "synthetic_key_event.rs"]
mod synthetic_key_event;
#[path = "tabs_test.rs"]
mod tabs_test;
#[path = "text_scaled_once.rs"]
mod text_scaled_once;
#[path = "textinput_local_mode.rs"]
mod textinput_local_mode;
#[path = "tile_app_widget.rs"]
mod tile_app_widget;
#[path = "toast_test.rs"]
mod toast_test;
#[path = "tokens_test.rs"]
mod tokens_test;
#[path = "tooltip_render_test.rs"]
mod tooltip_render_test;
#[path = "tooltip_test.rs"]
mod tooltip_test;
#[path = "touch_gestures_test.rs"]
mod touch_gestures_test;
#[path = "typography_baseline_geometry.rs"]
mod typography_baseline_geometry;
#[path = "typography_token_audit.rs"]
mod typography_token_audit;
#[path = "typography_visual_regression.rs"]
mod typography_visual_regression;
#[path = "undo_per_author.rs"]
mod undo_per_author;
#[path = "undo_the_agents_work.rs"]
mod undo_the_agents_work;
#[path = "wake_scope_host.rs"]
mod wake_scope_host;
#[path = "warmup_no_adapter.rs"]
mod warmup_no_adapter;
#[path = "widget_trait_test.rs"]
mod widget_trait_test;
#[path = "window_group_test.rs"]
mod window_group_test;
#[path = "window_group_visual_journey.rs"]
mod window_group_visual_journey;
#[path = "workspace_view_test.rs"]
mod workspace_view_test;

mod canonical_record_owner;

mod application_contract;

#[path = "bound_component_dispatch.rs"]
mod bound_component_dispatch;

mod instance_undo;

mod package_primary;
