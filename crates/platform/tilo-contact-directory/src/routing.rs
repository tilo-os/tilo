//! Explicit operation account choice. Defaults never choose a sending identity.
//! The provider must revalidate its current identity revision and device grant
//! before any I/O. These values carry provenance, never secrets or authority.

use tilo_resource::Id;

use crate::ContactDirectory;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SenderAccount {
    pub id: Id,
    pub service: String,
    pub scope: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub person: Id,
    pub association: Id,
    pub sender: Id,
    pub service: String,
    pub scope: String,
    pub recipient: String,
    pub revision: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountChoice {
    Unavailable,
    Ready(Route),
    SelectionRequired(Vec<Route>),
}

/// A sender metadata record is not evidence that its credentials are present.
/// Adapters supply only currently usable senders from the existing OS accounts.
pub fn routes(
    directory: &ContactDirectory,
    person: &Id,
    service: &str,
    senders: &[SenderAccount],
) -> Vec<Route> {
    let Some(selected) = directory
        .people
        .get(person)
        .filter(|person| !person.removed)
    else {
        return Vec::new();
    };
    let mut routes = Vec::new();
    for (association, endpoint) in selected
        .endpoints
        .iter()
        .filter(|(_, endpoint)| !endpoint.removed && endpoint.service == service)
    {
        if directory
            .resolve_account(
                service,
                &endpoint.scope,
                &endpoint.recipient,
                &endpoint.revision,
            )
            .as_ref()
            != Ok(person)
        {
            continue;
        }
        for sender in senders.iter().filter(|sender| {
            sender.enabled
                && !sender.id.as_str().trim().is_empty()
                && sender.service == service
                && sender.scope == endpoint.scope
        }) {
            let route = Route {
                person: person.clone(),
                association: association.clone(),
                sender: sender.id.clone(),
                service: service.into(),
                scope: endpoint.scope.clone(),
                recipient: endpoint.recipient.clone(),
                revision: endpoint.revision.clone(),
            };
            if !routes.contains(&route) {
                routes.push(route);
            }
        }
    }
    routes.sort_by(|a, b| {
        (a.sender.as_str(), a.association.as_str())
            .cmp(&(b.sender.as_str(), b.association.as_str()))
    });
    routes
}

/// A stale explicit choice refuses, even if another account is available.
pub fn select_route(routes: &[Route], choice: Option<&Route>) -> AccountChoice {
    if let Some(choice) = choice {
        return if routes.contains(choice) {
            AccountChoice::Ready(choice.clone())
        } else {
            AccountChoice::Unavailable
        };
    }
    match routes {
        [] => AccountChoice::Unavailable,
        [only] => AccountChoice::Ready(only.clone()),
        _ => AccountChoice::SelectionRequired(routes.to_vec()),
    }
}
