//! The native Tilo session: a thin host, as the web page is one.
//!
//! The compositor ([`tilo_compositor`]) shows buffers and routes input. The
//! session runs the desktop it owns (`tilo_session_core::desktop`) and the
//! tilolang shell that draws it (`apps/tl/shell`): the shell's views over the
//! whole screen, and the window policy that places each window in the well
//! the shell leaves it ([`platform::surfaces::desktop`]); and the session's
//! own surfaces, each on a layer of its own: the People panel, the sheets,
//! and setup over everything while a person first uses the device. What the
//! desktop asks of its host comes back as [`DesktopAsk`]s, carried out by the
//! ask loop in `asks`.
//!
//! Everything a person can do here is a published action: the shell's and
//! the chrome's, and each surface app's through its API
//! ([`ui::device_local_api::SharedApi`]). [`Session::resources`] lists
//! them and [`Session::invoke`] runs one, as any caller of the action
//! protocol on this device does.
//!
//! The session also hosts the system services it was started with
//! ([`Options::services`]): connectivity, audio and the rest publish their
//! resources beside the desktop's, and [`Session::invoke`] reaches their
//! actions as it reaches the desktop's, by address.
// A login asked of the daemon through tilo-api reaches iroh's endpoint, which is deep enough that
// the compiler's default recursion limit gives up when it proves the login's future is `Send`.
#![recursion_limit = "256"]

// The session's state between frames, as every host runs it, and the roles
// that keep it: the shell, the chrome, the records, the people and the
// workspaces — what every host runs, the web page included.
mod assembly;
pub use assembly::Session;
mod error;
pub use error::Error;
// Nothing compiled for the web reads this state yet: the roles that do land
// with the batches that ungate them.
#[cfg_attr(
    target_arch = "wasm32",
    allow(
        unused_imports,
        reason = "the roles that read this state land with their batches"
    )
)]
#[cfg(target_os = "linux")]
pub(crate) use assembly::BoxAsk;
pub(crate) use assembly::Shared;

mod activity_provider;
mod continuations;
mod daemon_fixture;
pub mod host_actions;
pub mod power_menu;
pub mod records;
pub mod search;
#[cfg(target_os = "linux")]
mod search_agent;
mod search_ask;
mod session_actions;
mod session_providers;
#[cfg(all(any(test, feature = "test-support"), target_os = "linux"))]
pub mod shell_session;
// The person's settings, which Settings' window drives through the broker as another provider's
// actions: the session keeps them in its settings service, so the answer is this host's.
#[cfg(target_os = "linux")]
mod settings_steps;
// What a message from another member is, and whether the workspace's policy grants it: the
// table every host asks.
pub use tilo_session_client::message_policy;
// This device's workspaces, and the sealed channels their records travel in.
pub use tilo_session_client::{engine, membership, workspace_keys, workspace_view};

