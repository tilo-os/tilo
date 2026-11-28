//! `tilo-agent-session` — agents as first-class session members.
//!
//! An agent works in a workspace the way a person's device does, not through a
//! side channel:
//!
//! - **Identity.** Its own device key, a capability-less `DeviceCert`
//!   issued under the user's root by ordinary device certification (the
//!   approving device is where consent is given), and a root-signed
//!   `tilo_server_protocol::delegation::Delegation` naming its label, cursor
//!   colour, workspace and apps. The host confines the device to that workspace and
//!   treats it as its own actor.
//! - **Records.** Each record the agent works in is the workspace's, changed
//!   through the app's own process and its published actions, as a person's
//!   window changes it.
//! - **Work.** The workspace's own agents (its `tilo.workspace.agent` roster) take
//!   their turns through the device's dispatch; each signs its changes with its own key.
//! - **Memory.** There is none of its own. What an agent learns it reads
//!   from the workspace's records, and what it keeps it writes there through an
//!   app's operations.

#![deny(rust_2018_idioms)]

pub mod model;
pub mod record_actions;
pub mod shell;
pub mod transform;
