//! # Tilo action services
//!
//! The actions of the services a device and a workspace run: the apps and files a
//! device keeps, the environments it holds and the terminals, machines and windows
//! in them, the line its sockets answer on, the tools and tasks it hands an agent,
//! and the records of its own device.
//!
//! Each module names one service's actions and the provider they are answered
//! under. They sit beside [`tilo_actions`], which keeps the action model itself:
//! resources and their actions, the remote protocol every peer speaks, and the
//! provider trait these services implement.

pub mod agent_tools;
/// The device's project checks an app runs by id, and the receipt each run leaves.
pub mod approved_checks;

pub mod browser;
/// The workspace's own folder as the device serves it: a file put into it, and Files' verbs on it.
pub mod device_files;

pub mod downloaded_apps;

pub mod environment_commands;

pub mod environment_device;

pub mod environment_machines;

pub mod environment_terminals;

pub mod environment_window;

pub mod formats;

#[cfg(not(target_arch = "wasm32"))]
pub mod handed;

pub mod line;

pub mod linux_apps;
/// Machines a person prepared to install: the plan and its waiting entry in
/// their Servers workspace, read and written through the session that holds
/// it. The command and the installer's window name one set of steps.
pub mod machines;

pub mod mcp_server;

pub mod nodes;

pub mod own_device;

pub mod server_account;

pub mod session_actions;


pub mod task_programs;

#[cfg(any(test, feature = "test-support"))]
pub mod test_provider;

pub mod window_follow;

pub mod workspace_events;

/// Adding a computer to the person's devices with a one-use ticket: the computer's own daemon
/// issues it, prints the one string, and the new computer logs in with it.
pub mod device_tickets;
/// A device's migrations: another product's data brought into the workspace by
/// `review`, `choose` and `start`, followed by `status`, on the resource of the
/// device that holds the data (`devices/<key>/migrate`). An app finds this
/// device's resource in its catalogue by its kind, and calls its steps as any
/// caller does; a run is one invocation of `start`, stopped by cancelling it.
pub mod migrations;
/// Where an offloadable invocation runs, asked of the device's daemon: a
/// session's broker places each offloadable invocation through the daemon,
/// which holds the link to the workspace's other devices.
pub mod offload_place;
pub mod workspace_invocations;

#[cfg(not(target_arch = "wasm32"))]
pub use line::{
    CallLanes, Handover, LineClient, LinePut, Served, Take, serve_lines, serve_lines_handing,
    serve_lines_with,
};
#[cfg(not(target_arch = "wasm32"))]
pub use line::{Carry, CarryAnswer, carry_answer_line, carry_line, carry_request};
pub use line::{answer_line, answer_message, frame_line, line_refusal};
pub use nodes::{
    NODE_PERMITS_ACTION, NODE_RESOURCE_PREFIX, SHARE_NODE_ACTION, UNSHARE_NODE_ACTION, node_target,
};
pub use session_actions::PROVIDER as SESSION_ACTIONS_PROVIDER;
#[cfg(any(test, feature = "test-support"))]
pub use test_provider::TestProvider;

/// The providers whose actions a workspace's host answers for a person in it: what the host
/// keeps for them rather than for the workspace ([`tilo_actions::remote::person_telling`], the vault, their accounts
/// and models, the connections made in their name, and the formats they read). A device of
/// theirs reaches them over the device action service, as the person the transport certified,
/// and its own dispatch answers nothing of them locally.
pub const WORKSPACE_KEPT_PROVIDERS: [&str; 6] = [
    tilo_actions::remote::ACCOUNTS_PROVIDER,
    tilo_actions::remote::MODELS_PROVIDER,
    tilo_actions::remote::VAULT_PROVIDER,
    tilo_actions::remote::person_telling::PROVIDER,
    tilo_actions::remote::CONNECTIONS_PROVIDER,
    crate::formats::PROVIDER,
];

/// Whether `provider` is one a workspace's host answers for the person
/// ([`WORKSPACE_KEPT_PROVIDERS`]).
#[must_use]
pub fn kept_by_workspace_host(provider: &str) -> bool {
    WORKSPACE_KEPT_PROVIDERS.contains(&provider)
}
