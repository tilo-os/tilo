//! The native Tilo session: a thin host, as the web page is one.
//!
//! The compositor ([`tilo_compositor`]) shows buffers and routes input. The
//! session gives it the desktop every host shows
//! ([`tilo_desktop::desktop_host`]): the desk (backdrop, launchpad, the
//! strips and window bars of the desktop surface) under the windows, and
//! the chrome (top bar, strip ribbon and rail, overview, menus) above them;
//! the window policy of the desk's strips ([`tilo_surface_host::desktop`]),
//! so each window is a tab of a strip; and the desktop's surface apps, each
//! an ordinary app hosted on a layer of its own: the notification banners,
//! quick settings, and setup over everything while a person first uses the
//! device. What the chrome asks of its host comes back as [`DesktopAsk`]s,
//! carried out by the ask loop in `asks`.
//!
//! Everything a person can do here is a published action: the desk's and
//! the chrome's, and each surface app's through its door
//! ([`ui::device_local_door::SharedDoor`]). [`Session::resources`] lists
//! them and [`Session::invoke`] runs one, as any caller of the action
//! protocol on this device does.
//!
//! The session also hosts the system services it was started with
//! ([`Options::services`]): connectivity, audio and the rest publish their
//! resources beside the desktop's, and [`Session::invoke`] reaches their
//! actions as it reaches the desktop's, by address.

// The session's state between frames, as every host runs it, and the roles
// that keep it: the desk, the chrome, the records, the people and the
// workspaces — what every host runs, the web page included.
mod assembly;
pub use assembly::Session;
// Nothing compiled for the web reads this state yet: the roles that do land
// with the batches that ungate them.
#[cfg_attr(
    target_arch = "wasm32",
    allow(
        unused_imports,
        reason = "the roles that read this state land with their batches"
    )
)]
pub(crate) use assembly::Shared;

mod activity_provider;
mod continuations;
pub mod host_actions;
pub mod overlays;
pub mod records;
pub mod search;
mod search_ask;
mod session_actions;
pub mod workspace_gate;

