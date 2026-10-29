//! The person's accounts and contacts, as the compositor holds them now,
//! deciding every route an account sends on or receives from.
//!
//! Each query asks the compositor for the records afresh, bounded by a
//! deadline; nothing here keeps a copy that could go stale.

use std::time::Duration;

use tilo_contact_directory::routing::{Route, SenderAccount};
use tilo_messaging::SendError;
use tilo_messaging_protocol::journal::Owner;
use tilo_messaging_protocol::routing::{LinkState, LiveBinding};
use tilo_messaging_protocol::signal_broker::{Authorization, SignalAuthority};

use super::identity::{Credential, Identity, service_name};
use crate::door::{Call, DoorLink, Reply};

const DEADLINE: Duration = Duration::from_secs(2);

enum Query {
    Account,
    Route(Route),
    Incoming(String),
}

/// The current records for one account: its identity, grant and scope.
pub(crate) struct CurrentAuthority {
    identity: Identity,
    credential: Credential,
    scope: String,
    door: DoorLink,
}

impl CurrentAuthority {
    /// # Errors
    /// Retired admission or grant, or a malformed scope.
    pub(crate) fn new(
        identity: &Identity,
        credential: &Credential,
        scope: &str,
        door: DoorLink,
    ) -> Result<Self, SendError> {
        identity.check()?;
        credential.check()?;
        if credential.owner != identity.owner
            || identity.owner.linking_generation == 0
            || scope.is_empty()
            || scope.len() > 512
            || scope.chars().any(char::is_control)
        {
            return Err(SendError::Revoked);
        }
        Ok(Self {
            identity: identity.clone(),
            credential: credential.clone(),
            scope: scope.into(),
            door,
        })
    }

    fn query(&self, query: &Query) -> Result<Option<Authorization>, SendError> {
        self.identity.check()?;
        self.credential.check()?;
        let Some(Reply::Person {
            accounts,
            directory,
        }) = self.door.call_blocking(Call::Person, DEADLINE)
        else {
            return Err(SendError::Unavailable);
        };
        let directory =
            <tilo_contact_directory::ContactDirectory as tilo_resource::Model>::from_json(
                &directory,
            )
            .map_err(|_| SendError::Unavailable)?;
        let result = resolve(
            &self.identity.owner,
            &self.scope,
            query,
            accounts,
            directory,
        );
        self.identity.check()?;
        self.credential.check()?;
        result
    }
}

fn resolve(
    owner: &Owner,
    scope: &str,
    query: &Query,
    accounts: accounts::AccountStore,
    directory: tilo_contact_directory::ContactDirectory,
) -> Result<Option<Authorization>, SendError> {
    let service = service_name(owner.service);
    let account = accounts
        .get(&accounts::AccountId::new(&owner.account))
        .filter(|account| {
            account.enabled
                && account.type_id.as_str() == service
                && account.owner.is_none()
                && account
                    .config
                    .get("messaging_scope")
                    .and_then(serde_json::Value::as_str)
                    == Some(scope)
                && account
                    .config
                    .get("linking_generation")
                    .and_then(serde_json::Value::as_u64)
                    == Some(owner.linking_generation)
        })
        .ok_or(SendError::Revoked)?;
    let sender = SenderAccount {
        id: tilo_resource::Id::from_raw(&owner.account),
        service: service.into(),
        scope: scope.into(),
        enabled: account.enabled,
    };
    let route = match query {
        Query::Account => return Ok(None),
        Query::Route(route) => {
            if !tilo_contact_directory::routing::routes(
                &directory,
                &route.person,
                service,
                &[sender],
            )
            .contains(route)
            {
                return Err(SendError::Revoked);
            }
            route.clone()
        }
        Query::Incoming(recipient) => {
            let mut matches = directory
                .people
                .iter()
                .flat_map(|(person, _)| {
                    tilo_contact_directory::routing::routes(
                        &directory,
                        person,
                        service,
                        std::slice::from_ref(&sender),
                    )
                })
                .filter(|route| route.recipient == *recipient);
            let found = matches.next().ok_or(SendError::Revoked)?;
            if matches.next().is_some() {
                return Err(SendError::Revoked);
            }
            found
        }
    };
    // The account's gate and its verified adapter supply the link; the
    // records are never credential or admission evidence.
    Ok(Some(Authorization::new(
        directory,
        accounts,
        LiveBinding {
            owner: owner.clone(),
            state: LinkState::Linked,
        },
        route,
    )))
}

impl SignalAuthority for CurrentAuthority {
    fn check_account_scope(&self, scope: &str) -> Result<(), SendError> {
        if scope != self.scope {
            return Err(SendError::Revoked);
        }
        self.check_account()
    }
    fn check_account(&self) -> Result<(), SendError> {
        match self.query(&Query::Account)? {
            None => Ok(()),
            Some(_) => Err(SendError::Unavailable),
        }
    }
    fn authorize(&self, route: &Route) -> Result<Authorization, SendError> {
        self.query(&Query::Route(route.clone()))?
            .ok_or(SendError::Unavailable)
    }
    fn incoming(&self, recipient: &str) -> Result<Authorization, SendError> {
        if recipient.is_empty() || recipient.len() > 512 || recipient.chars().any(char::is_control)
        {
            return Err(SendError::Revoked);
        }
        self.query(&Query::Incoming(recipient.into()))?
            .ok_or(SendError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_messaging_protocol::Service;

    fn owner() -> Owner {
        Owner {
            principal: "p".into(),
            device: "d".into(),
            service: Service::Sms,
            account: "a".into(),
            linking_generation: 1,
        }
    }

    #[test]
    fn an_account_the_records_do_not_hold_exactly_is_refused() {
        let store = accounts::AccountStore::default();
        assert_eq!(
            resolve(
                &owner(),
                "sms:a",
                &Query::Account,
                store,
                tilo_contact_directory::ContactDirectory::default()
            )
            .err(),
            Some(SendError::Revoked)
        );
    }
}
