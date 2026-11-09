//! The effects a host provides to the session's roles.
//!
//! The session's roles publish what a person sees and does as resources and
//! actions. What they cannot do themselves they ask a host for: surfaces to
//! draw on, apps to run, a store to keep records in, a peer to reach the
//! person's other devices, and the machine's own services. On a Linux
//! desktop that host is [`session`], this session's compositor and the
//! services it starts. On the web it is the page.

#[cfg(target_os = "linux")]
pub mod session;
