//! An app's view of the machine it runs on, through one connection on every
//! desktop platform.
//!
//! The vocabulary is the typed system information service's
//! ([`protocol`], [`client`]): readings pushed per subscription, a process
//! list cut to what the app was granted, and ending the person's own
//! processes. Where the readings come from is this crate's choice:
//!
//! - on Linux, the Tilo session reads the machine and serves it
//!   (`tilo_system_stats`): an app's sandbox has a process namespace of its
//!   own, so it could not read the machine itself. The launcher hands the
//!   connection over; an unconfined run dials the session's socket under the
//!   runtime directory ([`tilo_paths::runtime_dir`]);
//! - elsewhere (macOS), the platform confines an app without hiding the
//!   process table, and no session serves readings: the same service runs in
//!   this process over the platform's own interfaces ([`local`]).

#[cfg(unix)]
pub mod local;

pub use tilo_system_stats::protocol;
pub use tilo_system_stats::{Grant, Grants, SERVICE};

/// The app's side of the service.
#[cfg(unix)]
pub mod client {
    pub use tilo_system_stats::client::{Client, ClientSubscription, Notify};
}

/// This process's connection to the system information service, made on
/// first use and shared: the one the launcher handed it, the session's for
/// an unconfined run, or (where no session serves one) a service in this
/// process. `None` when there is none.
#[cfg(unix)]
#[must_use]
pub fn connection(app_id: &str) -> Option<client::Client> {
    #[cfg(target_os = "linux")]
    {
        client::Client::shared(session_socket, app_id)
    }
    #[cfg(not(target_os = "linux"))]
    {
        local::connection(app_id)
    }
}

/// The session's socket, where an unconfined copy finds the service.
#[cfg(target_os = "linux")]
fn session_socket() -> Option<std::path::PathBuf> {
    Some(
        tilo_paths::runtime_dir()?
            .join("tilo/services")
            .join(format!("{SERVICE}.sock")),
    )
}
