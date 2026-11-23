//! The one place a refusal becomes the text an MCP client reads.
//!
//! Each refusal starts with the code the API documents
//! (`docs/tilo-sh/errors.md`) and then what happened, so a caller that reads
//! the typed answer gets the code rather than a bare `failed`, and one that
//! reads the text still reads a sentence.

use super::BridgeError;

pub(crate) fn said(error: BridgeError) -> String {
    match error {
        BridgeError::Host(host) => host.said(),
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
