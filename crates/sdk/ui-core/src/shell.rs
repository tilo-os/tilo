//! The shell actions, which an app asks of the shell it runs in through
//! [`Call::host`](tilo_app::framework::Call::host): named here so an app reaches them through `ui`
//! rather than depending on the host crate.
use tilo_desktop_model::commands::{RESOURCE as SESSION, layout};

/// The provider the session's broker answers the shell's actions under.
pub const PROVIDER: &str = layout::PROVIDER;
/// The shell's session resource the actions are on.
pub const RESOURCE: &str = SESSION;
/// Open a resource, content or web address in a window (or, with a `place` of `tab-of` and a `group`, a
/// tab of that group).
pub const OPEN: &str = layout::OPEN;
