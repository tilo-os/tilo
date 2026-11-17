//! Coding agents a workspace's host runs in the workspace's machine.
//!
//! A coding agent is one kind of sub-agent: an agent of the workspace's own
//! record of its agents whose work is done by a coding harness (Claude Code
//! is the first) in the workspace's Linux machine, not by the host's own
//! loop. Everything a person sees of it is the sub-agent's: its job, its
//! conversation, how it stands. What this module adds is the machine's part:
//!
//! - [`harness`]: what a coding harness is to its host, so which one runs is
//!   a parameter; [`claude_code`] keeps Claude Code's own ways in one file.
//! - [`model_endpoint`]: the harness's model, asked in its provider's own
//!   protocol inside the machine and answered on the host by the person's own
//!   model, so the person's key never enters the machine, every answer is
//!   counted on their ledger and their day's limit stops it.
//! - [`run`]: one run, followed for as long as it lasts: its endpoints
//!   answered, what it says written into the sub-agent's conversation as it
//!   says it, a message carried to it, a stop that asks before it ends.
//! - [`working_copy`]: a working copy and a branch of its own for each
//!   coding agent, so several work on one repository at once.
//! - [`runner`]: the coding agents a host runs now, each started by the
//!   first words it is given and at rest when it has nothing to do.
//!
//! The run itself is a served run in the machine ([`ush::served_runs`]): the
//! harness is started through the guest agent's serving mode, which opens the
//! endpoints on the machine's loopback for the run's own processes alone.

pub mod claude_code;
pub mod harness;
pub mod model_endpoint;
pub mod run;
pub mod runner;
pub mod working_copy;

#[cfg(test)]
mod tests;
