//! The agent inside a legacy VM guest.
//!
//! Runs as root in the guest (a systemd system service) and listens on
//! vsock [`AGENT_PORT`](legacy_agent_protocol::AGENT_PORT). It starts apps as
//! the guest's one ordinary user, relays their stdio, mounts the host's
//! virtio-fs shares at the paths the host names, and reports what runs and
//! what is installed. It decides nothing about the host: which directories
//! are shared, and when the VM sleeps, are the host's decisions.

#![allow(
    clippy::missing_errors_doc,
    reason = "errors are typed enums whose variants document themselves"
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod apps;
pub mod launch;
pub mod mounts;
pub mod server;
pub mod system;
pub mod user;
