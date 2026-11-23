#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]
// One test binary: each file under tests/ is a module of it.

#[cfg(target_os = "linux")]
#[path = "agent_socket.rs"]
pub mod agent_socket;
