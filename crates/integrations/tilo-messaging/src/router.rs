//! Exact service/account dispatch for host-owned messaging capabilities.
//! Registration is performed by the authenticated host before mounting; no
//! active account, display name or transport availability selects a fallback.
use crate::{
    HistoryFuture, HistoryHost, MessagingClient, MessagingHost, SendError, SendFuture, SendRequest,
};
use std::{collections::BTreeMap, sync::Arc};
use tilo_contact_directory::routing::Route;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "wire", derive(serde::Serialize, serde::Deserialize))]
pub struct ServiceAccount {
    pub service: String,
    pub account: String,
}
impl ServiceAccount {
    pub fn new(service: impl Into<String>, account: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            account: account.into(),
        }
    }
    fn valid(&self) -> bool {
        matches!(
            self.service.as_str(),
            "signal" | "whatsapp" | "sms" | "tilo"
        ) && !self.account.is_empty()
            && self.account.len() <= 512
            && !self
                .account
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
    }
    fn route(route: &Route) -> Self {
        Self::new(&route.service, route.sender.as_str())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistrationError {
    InvalidIdentity,
    WrongActor,
    DuplicateAccount,
}

/// An immutable per-person registry: a new registration needs a new mounting loan,
/// and a mounted window keeps exactly its admitted capabilities.
#[derive(Clone)]
pub struct MessagingRouter {
    actor: String,
    services: BTreeMap<ServiceAccount, MessagingClient>,
}
impl MessagingRouter {
    pub fn new(
        actor: &str,
        services: Vec<(ServiceAccount, MessagingClient)>,
    ) -> Result<Self, RegistrationError> {
        if actor.is_empty() || actor.len() > 512 || actor.chars().any(char::is_control) {
            return Err(RegistrationError::InvalidIdentity);
        }
        let mut registered = BTreeMap::new();
        for (identity, client) in services {
            if !identity.valid() {
                return Err(RegistrationError::InvalidIdentity);
            }
            if client.actor() != actor {
                return Err(RegistrationError::WrongActor);
            }
            if registered.insert(identity, client).is_some() {
                return Err(RegistrationError::DuplicateAccount);
            }
        }
        Ok(Self {
            actor: actor.into(),
            services: registered,
        })
    }
    pub fn client(&self) -> MessagingClient {
        MessagingClient::new(&self.actor, Arc::new(self.clone()))
    }
}
impl MessagingHost for MessagingRouter {
    fn groups(&self, account: &ServiceAccount) -> Option<crate::GroupClient> {
        self.services.get(account)?.groups(account)
    }
    fn setup(&self, account: &ServiceAccount) -> Option<crate::SetupClient> {
        self.services.get(account)?.setup(account)
    }
    fn account_state(&self, account: &ServiceAccount) -> crate::AccountState {
        self.services
            .get(account)
            .map_or(crate::AccountState::Unavailable, |client| {
                client.account_state(account)
            })
    }
    fn supports(&self, route: &Route) -> bool {
        self.services
            .get(&ServiceAccount::route(route))
            .is_some_and(|service| service.supports(route))
    }
    fn send(&self, request: SendRequest) -> SendFuture {
        let client = self
            .services
            .get(&ServiceAccount::route(&request.route))
            .cloned();
        Box::pin(async move { client.ok_or(SendError::Unavailable)?.send(request).await })
    }
}
impl HistoryHost for MessagingRouter {
    fn read(
        &self,
        route: Route,
        cursor: Option<crate::HistoryCursor>,
        limit: usize,
    ) -> HistoryFuture {
        let client = self.services.get(&ServiceAccount::route(&route)).cloned();
        Box::pin(async move {
            if limit != crate::MAX_HISTORY_MESSAGES {
                return Err(crate::HistoryError::InvalidRoute);
            }
            client
                .ok_or(crate::HistoryError::Unavailable)?
                .history()
                .read(route, cursor)
                .await
        })
    }
    fn observe(
        &self,
        changed: Arc<dyn Fn() + Send + Sync>,
    ) -> Option<Box<dyn std::any::Any + Send + Sync>> {
        let guards: Vec<_> = self
            .services
            .values()
            .filter_map(|service| service.history().observe(changed.clone()))
            .collect();
        if guards.is_empty() {
            None
        } else {
            Some(Box::new(guards))
        }
    }
}
