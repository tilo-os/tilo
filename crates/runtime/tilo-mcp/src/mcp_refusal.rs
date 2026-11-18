//! The one place a refusal becomes the text an MCP client reads.
//!
//! Each refusal starts with the code the API documents
//! (`docs/tilo-sh/errors.md`) and then what happened, so a caller that reads
//! the typed answer gets the code rather than a bare `failed`, and one that
//! reads the text still reads a sentence.

use super::BridgeError;

pub(crate) fn said(error: BridgeError) -> String {
    match error {
        // What happened, and what to do: a bare code leaves the caller
        // guessing whether to retry, and a caller that retries the same
        // stale revision fails the same way.
        BridgeError::Host(tilo_actions::remote::ActionCallError::StaleRevision) => {
            "stale_revision: the resource changed since you read it. Call the action \
                 again with tilo_do, which runs it against the resource as it is now, or \
                 tilo_read the resource first to see what changed"
                .to_owned()
        }
        BridgeError::Host(ref host) => {
            // The code the API documents, then the host's own words rather
            // than a second restatement of them.
            let code = match host {
                // What the host could not run now is `unavailable`: the code
                // the API documents, not the one this crate spells.
                tilo_actions::remote::ActionCallError::Unavailable { .. } => "unavailable",
                host => host.code(),
            };
            match host {
                // Refused by the workspace's permissions: said as that, with where it is
                // changed, never as an action the caller cannot name.
                tilo_actions::remote::ActionCallError::NotPermitted => {
                    format!("{code}: {}", tilo_actions::Availability::NOT_ALLOWED_HERE)
                }
                tilo_actions::remote::ActionCallError::InvalidRequest { reason }
                | tilo_actions::remote::ActionCallError::Refused { reason }
                | tilo_actions::remote::ActionCallError::Unavailable { reason } => {
                    format!("{code}: {reason}")
                }
                host => format!("{code}: {host}"),
            }
        }
        BridgeError::Bridge(message) => message,
    }
}

/// `action`, unless the catalogue lists it to the agent as refused: its permissions deny it
/// ([`tilo_actions::Availability::not_allowed`]), said with where that is changed, or it only
/// changes what the person at the publishing device sees
/// ([`tilo_actions::Availability::shown_here`]), said naming it. Either is refused before the
/// workspace is asked anything.
pub(crate) fn not_allowed(
    action: tilo_actions::ActionDescriptor,
) -> Result<tilo_actions::ActionDescriptor, String> {
    match action.contract.availability.refusal() {
        Some((tilo_actions::Availability::PRIVATE_VIEW, refused)) => Err(format!(
            "{}: #{} {refused}; do what it leads to with the app's own actions",
            tilo_actions::Availability::PRIVATE_VIEW,
            action.address.action
        )),
        Some((_, refused)) => Err(format!("not_permitted: {refused}")),
        None => Ok(action),
    }
}
