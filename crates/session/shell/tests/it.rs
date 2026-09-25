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

#[path = "support/mod.rs"]
mod support;

#[path = "active_group_wireup_test.rs"]
mod active_group_wireup_test;
#[path = "advertised_globals.rs"]
mod advertised_globals;
#[path = "apps_integration.rs"]
mod apps_integration;
#[path = "blend_layer_cost.rs"]
mod blend_layer_cost;
#[path = "capability_enforcement.rs"]
mod capability_enforcement;
#[path = "clipboard_e2e.rs"]
mod clipboard_e2e;
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
#[path = "control_socket_test.rs"]
mod control_socket_test;
#[path = "cross_strip_drag.rs"]
mod cross_strip_drag;
#[path = "dbus_runtime_context.rs"]
mod dbus_runtime_context;
#[path = "desktop_env_interactions.rs"]
mod desktop_env_interactions;
#[path = "dmabuf_commit_barrier.rs"]
mod dmabuf_commit_barrier;
#[path = "dmabuf_direct_capture_path.rs"]
mod dmabuf_direct_capture_path;
#[path = "dmabuf_feedback_protocol.rs"]
mod dmabuf_feedback_protocol;
#[path = "document_open_e2e.rs"]
mod document_open_e2e;
#[path = "drm_backend_boot.rs"]
mod drm_backend_boot;
#[path = "e2e_shell_flows.rs"]
mod e2e_shell_flows;
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
#[path = "interaction_test.rs"]
mod interaction_test;
#[path = "keyboard_focus_wire.rs"]
mod keyboard_focus_wire;
#[path = "leak_probe.rs"]
mod leak_probe;
#[path = "lock_screen_visual.rs"]
mod lock_screen_visual;
#[path = "maximize_persist_wireup_test.rs"]
mod maximize_persist_wireup_test;
#[path = "onboarding_visual.rs"]
mod onboarding_visual;
#[path = "opening_configure_carries_the_tab_size.rs"]
mod opening_configure_carries_the_tab_size;
#[path = "peer_join_toast_batches.rs"]
mod peer_join_toast_batches;
#[path = "peer_sync_test.rs"]
mod peer_sync_test;
#[path = "permission_prompt_pump.rs"]
mod permission_prompt_pump;
#[path = "permissions_tab_test.rs"]
mod permissions_tab_test;
#[path = "persisted_app_placeholders.rs"]
mod persisted_app_placeholders;
#[path = "portal_file_chooser_e2e.rs"]
mod portal_file_chooser_e2e;
#[path = "portal_grant_reaches_sandbox.rs"]
mod portal_grant_reaches_sandbox;
#[path = "portal_prompt_drain_e2e.rs"]
mod portal_prompt_drain_e2e;
#[path = "portal_test.rs"]
mod portal_test;
#[path = "qs_actions_click_routing.rs"]
mod qs_actions_click_routing;
#[path = "real_apps_visual.rs"]
mod real_apps_visual;
#[path = "remote_cursors_test.rs"]
mod remote_cursors_test;
#[path = "sandbox_check.rs"]
mod sandbox_check;
#[path = "server_side_decorations.rs"]
mod server_side_decorations;
#[path = "session_lock_e2e.rs"]
mod session_lock_e2e;
#[path = "shell_chrome_test.rs"]
mod shell_chrome_test;
#[path = "shell_state_test.rs"]
mod shell_state_test;
#[path = "sidebar_wheel_disc_snapshot.rs"]
mod sidebar_wheel_disc_snapshot;
#[path = "state_file_format.rs"]
mod state_file_format;
#[path = "strip_navigation.rs"]
mod strip_navigation;
#[path = "surface_substream_e2e.rs"]
mod surface_substream_e2e;
#[path = "system_monitor_test.rs"]
mod system_monitor_test;
#[path = "tilo_e2e_test.rs"]
mod tilo_e2e_test;
#[path = "two_netns_share.rs"]
mod two_netns_share;
#[path = "wayland_test.rs"]
mod wayland_test;
#[path = "workspace_settings_state_test.rs"]
mod workspace_settings_state_test;
#[path = "xdg_activation.rs"]
mod xdg_activation;
