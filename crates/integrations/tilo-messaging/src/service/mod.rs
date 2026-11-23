//! Messages' connection to the person's messaging service.
//!
//! The service holds the accounts; the launcher connects an app to it and names
//! the app on the connection before handing it over. The app then gets the same
//! clients a host would lend it.

pub mod wire;

#[cfg(unix)]
pub mod link;

#[cfg(unix)]
pub mod client;

pub const SERVICE: &str = "messaging";
/// The only app that holds messaging accounts.
pub use tilo_app_id::apps::MESSAGES as MESSAGES_APP;
pub const SERVICE_FD_ENV: &str = "TILO_SVC_MESSAGING_FD";

pub fn inherited() -> Option<(crate::EnrollmentClient, crate::MessagingClient)> {
    #[cfg(unix)]
    {
        client::Connection::inherited()
            .map(|connection| (connection.enrollment(), connection.messaging()))
    }
    #[cfg(not(unix))]
    {
        None
    }
}
