//! Messages' connection to the person's messaging service (`tilo-messaging`).
//!
//! The service holds the accounts; the launcher connects an app to it and
//! names the app on the connection before handing it over. What an app gets
//! from it are the same [`EnrollmentClient`](crate::EnrollmentClient) and
//! [`MessagingClient`](crate::MessagingClient) a host would lend it.

pub mod wire;

#[cfg(unix)]
pub mod link;

#[cfg(unix)]
pub mod client;

/// The session service name apps declare (`[sandbox.services] messaging`).
pub const SERVICE: &str = "messaging";
/// The only app that holds messaging accounts.
pub const MESSAGES_APP: &str = "so.tilo.messages";
/// The app that answers the service's permission requests.
pub const PASSWORDS_APP: &str = "so.tilo.passwords";
/// Where the launcher hands an app its connection.
pub const SERVICE_FD_ENV: &str = "TILO_SVC_MESSAGING_FD";

/// The enrollment and messaging clients of the service this app was
/// launched with; `None` when it has none (the service is not running, or
/// this platform has no session services).
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

/// The permission requests the messaging service waits on, for Passwords
/// launched with a connection to it.
pub fn proposals() -> Vec<accounts::CredentialProposal> {
    #[cfg(unix)]
    {
        client::Connection::inherited()
            .map(|connection| {
                connection
                    .proposals()
                    .iter()
                    .map(wire::Proposal::credential)
                    .collect()
            })
            .unwrap_or_default()
    }
    #[cfg(not(unix))]
    {
        Vec::new()
    }
}

/// How many times those requests changed.
pub fn proposals_revision() -> Option<u64> {
    #[cfg(unix)]
    {
        client::Connection::inherited().map(|connection| connection.proposals_revision())
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Answer one of those requests.
///
/// # Errors
/// There is no service, or the request is gone.
pub fn decide(proposal: &str, decision: accounts::AccessDecision) -> Result<(), crate::SendError> {
    let decision = match decision {
        accounts::AccessDecision::Deny => wire::Decision::Deny,
        accounts::AccessDecision::AllowForSession => wire::Decision::Session,
        accounts::AccessDecision::AllowAlways => wire::Decision::Always,
        accounts::AccessDecision::AllowOnce => return Err(crate::SendError::InvalidRequest),
    };
    #[cfg(unix)]
    {
        client::Connection::inherited()
            .ok_or(crate::SendError::Unavailable)?
            .decide(proposal, decision)
    }
    #[cfg(not(unix))]
    {
        let _ = (proposal, decision);
        Err(crate::SendError::Unavailable)
    }
}
