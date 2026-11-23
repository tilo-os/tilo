#![recursion_limit = "256"]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
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

#[path = "agent_fixture.rs"]
mod agent_fixture;
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
#[path = "chat.rs"]
mod chat;
#[path = "customer_workspaces.rs"]
mod customer_workspaces;
#[path = "daemon_flows.rs"]
mod daemon_flows;
#[path = "device_actions.rs"]
mod device_actions;
#[path = "device_host.rs"]
mod device_host;
#[path = "device_writes.rs"]
mod device_writes;
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
#[path = "joining.rs"]
mod joining;
#[path = "root_holder_login.rs"]
mod root_holder_login;
#[path = "runtime_daemon_process.rs"]
mod runtime_daemon_process;
#[path = "session_api_connect.rs"]
mod session_api_connect;
#[path = "tracker.rs"]
mod tracker;
#[path = "workspace_list.rs"]
mod workspace_list;
#[path = "workspace_members.rs"]
mod workspace_members;
#[path = "workspace_records.rs"]
mod workspace_records;
