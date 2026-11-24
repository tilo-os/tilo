#![recursion_limit = "256"]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::single_match_else,
    clippy::string_slice,
    clippy::significant_drop_tightening,
    clippy::many_single_char_names,
    clippy::type_complexity,
    clippy::unused_self,
    clippy::unreachable,
    clippy::future_not_send,
    clippy::arithmetic_side_effects,
    clippy::while_immutable_condition,
    reason = "in a test these are the assertion, not a hazard; one scenario reads in order, and its names are its domain's"
)]
// One test binary instead of four: each tests/*.rs linked this whole crate as a
// binary of its own. #[path] keeps the files where they are, and nextest runs
// each test in a process of its own, so the isolation separate binaries gave
// them stays. A test that starts itself again as a helper process selects
// itself by its name here, which carries its module.

// The hosts and brokers these tests run decode peer bytes in worker processes
// started from this executable, which is the test binary here: the one worker
// entry every module's tests start.
tilo_automerge_isolate::test_worker!();

#[path = "account_sign_ins.rs"]
mod account_sign_ins;
#[path = "agent_fixture.rs"]
mod agent_fixture;
#[path = "agent_keys.rs"]
mod agent_keys;
#[path = "agent_machine.rs"]
mod agent_machine;
#[path = "agent_member.rs"]
mod agent_member;
#[path = "agent_models.rs"]
mod agent_models;
#[path = "agent_on_device.rs"]
mod agent_on_device;
#[path = "agent_presence.rs"]
mod agent_presence;
#[path = "agent_scheduling.rs"]
mod agent_scheduling;
#[path = "agent_terminal.rs"]
mod agent_terminal;
#[path = "agent_turns.rs"]
mod agent_turns;
#[path = "agent_windows.rs"]
mod agent_windows;
#[path = "api_admin.rs"]
mod api_admin;
#[path = "api_hosted.rs"]
mod api_hosted;
#[path = "api_key.rs"]
mod api_key;
#[path = "api_model_asks.rs"]
mod api_model_asks;
#[path = "api_records.rs"]
mod api_records;
#[path = "api_workspaces.rs"]
mod api_workspaces;
#[path = "app_views.rs"]
mod app_views;
#[path = "chat.rs"]
mod chat;
#[path = "customer_workspaces.rs"]
mod customer_workspaces;
#[path = "daemon_flows.rs"]
mod daemon_flows;
#[path = "desktop_page.rs"]
mod desktop_page;
#[path = "device_actions.rs"]
mod device_actions;
#[path = "device_host.rs"]
mod device_host;
#[path = "device_writes.rs"]
mod device_writes;
#[path = "files_collab.rs"]
mod files_collab;
#[path = "files_content.rs"]
mod files_content;
#[path = "files_paths.rs"]
mod files_paths;
/// What the Files tests read of what a daemon keeps.
mod files_support;
#[path = "files_views.rs"]
mod files_views;
#[path = "support/guests.rs"]
mod guests;
/// The one fixture a test reaches a device's daemon through.
mod host;
#[path = "hosted_agent_terminal.rs"]
mod hosted_agent_terminal;
#[path = "hosted_mail_accounts.rs"]
mod hosted_mail_accounts;
#[path = "hosted_terminal_fresh_tab.rs"]
mod hosted_terminal_fresh_tab;
#[path = "hosted_web_call.rs"]
mod hosted_web_call;
#[path = "invocations.rs"]
mod invocations;
#[path = "joining.rs"]
mod joining;
#[path = "keeping_pages.rs"]
mod keeping_pages;
#[path = "machine_secrets.rs"]
mod machine_secrets;
#[path = "migrate_engine.rs"]
mod migrate_engine;
#[path = "migrate_window.rs"]
mod migrate_window;
#[path = "provided.rs"]
mod provided;
#[path = "refresh_reads.rs"]
mod refresh_reads;
#[path = "root_holder_login.rs"]
mod root_holder_login;
#[path = "runtime_daemon_process.rs"]
mod runtime_daemon_process;
#[path = "search.rs"]
mod search;
#[path = "session_api_connect.rs"]
mod session_api_connect;
#[path = "shared_workspace.rs"]
mod shared_workspace;
#[path = "shell_link.rs"]
mod shell_link;
#[path = "terminal_streams.rs"]
mod terminal_streams;
#[path = "tracker.rs"]
mod tracker;
#[path = "workspace_list.rs"]
mod workspace_list;
#[path = "workspace_members.rs"]
mod workspace_members;
#[path = "workspace_records.rs"]
mod workspace_records;
