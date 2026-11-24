// One test binary instead of one per file: each tests/*.rs linked this whole
// crate separately, and its test binaries were the tree's largest link. #[path]
// keeps the files where they are; nextest runs each test in its own process, so
// tests that touch process state keep the isolation separate binaries gave them.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::expect_used,
    clippy::float_cmp,
    clippy::indexing_slicing,
    clippy::needless_pass_by_value,
    clippy::panic,
    clippy::significant_drop_tightening,
    clippy::string_slice,
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::unreachable,
    clippy::unused_self,
    clippy::unwrap_used,
    clippy::used_underscore_binding,
    reason = "in a test these are the assertion, not a hazard"
)]

// A site admits its members' records through an Automerge worker: this binary
// is that worker, as the session's own is.
tilo_automerge_isolate::test_worker!();

#[path = "common/mod.rs"]
mod common;

#[path = "a11y_export.rs"]
mod a11y_export;
#[path = "acceptance.rs"]
mod acceptance;
#[path = "admission.rs"]
mod admission;
#[path = "agent_control.rs"]
mod agent_control;
#[path = "agent_environments.rs"]
mod agent_environments;
#[path = "agent_story.rs"]
mod agent_story;
#[path = "app_open_budget.rs"]
mod app_open_budget;
#[path = "app_records.rs"]
mod app_records;
#[path = "app_sheets.rs"]
mod app_sheets;
#[path = "app_socket.rs"]
mod app_socket;
#[path = "ask.rs"]
mod ask;
#[path = "automation.rs"]
mod automation;
#[path = "canvas_ask.rs"]
mod canvas_ask;
#[path = "canvas_chrome.rs"]
mod canvas_chrome;
#[path = "canvas_point.rs"]
mod canvas_point;
#[path = "canvas_turn_open.rs"]
mod canvas_turn_open;
#[path = "chat_2x2.rs"]
mod chat_2x2;
#[path = "clipboard_fabric.rs"]
mod clipboard_fabric;
#[path = "clipboard_files.rs"]
mod clipboard_files;
#[path = "clipboard_reading.rs"]
mod clipboard_reading;
#[path = "closed_tasks.rs"]
mod closed_tasks;
#[path = "collab.rs"]
mod collab;
#[path = "controllers.rs"]
mod controllers;
#[path = "delete_workspace.rs"]
mod delete_workspace;
#[path = "desktop_services.rs"]
mod desktop_services;
#[path = "dev_session_apps.rs"]
mod dev_session_apps;
#[path = "device_branch.rs"]
mod device_branch;
#[path = "device_record.rs"]
mod device_record;
#[path = "devices.rs"]
mod devices;
#[path = "directory.rs"]
mod directory;
#[path = "display_settings.rs"]
mod display_settings;
#[path = "displays.rs"]
mod displays;
#[path = "do_action.rs"]
mod do_action;
#[path = "fabric.rs"]
mod fabric;
#[path = "file_clipboard.rs"]
mod file_clipboard;
#[path = "find_off_loop.rs"]
mod find_off_loop;
#[path = "find_records.rs"]
mod find_records;
#[path = "first_app.rs"]
mod first_app;
#[path = "firstboot.rs"]
mod firstboot;
#[path = "float_windows.rs"]
mod float_windows;
#[path = "german_surfaces.rs"]
mod german_surfaces;
#[path = "hosted_browser.rs"]
mod hosted_browser;
#[path = "hosted_windows.rs"]
mod hosted_windows;
#[path = "idle_notes.rs"]
mod idle_notes;
#[path = "input_method.rs"]
mod input_method;
#[path = "invitations.rs"]
mod invitations;
#[path = "invite.rs"]
mod invite;
#[path = "join.rs"]
mod join;
#[path = "kept_apps.rs"]
mod kept_apps;
#[path = "launch.rs"]
mod launch;
#[path = "layout.rs"]
mod layout;
#[path = "layout_gestures.rs"]
mod layout_gestures;
#[path = "layout_placing.rs"]
mod layout_placing;
#[path = "layout_record.rs"]
mod layout_record;
#[path = "layout_shortcuts.rs"]
mod layout_shortcuts;
#[path = "layout_verbs.rs"]
mod layout_verbs;
#[path = "layout_view.rs"]
mod layout_view;
#[path = "live_activities.rs"]
mod live_activities;
#[path = "live_relay_bound.rs"]
mod live_relay_bound;
#[path = "lockdown_live.rs"]
mod lockdown_live;
#[path = "log_says_once.rs"]
mod log_says_once;
#[path = "logging.rs"]
mod logging;
#[path = "machine_isolation.rs"]
mod machine_isolation;
#[path = "machine_plan.rs"]
mod machine_plan;
#[path = "mcp_server_workspace.rs"]
mod mcp_server_workspace;
#[path = "network_unlock.rs"]
mod network_unlock;
#[path = "network_unlock_enrolment.rs"]
mod network_unlock_enrolment;
#[path = "no_locale_leaks.rs"]
mod no_locale_leaks;
#[path = "no_view_call.rs"]
mod no_view_call;
#[path = "notification_agent.rs"]
mod notification_agent;
#[path = "offline_start.rs"]
mod offline_start;
#[path = "origin_connector.rs"]
mod origin_connector;
#[path = "overlays.rs"]
mod overlays;
#[path = "overlays_styles.rs"]
mod overlays_styles;
#[path = "own_api.rs"]
mod own_api;
#[path = "owner_control.rs"]
mod owner_control;
#[path = "palette_captures.rs"]
mod palette_captures;
#[path = "panel_services.rs"]
mod panel_services;
#[path = "part_view.rs"]
mod part_view;
#[path = "people.rs"]
mod people;
#[path = "person_workspace.rs"]
mod person_workspace;
#[path = "placed_apps.rs"]
mod placed_apps;
#[path = "portal_link.rs"]
mod portal_link;
#[path = "power.rs"]
mod power;
#[path = "primary_display.rs"]
mod primary_display;
#[path = "profile_name.rs"]
mod profile_name;
#[path = "quick_devices.rs"]
mod quick_devices;
#[path = "recents.rs"]
mod recents;
#[path = "refounded_workspace.rs"]
mod refounded_workspace;
#[path = "refused_writes.rs"]
mod refused_writes;
#[path = "region_formats.rs"]
mod region_formats;
#[path = "repl_api.rs"]
mod repl_api;
#[path = "rotation.rs"]
mod rotation;
#[path = "search.rs"]
mod search;
#[path = "second_device.rs"]
mod second_device;
#[path = "send_box_targets.rs"]
mod send_box_targets;
#[path = "service_sessions.rs"]
mod service_sessions;
#[path = "services.rs"]
mod services;
#[path = "session.rs"]
mod session;
#[path = "session_package.rs"]
mod session_package;
#[path = "settings_authorship.rs"]
mod settings_authorship;
#[path = "settings_roam.rs"]
mod settings_roam;
#[path = "shared_input.rs"]
mod shared_input;
#[path = "sheet_frames.rs"]
mod sheet_frames;
#[path = "sheets.rs"]
mod sheets;
#[path = "sheets_placement.rs"]
mod sheets_placement;
#[path = "shell.rs"]
mod shell;
#[path = "shell_a11y.rs"]
mod shell_a11y;
#[path = "shell_displays.rs"]
mod shell_displays;
#[path = "shell_navigation.rs"]
mod shell_navigation;
#[path = "shell_platform.rs"]
mod shell_platform;
#[path = "shell_record_persists.rs"]
mod shell_record_persists;
#[path = "shell_screen.rs"]
mod shell_screen;
#[path = "shell_themes.rs"]
mod shell_themes;
#[path = "shortcut_effects.rs"]
mod shortcut_effects;
#[path = "shortcut_guide.rs"]
mod shortcut_guide;
#[path = "startup_timeline.rs"]
mod startup_timeline;
#[path = "strips_tabs.rs"]
mod strips_tabs;
#[path = "style_packages.rs"]
mod style_packages;
#[path = "styles.rs"]
mod styles;
#[path = "surface_input_frames.rs"]
mod surface_input_frames;
#[path = "surface_lifted.rs"]
mod surface_lifted;
#[path = "test_control.rs"]
mod test_control;
#[path = "tools.rs"]
mod tools;
#[path = "traditional_desktop.rs"]
mod traditional_desktop;
#[path = "tray.rs"]
mod tray;
#[path = "unheld_workspace.rs"]
mod unheld_workspace;
#[path = "visual_alerts.rs"]
mod visual_alerts;
#[path = "vpn.rs"]
mod vpn;
#[path = "well_policy.rs"]
mod well_policy;
#[path = "wifi_continuity.rs"]
mod wifi_continuity;
#[path = "window_accent.rs"]
mod window_accent;
#[path = "window_bar_actions.rs"]
mod window_bar_actions;
#[path = "window_chrome.rs"]
mod window_chrome;
#[path = "window_layout.rs"]
mod window_layout;
#[path = "window_menu.rs"]
mod window_menu;
#[path = "window_menu_probe.rs"]
mod window_menu_probe;
#[path = "window_share_live.rs"]
mod window_share_live;
#[path = "windows_compat.rs"]
mod windows_compat;
#[path = "workspace_agents.rs"]
mod workspace_agents;
#[path = "workspaces.rs"]
mod workspaces;
#[path = "youtube_client.rs"]
mod youtube_client;
