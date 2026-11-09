//! The effects a host provides to the session's roles.
//!
//! The session's roles publish what a person sees and does as resources and
//! actions. What they cannot do themselves they ask a host for: surfaces to
//! draw on, apps to run, a store to keep records in, a peer to reach the
//! person's other devices, and the machine's own services. On a Linux
//! desktop that host is [`session`], this session's compositor and the
//! services it starts. On the web it is the page, which provides the same
//! effects itself: the canvas the surfaces are drawn on, the browser's app
//! host, the browser's store, and the page's own transport.
//!
//! Each concern the roles ask for lives here as one seam, so a role names
//! the effect and never a platform crate: [`watched`] is the store a value
//! is kept current in, and a concern with a Linux half in [`session`] gains
//! its page half here beside it.

#[cfg(target_os = "linux")]
pub mod session;

/// A value the host keeps current: a directory of files it watches on a
/// Linux desktop, the page's own store on the web.
pub mod watched;
