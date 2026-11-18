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
            "stale_revision: the resource changed since the revision this call named, \
                 which you read: the action is offered at a newer revision now. Invoke it \
                 again and omit revision, which runs it against the directory as it is now, \
                 or re-run tilo_query and invoke it at the revision that page names"
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

/// `action`, unless the caller's permissions deny it: the catalogue lists such an action to an
/// agent as not allowed ([`tilo_actions::Availability::not_allowed`]), and a call of it is
/// refused as such, with where that is changed, before the workspace is asked anything.
pub(crate) fn not_allowed(
    action: tilo_actions::ActionDescriptor,
) -> Result<tilo_actions::ActionDescriptor, String> {
    match action.contract.availability.refusal() {
        Some(refused) => Err(format!("not_permitted: {refused}")),
        None => Ok(action),
    }
}
