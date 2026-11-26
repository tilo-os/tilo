//! Where the workspace's activity is served: the provider its calls name, the resource its
//! actions are of, and the application its calls are admitted under. Its records, its actions
//! and their arguments are declared where the session answers them
//! (`tilo_session_core::desktop::activity::vocabulary`), and a test there binds these names to
//! that declaration.

/// The provider the session serves workspace activity under.
pub const PROVIDER: &str = "tilo.activity";
/// The application whose window shows it, and whose grants reach it.
pub use tilo_app_id::apps::ACTIVITY as APP_ID;
