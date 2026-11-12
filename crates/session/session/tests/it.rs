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
#[path = "action_plane.rs"]
mod action_plane;
#[path = "agent_control.rs"]
mod agent_control;
#[path = "agent_environments.rs"]
mod agent_environments;
#[path = "agent_story.rs"]
mod agent_story;
#[path = "app_sheets.rs"]
mod app_sheets;
#[path = "app_side_records.rs"]
mod app_side_records;
#[path = "arrangement_placing.rs"]
mod arrangement_placing;
#[path = "arrangement_record.rs"]
mod arrangement_record;
#[path = "ask.rs"]
mod ask;
#[path = "atspi_bus.rs"]
mod atspi_bus;
#[path = "automation.rs"]
mod automation;
#[path = "canvas_ask.rs"]
mod canvas_ask;
#[path = "chat_2x2.rs"]
mod chat_2x2;
#[path = "chat_view.rs"]
mod chat_view;
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
#[path = "desk_arrangement.rs"]
mod desk_arrangement;
#[path = "desk_invitations.rs"]
mod desk_invitations;
#[path = "desk_layout.rs"]
mod desk_layout;
#[path = "desk_styles.rs"]
mod desk_styles;
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
#[path = "fabric.rs"]
mod fabric;
#[path = "file_clipboard.rs"]
mod file_clipboard;
#[path = "files_tree.rs"]
mod files_tree;
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
#[path = "import_readiness.rs"]
mod import_readiness;
#[path = "input_method.rs"]
mod input_method;
#[path = "invite.rs"]
mod invite;
#[path = "join.rs"]
mod join;
#[path = "launch.rs"]
mod launch;
#[path = "live_revocation.rs"]
mod live_revocation;
#[path = "lockdown_live.rs"]
mod lockdown_live;
#[path = "log_says_once.rs"]
mod log_says_once;
#[path = "logging.rs"]
mod logging;
#[path = "lounge_e2e.rs"]
mod lounge_e2e;
#[path = "mention_notice.rs"]
mod mention_notice;
#[path = "network_agent_questions.rs"]
mod network_agent_questions;
#[path = "network_unlock.rs"]
mod network_unlock;
#[path = "network_unlock_enrolment.rs"]
mod network_unlock_enrolment;
#[path = "no_locale_leaks.rs"]
mod no_locale_leaks;
#[path = "origin_connector.rs"]
mod origin_connector;
#[path = "overlays.rs"]
mod overlays;
#[path = "owner_door.rs"]
mod owner_door;
#[path = "palette_captures.rs"]
mod palette_captures;
#[path = "panel_services.rs"]
mod panel_services;
#[path = "people.rs"]
mod people;
#[path = "personal_store.rs"]
mod personal_store;
#[path = "placed_apps.rs"]
mod placed_apps;
#[path = "portal_link.rs"]
mod portal_link;
#[path = "portal_questions.rs"]
mod portal_questions;
#[path = "power.rs"]
mod power;
#[path = "primary_display.rs"]
mod primary_display;
#[path = "questions.rs"]
mod questions;
#[path = "refused_writes.rs"]
mod refused_writes;
#[path = "region_formats.rs"]
mod region_formats;
#[path = "remote_door.rs"]
mod remote_door;
#[path = "repl_door.rs"]
mod repl_door;
#[path = "rotation.rs"]
mod rotation;
#[path = "rust_views.rs"]
mod rust_views;
#[path = "search.rs"]
mod search;
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
#[path = "settings_sync.rs"]
mod settings_sync;
#[path = "shared_folder.rs"]
mod shared_folder;
#[path = "sheet_frames.rs"]
mod sheet_frames;
#[path = "sheets.rs"]
mod sheets;
#[path = "sheets_placement.rs"]
mod sheets_placement;
#[path = "shell.rs"]
mod shell;
#[path = "shell_displays.rs"]
mod shell_displays;
#[path = "shell_screen.rs"]
mod shell_screen;
#[path = "shell_themes.rs"]
mod shell_themes;
#[path = "shortcut_effects.rs"]
mod shortcut_effects;
#[path = "shortcut_guide.rs"]
mod shortcut_guide;
#[path = "slot_view.rs"]
mod slot_view;
#[path = "test_control.rs"]
mod test_control;
#[path = "tools.rs"]
mod tools;
#[path = "traditional_desktop.rs"]
mod traditional_desktop;
#[path = "tray.rs"]
mod tray;
#[path = "visual_alerts.rs"]
mod visual_alerts;
#[path = "vpn.rs"]
mod vpn;
#[path = "wifi_continuity.rs"]
mod wifi_continuity;
#[path = "window_menu.rs"]
mod window_menu;
#[path = "window_menu_probe.rs"]
mod window_menu_probe;
#[path = "window_switcher.rs"]
mod window_switcher;
#[path = "workspace_agents.rs"]
mod workspace_agents;
#[path = "workspaces.rs"]
mod workspaces;
#[path = "youtube_client.rs"]
mod youtube_client;
