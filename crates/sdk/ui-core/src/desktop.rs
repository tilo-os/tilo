//! The actions the host publishes itself, which an app calls through
//! [`Call::host`](tilo_app::framework::Call::host): named here so an app
//! reaches them through `ui` rather than depending on the host's own crate.

/// The desktop's own actions: what an app asks of the desktop it runs in,
/// as the session's broker answers them.
pub mod desktop {
    use tilo_desktop_model::commands::{RESOURCE as SESSION, layout};

    /// The provider the session's broker answers the desktop's actions under.
    pub const PROVIDER: &str = layout::PROVIDER;
    /// The desktop's session resource the actions are on.
    pub const RESOURCE: &str = SESSION;
    /// Open a resource, content or web address in a window (or, with a
    /// `place` of `tab-of` and a `group`, a tab of that group).
    pub const OPEN: &str = layout::OPEN;
}