// The Linux host's effects: the compositor, the services it starts, the
// devices and the processes. A web host provides these through `platform`.
#[cfg(target_os = "linux")]
mod a11y;
#[cfg(target_os = "linux")]
mod a11y_settings;
#[cfg(target_os = "linux")]
mod account_passwords;
pub mod activity;
// The desktop as the shell of the device's daemon: how its workspaces stand, and the actions
// that change them.
mod activity_models;
#[cfg(target_os = "linux")]
mod activity_service;
#[cfg(target_os = "linux")]
mod agent_sessions;
#[cfg(target_os = "linux")]
mod agents;
#[cfg(target_os = "linux")]
mod ambient_brightness;
#[cfg(target_os = "linux")]
mod api_server;
#[cfg(target_os = "linux")]
mod app_icons;
#[cfg(target_os = "linux")]
mod app_liveness;
mod app_names;
#[cfg(target_os = "linux")]
mod app_sheets;
#[cfg(target_os = "linux")]
mod app_socket;
#[cfg(target_os = "linux")]
mod app_stores;
#[cfg(target_os = "linux")]
pub mod app_tabs;
#[cfg(target_os = "linux")]
mod app_tiers;
#[cfg(target_os = "linux")]
mod app_wire;
#[cfg(target_os = "linux")]
pub mod apps;
#[cfg(target_os = "linux")]
mod asked_turns;
#[cfg(target_os = "linux")]
mod audio_output;
#[cfg(target_os = "linux")]
mod automation;
#[cfg(target_os = "linux")]
mod background;
#[cfg(target_os = "linux")]
mod bluetooth_handoff;
#[cfg(target_os = "linux")]
mod boot_join;
#[cfg(target_os = "linux")]
mod broker_services;
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
mod carets;
#[cfg(target_os = "linux")]
mod change_feed;
#[cfg(target_os = "linux")]
mod clipboard;
#[cfg(target_os = "linux")]
mod clock_wait;
#[cfg(target_os = "linux")]
mod collaboration;
#[cfg(target_os = "linux")]
pub mod comforts;
#[cfg(target_os = "linux")]
mod config;
#[cfg(target_os = "linux")]
pub mod connectors;
#[cfg(target_os = "linux")]
mod contacts;
#[cfg(target_os = "linux")]
pub mod controller_nav;
#[cfg(target_os = "linux")]
pub mod controllers;
#[cfg(target_os = "linux")]
mod controls;
#[cfg(target_os = "linux")]
mod daemon_actions;
#[cfg(target_os = "linux")]
pub mod desktop_services;
#[cfg(target_os = "linux")]
mod device_lists;
#[cfg(target_os = "linux")]
mod device_shell;
#[cfg(target_os = "linux")]
mod device_state;
#[cfg(target_os = "linux")]
mod device_tickets;
#[cfg(target_os = "linux")]
mod device_trust;
#[cfg(target_os = "linux")]
pub mod devices;
#[cfg(target_os = "linux")]
mod diagnostics;
#[cfg(target_os = "linux")]
mod dictation;
pub mod directory;
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
mod documents;
#[cfg(target_os = "linux")]
mod downloaded;
#[cfg(target_os = "linux")]
pub mod drives;
#[cfg(target_os = "linux")]
mod emoji;
#[cfg(target_os = "linux")]
mod engine_write;
#[cfg(target_os = "linux")]
mod env;
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
mod followed_resource;
#[cfg(target_os = "linux")]
mod formats;
#[cfg(target_os = "linux")]
#[cfg(target_os = "linux")]
mod governance;
#[cfg(target_os = "linux")]
mod held;
mod here;
#[cfg(target_os = "linux")]
mod host_shell;
#[cfg(target_os = "linux")]
mod listings;
mod live_list;
// What the session publishes for the quick settings to draw: the Devices section's rows, the
// clipboard's copies and the notification centre.
#[cfg(target_os = "linux")]
pub mod hosted;
#[cfg(target_os = "linux")]
pub mod hosted_browser;
#[cfg(target_os = "linux")]
mod hosted_workspaces;
mod i18n;
#[cfg(target_os = "linux")]
pub mod identity;
#[cfg(target_os = "linux")]
mod input_method;
#[cfg(target_os = "linux")]
mod input_sources;
pub mod invite;
#[cfg(target_os = "linux")]
mod item_lending;
#[cfg(target_os = "linux")]
#[cfg(target_os = "linux")]
pub mod keyboard;
mod keyboard_shortcuts;
mod keys;
#[cfg_attr(
    not(target_os = "linux"),
    expect(
        dead_code,
        reason = "a page answers on its own loop: only a Linux session lands work done off it"
    )
)]
mod landing;
// The machines a person planned to install, whose plans are entries in their Servers workspace:
// the same steps the command and the installer's window name, answered from this session.
#[cfg(target_os = "linux")]
mod launches;
// The workspaces' layouts as a device's workspace service holds them. A page never had this
// path: it keeps the layout of the workspace it shows in its own layout document.
#[cfg(target_os = "linux")]
mod layout;
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
#[cfg(target_os = "linux")]
mod machine_steps;
#[cfg(target_os = "linux")]
pub mod mcp_server;
pub mod mentions;
#[cfg(target_os = "linux")]
mod model_steps;
#[cfg(target_os = "linux")]
mod nearby_send;
#[cfg(target_os = "linux")]
#[cfg(target_os = "linux")]
mod network;
#[cfg(target_os = "linux")]
mod network_reach;
#[cfg(target_os = "linux")]
pub mod night_light;
#[cfg(target_os = "linux")]
pub mod notices;
#[cfg(target_os = "linux")]
mod notifications;
#[cfg(target_os = "linux")]
mod options;
#[cfg(target_os = "linux")]
mod owner_session;
#[cfg(target_os = "linux")]
mod page_track;
#[cfg(target_os = "linux")]
mod part_binding;
#[cfg(target_os = "linux")]
mod part_views;
#[cfg(target_os = "linux")]
pub mod parts;
#[cfg(target_os = "linux")]
mod pass;
pub mod people;
#[cfg(target_os = "linux")]
mod person_store;
#[cfg(target_os = "linux")]
pub(crate) use tilo_workspace_service::workspaces::PersonResources;
#[cfg(target_os = "linux")]
pub mod portal_link;
#[cfg(target_os = "linux")]
mod power;
#[cfg(target_os = "linux")]
pub use power::system_service;
#[cfg(target_os = "linux")]
mod media_controls;
#[cfg(target_os = "linux")]
mod program_asks;
#[cfg(target_os = "linux")]
mod projection;
#[cfg(target_os = "linux")]
mod projection_video;
#[cfg(target_os = "linux")]
mod published_workspace;
#[cfg(target_os = "linux")]
pub mod quick_settings;
#[cfg(target_os = "linux")]
mod ready;
#[cfg(target_os = "linux")]
mod record_content;
#[cfg(target_os = "linux")]
pub mod recovery;
#[cfg(target_os = "linux")]
mod render;
#[cfg(target_os = "linux")]
mod resource_index;
#[cfg(target_os = "linux")]
pub mod restart;
#[cfg(target_os = "linux")]
mod restore;
#[cfg(target_os = "linux")]
mod rotation;
#[cfg(target_os = "linux")]
pub mod run;
#[cfg(target_os = "linux")]
mod seat_services;
#[cfg(target_os = "linux")]
#[cfg(target_os = "linux")]
pub mod seeded_apps;
#[cfg(target_os = "linux")]
mod send_to;
#[cfg(target_os = "linux")]
mod server_account;
#[cfg(target_os = "linux")]
mod service_sessions;
#[cfg(target_os = "linux")]
mod services;
#[cfg(target_os = "linux")]
pub mod session_package;
#[cfg(target_os = "linux")]
mod session_settings;
#[cfg(target_os = "linux")]
mod setup;
#[cfg(target_os = "linux")]
mod shared;
#[cfg(target_os = "linux")]
mod shared_displays;
#[cfg(target_os = "linux")]
mod shared_input;
#[cfg(target_os = "linux")]
mod sheets;
#[cfg(target_os = "linux")]
pub mod shell;
#[cfg(target_os = "linux")]
mod shell_document;
#[cfg(target_os = "linux")]
mod sockets;
#[cfg(target_os = "linux")]
mod speakers;
#[cfg(target_os = "linux")]
pub mod startup;
pub mod status;
#[cfg(target_os = "linux")]
#[cfg(target_os = "linux")]
pub mod style_packages;
#[cfg(target_os = "linux")]
pub mod styles;
#[cfg(target_os = "linux")]
pub mod surfaces;
pub mod sync;
#[cfg(target_os = "linux")]
mod system_sounds;
#[cfg(target_os = "linux")]
mod task_programs_host;
#[cfg(target_os = "linux")]
pub mod termination;
#[cfg(target_os = "linux")]
pub mod test_control;
#[cfg(target_os = "linux")]
mod time;
#[cfg(target_os = "linux")]
mod tools;
#[cfg(target_os = "linux")]
pub mod unlock;
#[cfg(target_os = "linux")]
mod update_finish;
#[cfg(target_os = "linux")]
mod vpn;
#[cfg(target_os = "linux")]
mod wake;
#[cfg(target_os = "linux")]
mod wallpapers;
#[cfg(target_os = "linux")]
mod wifi_continuity;
#[cfg(target_os = "linux")]
mod window_follow;
#[cfg(target_os = "linux")]
mod window_motion;
#[cfg(target_os = "linux")]
mod window_move;
#[cfg(target_os = "linux")]
mod window_share;
/// The periods a window share is renewed in: how often a viewer asks the device sharing a window
/// again for what it shows (`TILO_WINDOW_SHARE_VIEW_AGAIN_MS`), and how long an ask keeps that
/// device's streams up (`TILO_WINDOW_SHARE_VIEW_EXPIRY_MS`). Production's own unless the
/// environment names them; a test that proves a share is renewed past those periods sets them.
pub const VIEW_AGAIN_ENV: &str = "TILO_WINDOW_SHARE_VIEW_AGAIN_MS";
/// [`VIEW_AGAIN_ENV`]'s partner: how long a viewer's ask keeps the sharing device's streams up.
pub const VIEW_EXPIRY_ENV: &str = "TILO_WINDOW_SHARE_VIEW_EXPIRY_MS";
#[cfg(target_os = "linux")]
mod window_transfer;
#[cfg(target_os = "linux")]
mod window_warmth;
pub mod windows;
#[cfg(target_os = "linux")]
mod windows_compat;
#[cfg(target_os = "linux")]
mod workspace_branch;
#[cfg(target_os = "linux")]
mod workspace_delete;
#[cfg(target_os = "linux")]
mod workspace_nodes;
pub mod workspace_people;
pub mod workspace_settings;
#[cfg(target_os = "linux")]
mod workspace_terminals;
#[cfg(target_os = "linux")]
mod workspaces;
// A session a display manager starts where Tilo's service runs as the person: the service starts
// the shell.
#[cfg(target_os = "linux")]
pub mod shell_start;

/// The effects a host provides: the compositor and the services a Linux
/// session hosts, and the page's own on the web.
pub mod platform;
#[cfg(target_os = "linux")]
pub use platform::session::*;