// The Linux host's effects: the compositor, the services it starts, the
// devices and the processes. A web host provides these through `platform`.
#[cfg(target_os = "linux")]
mod a11y;
#[cfg(target_os = "linux")]
mod a11y_settings;
#[cfg(target_os = "linux")]
mod activity;
#[cfg(target_os = "linux")]
mod activity_models;
#[cfg(target_os = "linux")]
pub mod agent_door;
#[cfg(target_os = "linux")]
mod agents;
#[cfg(target_os = "linux")]
mod ambient_brightness;
#[cfg(target_os = "linux")]
mod app_icons;
#[cfg(target_os = "linux")]
mod app_liveness;
#[cfg(target_os = "linux")]
mod app_names;
#[cfg(target_os = "linux")]
mod app_sheets;
#[cfg(target_os = "linux")]
pub mod app_tabs;
#[cfg(target_os = "linux")]
mod app_tiers;
#[cfg(target_os = "linux")]
pub mod apps;
pub mod arrangement;
#[cfg(target_os = "linux")]
mod asks;
#[cfg(target_os = "linux")]
mod automation;
#[cfg(target_os = "linux")]
mod bars;
#[cfg(target_os = "linux")]
mod bluetooth_handoff;
#[cfg(target_os = "linux")]
mod boot_join;
#[cfg(target_os = "linux")]
mod calls;
#[cfg(target_os = "linux")]
pub mod capture;
#[cfg(target_os = "linux")]
mod capture_grants;
#[cfg(target_os = "linux")]
mod capture_io;
#[cfg(target_os = "linux")]
mod capture_send;
#[cfg(target_os = "linux")]
mod capture_studio;
#[cfg(target_os = "linux")]
mod cards;
#[cfg(target_os = "linux")]
mod certification;
mod channels;
#[cfg(target_os = "linux")]
mod chrome;
#[cfg(target_os = "linux")]
mod clipboard;
#[cfg(target_os = "linux")]
mod clock_wait;
#[cfg(target_os = "linux")]
mod collaboration;
#[cfg(target_os = "linux")]
mod community;
#[cfg(target_os = "linux")]
mod config;
#[cfg(target_os = "linux")]
mod content;
#[cfg(target_os = "linux")]
pub mod controller_nav;
#[cfg(target_os = "linux")]
pub mod controllers;
#[cfg(target_os = "linux")]
mod controls;
#[cfg(target_os = "linux")]
mod conversations;
#[cfg(target_os = "linux")]
mod desk;
#[cfg(target_os = "linux")]
mod desk_doc;
#[cfg(target_os = "linux")]
mod desk_migration;
#[cfg(target_os = "linux")]
mod desk_resource;
#[cfg(target_os = "linux")]
mod desk_restore;
#[cfg(target_os = "linux")]
pub mod desk_styles;
#[cfg(target_os = "linux")]
pub mod desktop_services;
#[cfg(target_os = "linux")]
/// A Rust app's sample resources and views, in a development session only.
#[cfg(feature = "dev-rust-views")]
pub mod dev_rust_views;
#[cfg(target_os = "linux")]
mod device_actions;
#[cfg(target_os = "linux")]
mod devices;
#[cfg(target_os = "linux")]
mod diagnostics;
#[cfg(target_os = "linux")]
mod dictation;
#[cfg(target_os = "linux")]
mod directory;
#[cfg(target_os = "linux")]
mod display_confirm;
#[cfg(target_os = "linux")]
mod display_control;
#[cfg(target_os = "linux")]
mod display_identify;
#[cfg(target_os = "linux")]
mod display_settings;
#[cfg(target_os = "linux")]
mod display_sheet;
#[cfg(target_os = "linux")]
pub mod displays;
#[cfg(target_os = "linux")]
mod door_runs;
#[cfg(target_os = "linux")]
mod doors;
#[cfg(target_os = "linux")]
mod downloaded;
#[cfg(target_os = "linux")]
mod emoji;
mod engine;
#[cfg(target_os = "linux")]
mod env;
#[cfg(target_os = "linux")]
pub mod fabric;
#[cfg(target_os = "linux")]
mod feedback;
#[cfg(target_os = "linux")]
mod feeds;
#[cfg(target_os = "linux")]
mod file_clipboard;
#[cfg(target_os = "linux")]
pub mod files_daemon;
#[cfg(target_os = "linux")]
pub mod firstboot;
#[cfg(target_os = "linux")]
mod fs_authority;
#[cfg(target_os = "linux")]
mod held;
mod here;
#[cfg(target_os = "linux")]
mod host_account;
#[cfg(target_os = "linux")]
mod host_shell;
mod i18n;
#[cfg(target_os = "linux")]
pub mod identity;
#[cfg(target_os = "linux")]
mod identity_admin;
#[cfg(target_os = "linux")]
mod import_readiness;
#[cfg(target_os = "linux")]
mod input_method;
#[cfg(target_os = "linux")]
mod input_sources;
#[cfg(target_os = "linux")]
mod invite;
#[cfg(target_os = "linux")]
pub mod keyboard;
#[cfg(target_os = "linux")]
mod keyboard_shortcuts;
#[cfg(target_os = "linux")]
mod keys;
#[cfg(target_os = "linux")]
mod launches;
#[cfg(target_os = "linux")]
mod lists;
#[cfg(target_os = "linux")]
pub mod live;
#[cfg(target_os = "linux")]
mod lock_screen;
#[cfg(target_os = "linux")]
mod lock_service;
#[cfg(target_os = "linux")]
mod look;
mod membership;
#[cfg(target_os = "linux")]
pub mod mount;
#[cfg(target_os = "linux")]
pub mod nearby_transfer;
#[cfg(target_os = "linux")]
mod network;
#[cfg(target_os = "linux")]
pub mod night_light;
#[cfg(target_os = "linux")]
pub mod notices;
#[cfg(target_os = "linux")]
mod notifications;
#[cfg(target_os = "linux")]
mod options;
#[cfg(target_os = "linux")]
mod overview;
#[cfg(target_os = "linux")]
mod owner_session;
#[cfg(target_os = "linux")]
mod pairing;
#[cfg(target_os = "linux")]
mod panels;
#[cfg(target_os = "linux")]
mod pass;
pub mod people;
#[cfg(target_os = "linux")]
mod personal;
#[cfg(target_os = "linux")]
mod plane;
#[cfg(target_os = "linux")]
pub mod portal_link;
#[cfg(target_os = "linux")]
mod press_outside;
#[cfg(target_os = "linux")]
mod projection;
#[cfg(target_os = "linux")]
mod projection_video;
#[cfg(target_os = "linux")]
mod questions;
#[cfg(target_os = "linux")]
pub mod quick_settings;
#[cfg(target_os = "linux")]
mod record_index;
#[cfg(target_os = "linux")]
pub mod recovery;
#[cfg(target_os = "linux")]
mod remote_door;
#[cfg(target_os = "linux")]
mod render;
#[cfg(target_os = "linux")]
mod rotation;
#[cfg(target_os = "linux")]
pub mod run;
#[cfg(target_os = "linux")]
mod run_questions;
#[cfg(target_os = "linux")]
mod seams;
#[cfg(target_os = "linux")]
mod seats;
#[cfg(target_os = "linux")]
pub mod seeded_apps;
#[cfg(target_os = "linux")]
mod send_to;
#[cfg(target_os = "linux")]
mod service_sessions;
#[cfg(target_os = "linux")]
pub mod session_package;
#[cfg(target_os = "linux")]
mod settings_sync;
#[cfg(target_os = "linux")]
mod setup;
#[cfg(target_os = "linux")]
mod shared;
mod shared_desktop;
#[cfg(target_os = "linux")]
mod shared_displays;
#[cfg(target_os = "linux")]
mod sheets;
#[cfg(target_os = "linux")]
pub mod shell;
#[cfg(target_os = "linux")]
mod shortcut_guide;
#[cfg(target_os = "linux")]
mod slot_binding;
#[cfg(target_os = "linux")]
mod slot_views;
#[cfg(target_os = "linux")]
pub mod slots;
#[cfg(target_os = "linux")]
mod sockets;
#[cfg(target_os = "linux")]
pub mod startup;
#[cfg(target_os = "linux")]
mod status;
#[cfg(target_os = "linux")]
pub mod surfaces;
#[cfg(target_os = "linux")]
mod switcher;
pub mod sync;
#[cfg(target_os = "linux")]
mod system_sounds;
mod task_programs;
#[cfg(target_os = "linux")]
mod task_programs_host;
#[cfg(target_os = "linux")]
mod task_undo;
#[cfg(target_os = "linux")]
pub mod termination;
#[cfg(target_os = "linux")]
pub mod test_control;
#[cfg(target_os = "linux")]
mod tools;
#[cfg(target_os = "linux")]
pub mod unlock;
#[cfg(target_os = "linux")]
mod unlock_history;
#[cfg(target_os = "linux")]
mod update_finish;
#[cfg(target_os = "linux")]
mod wallpapers;
#[cfg(target_os = "linux")]
mod wifi_continuity;
#[cfg(target_os = "linux")]
mod window_motion;
#[cfg(target_os = "linux")]
mod window_move;
#[cfg(target_os = "linux")]
mod window_share;
#[cfg(target_os = "linux")]
mod window_transfer;
#[cfg(target_os = "linux")]
mod window_warmth;
#[cfg(target_os = "linux")]
mod workspace_branch;
#[cfg(target_os = "linux")]
mod workspace_delete;
#[cfg(target_os = "linux")]
mod workspace_settings;
#[cfg(target_os = "linux")]
mod workspaces;

/// The effects a host provides: the compositor and the services a Linux
/// session hosts, and the page's own on the web.
pub mod platform;
#[cfg(target_os = "linux")]
pub use platform::session::*;
