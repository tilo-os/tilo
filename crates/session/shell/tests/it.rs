// Linux compositor/protocol integration suite; portable native app tests also
// run through the shell library test target on macOS.
#![cfg(target_os = "linux")]
// One test binary instead of one per file: each tests/*.rs linked the whole
// crate separately. Files stay where they are; #[path] pulls them in as
// modules. nextest runs each test in its own process, so tests that touch
// process state keep the isolation separate binaries gave them.
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

// Room admission decodes peer bytes in automerge worker processes started
// from this test binary; this serves them.
tilo_automerge_isolate::test_worker!();

#[path = "support/mod.rs"]
mod support;

#[path = "active_group_wireup_test.rs"]
mod active_group_wireup_test;
#[path = "advertised_globals.rs"]
mod advertised_globals;
#[path = "argb_opaque_region.rs"]
mod argb_opaque_region;
#[path = "blend_layer_cost.rs"]
mod blend_layer_cost;
#[path = "bluetooth_pairing.rs"]
mod bluetooth_pairing;
#[path = "capability_enforcement.rs"]
mod capability_enforcement;
#[path = "capture_protection.rs"]
mod capture_protection;
#[path = "client_exit_closes_its_tabs.rs"]
mod client_exit_closes_its_tabs;
#[path = "clipboard_e2e.rs"]
mod clipboard_e2e;
#[path = "closing_window_frames.rs"]
mod closing_window_frames;
#[path = "collab_policy_state.rs"]
mod collab_policy_state;
#[path = "color_management_protocol.rs"]
mod color_management_protocol;
#[path = "color_management_surface_override.rs"]
mod color_management_surface_override;
#[path = "compositor_test.rs"]
mod compositor_test;
#[path = "concept_parity_spec.rs"]
mod concept_parity_spec;
#[path = "continuity_suggestion_flow.rs"]
mod continuity_suggestion_flow;
#[path = "control_socket_test.rs"]
mod control_socket_test;
#[path = "controller_navigation.rs"]
mod controller_navigation;
#[path = "cpu_raster_parity.rs"]
mod cpu_raster_parity;
#[path = "cross_strip_drag.rs"]
mod cross_strip_drag;
#[path = "cursor_protocol.rs"]
mod cursor_protocol;
#[path = "dbus_runtime_context.rs"]
mod dbus_runtime_context;
#[path = "desktop_env_interactions.rs"]
mod desktop_env_interactions;
#[path = "display_layout_switcher.rs"]
mod display_layout_switcher;
#[path = "dmabuf_commit_barrier.rs"]
mod dmabuf_commit_barrier;
#[path = "dmabuf_direct_capture_path.rs"]
mod dmabuf_direct_capture_path;
#[path = "dmabuf_feedback_protocol.rs"]
mod dmabuf_feedback_protocol;
#[path = "dmabuf_scanout_feedback.rs"]
mod dmabuf_scanout_feedback;
#[path = "document_open_e2e.rs"]
mod document_open_e2e;
#[path = "e2e_shell_flows.rs"]
mod e2e_shell_flows;
#[path = "emoji_picker_commit.rs"]
mod emoji_picker_commit;
#[path = "expand_fullscreen_visual.rs"]
mod expand_fullscreen_visual;
#[path = "file_drop_routing.rs"]
mod file_drop_routing;
#[path = "flatpak_share_e2e.rs"]
mod flatpak_share_e2e;
#[path = "flatpak_share_encoder_probe.rs"]
mod flatpak_share_encoder_probe;
#[path = "focused_group_marking.rs"]
mod focused_group_marking;
#[path = "foreign_surface_compositing.rs"]
mod foreign_surface_compositing;
#[path = "fractional_scale_protocol.rs"]
mod fractional_scale_protocol;
#[path = "freeze_bench.rs"]
mod freeze_bench;
#[path = "hardware_encode.rs"]
mod hardware_encode;
#[path = "hdr_capability_probe.rs"]
mod hdr_capability_probe;
#[path = "hdr_decode_pipeline.rs"]
mod hdr_decode_pipeline;
#[path = "hdr_layer_composite.rs"]
mod hdr_layer_composite;
#[path = "hdr_output_encode_shader.rs"]
mod hdr_output_encode_shader;
#[path = "hdr_output_pipeline.rs"]
mod hdr_output_pipeline;
#[path = "hdr_tonemap_shader.rs"]
mod hdr_tonemap_shader;
#[path = "headless_examples_smoke.rs"]
mod headless_examples_smoke;
#[path = "identity_startup.rs"]
mod identity_startup;
#[path = "idle_inhibit_protocol.rs"]
mod idle_inhibit_protocol;
#[path = "idle_settles.rs"]
mod idle_settles;
#[path = "idle_wakeups.rs"]
mod idle_wakeups;
#[path = "input_device_settings.rs"]
mod input_device_settings;
#[path = "input_method_wire.rs"]
mod input_method_wire;
#[path = "interaction_test.rs"]
mod interaction_test;
#[path = "keyboard_focus_wire.rs"]
mod keyboard_focus_wire;
#[path = "keyboard_layout_wire.rs"]
mod keyboard_layout_wire;
#[path = "leak_probe.rs"]
mod leak_probe;
#[path = "legacy_window_between_devices.rs"]
mod legacy_window_between_devices;
#[path = "lock_screen_visual.rs"]
mod lock_screen_visual;
#[path = "maximize_persist_wireup_test.rs"]
mod maximize_persist_wireup_test;
#[path = "multi_output.rs"]
mod multi_output;
#[path = "notification_center.rs"]
mod notification_center;
#[path = "onboarding_visual.rs"]
mod onboarding_visual;
#[path = "opening_configure_carries_the_tab_size.rs"]
mod opening_configure_carries_the_tab_size;
#[path = "output_management_protocol.rs"]
mod output_management_protocol;
#[path = "overview_window_actions.rs"]
mod overview_window_actions;
#[path = "peer_sync_test.rs"]
mod peer_sync_test;
#[path = "pen_and_key_capture.rs"]
mod pen_and_key_capture;
#[path = "per_output_groups.rs"]
mod per_output_groups;
#[path = "per_output_scale.rs"]
mod per_output_scale;
#[path = "permission_prompt_pump.rs"]
mod permission_prompt_pump;
#[path = "permissions_tab_test.rs"]
mod permissions_tab_test;
#[path = "persisted_app_placeholders.rs"]
mod persisted_app_placeholders;
#[path = "pointer_lock_protocol.rs"]
mod pointer_lock_protocol;
#[path = "portal_file_chooser_e2e.rs"]
mod portal_file_chooser_e2e;
#[path = "portal_grant_reaches_sandbox.rs"]
mod portal_grant_reaches_sandbox;
#[path = "portal_prompt_drain_e2e.rs"]
mod portal_prompt_drain_e2e;
#[path = "portal_test.rs"]
mod portal_test;
#[path = "presenter_screen.rs"]
mod presenter_screen;
#[path = "qs_actions_click_routing.rs"]
mod qs_actions_click_routing;
#[path = "real_apps_visual.rs"]
mod real_apps_visual;
#[path = "remote_typing_wire.rs"]
mod remote_typing_wire;
#[path = "removed_run_modes.rs"]
mod removed_run_modes;
#[path = "reopen_closed_tab.rs"]
mod reopen_closed_tab;
#[path = "runtime_daemon_process.rs"]
mod runtime_daemon_process;
#[path = "sandbox_check.rs"]
mod sandbox_check;
#[path = "server_side_decorations.rs"]
mod server_side_decorations;
#[path = "session_lock_e2e.rs"]
mod session_lock_e2e;
#[path = "session_lock_recovery.rs"]
mod session_lock_recovery;
#[path = "shell_chrome_test.rs"]
mod shell_chrome_test;
#[path = "shell_narration.rs"]
mod shell_narration;
#[path = "shell_state_test.rs"]
mod shell_state_test;
#[path = "shm_readback.rs"]
mod shm_readback;
#[path = "sidebar_wheel_disc_snapshot.rs"]
mod sidebar_wheel_disc_snapshot;
#[path = "speaker_calibration_fabric.rs"]
mod speaker_calibration_fabric;
#[path = "speaker_clock_policy.rs"]
mod speaker_clock_policy;
#[path = "state_file_format.rs"]
mod state_file_format;
#[path = "strip_navigation.rs"]
mod strip_navigation;
#[path = "surface_substream_e2e.rs"]
mod surface_substream_e2e;
#[path = "tilo_e2e_test.rs"]
mod tilo_e2e_test;
#[path = "toplevel_configure_states.rs"]
mod toplevel_configure_states;
#[path = "toplevel_icon_protocol.rs"]
mod toplevel_icon_protocol;
#[path = "transient_sheets.rs"]
mod transient_sheets;
#[path = "two_netns_share.rs"]
mod two_netns_share;
#[path = "wayland_test.rs"]
mod wayland_test;
#[path = "wheel_scroll_wire.rs"]
mod wheel_scroll_wire;
#[path = "window_previews.rs"]
mod window_previews;
#[path = "window_switcher.rs"]
mod window_switcher;
#[path = "window_transfer_flow.rs"]
mod window_transfer_flow;
#[path = "workspace_settings_state_test.rs"]
mod workspace_settings_state_test;
#[path = "xdg_activation.rs"]
mod xdg_activation;

#[path = "vulkan_present_damage.rs"]
mod vulkan_present_damage;

#[cfg(feature = "test-support")]
#[path = "network_cost_boundary.rs"]
mod network_cost_boundary;
