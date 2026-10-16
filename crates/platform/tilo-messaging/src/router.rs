//! Exact service/account dispatch for host-owned messaging capabilities.
//! Registration is performed by the authenticated host before mounting; no
//! active account, display name or transport availability selects a fallback.
use crate::{
    HistoryFuture, HistoryHost, MessagingClient, MessagingHost, SendError, SendFuture, SendRequest,
};
use std::{collections::BTreeMap, sync::Arc};
use tilo_contact_directory::routing::Route;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
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
    WrongPrincipal,
    DuplicateAccount,
}

/// An immutable per-person registry. New registrations require a new mounting
/// loan; already mounted windows retain exactly their admitted capabilities.
#[derive(Clone)]
pub struct MessagingRouter {
    principal: String,
    services: BTreeMap<ServiceAccount, MessagingClient>,
}
impl MessagingRouter {
    /// Assemble already-authorized services; this creates no account grant.
    /// # Errors
    /// Refuses malformed identities, another person or duplicate service/account.
    pub fn new(
        principal: &str,
        services: Vec<(ServiceAccount, MessagingClient)>,
    ) -> Result<Self, RegistrationError> {
        if principal.is_empty() || principal.len() > 512 || principal.chars().any(char::is_control)
        {
            return Err(RegistrationError::InvalidIdentity);
        }
        let mut registered = BTreeMap::new();
        for (identity, client) in services {
            if !identity.valid() {
                return Err(RegistrationError::InvalidIdentity);
            }
            if client.principal() != principal {
                return Err(RegistrationError::WrongPrincipal);
            }
            if registered.insert(identity, client).is_some() {
                return Err(RegistrationError::DuplicateAccount);
            }
        }
        Ok(Self {
            principal: principal.into(),
            services: registered,
        })
    }
    pub fn client(&self) -> MessagingClient {
        MessagingClient::new(&self.principal, Arc::new(self.clone()))
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
    fn read(&self, route: Route, limit: usize) -> HistoryFuture {
        let client = self.services.get(&ServiceAccount::route(&route)).cloned();
        Box::pin(async move {
            if limit != crate::MAX_HISTORY_MESSAGES {
                return Err(crate::HistoryError::InvalidRoute);
            }
            client
                .ok_or(crate::HistoryError::Unavailable)?
                .history()
                .read(route)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Delivery, HistoryPage, SendReceipt};
    use std::{
        sync::Mutex,
        task::{Context, Poll, Waker},
    };
    use tilo_resource::Id;
    type Calls = Arc<Mutex<Vec<(String, String)>>>;
    struct Host {
        calls: Calls,
        identity: ServiceAccount,
    }
    impl MessagingHost for Host {
        fn supports(&self, route: &Route) -> bool {
            ServiceAccount::route(route) == self.identity
        }
        fn send(&self, request: SendRequest) -> SendFuture {
            self.calls.lock().expect("fixture calls lock").push((
                request.route.service.clone(),
                request.route.sender.as_str().into(),
            ));
            Box::pin(std::future::ready(Ok(SendReceipt {
                route: request.route,
                operation: request.operation,
                delivery: Delivery::ServerAcknowledged,
            })))
        }
    }
    impl HistoryHost for Host {
        fn read(&self, route: Route, _: usize) -> HistoryFuture {
            self.calls
                .lock()
                .expect("fixture value")
                .push((route.service.clone(), route.sender.as_str().into()));
            Box::pin(std::future::ready(Ok(HistoryPage {
                route,
                messages: Vec::new(),
                more: false,
            })))
        }
    }
    fn route(service: &str, account: &str) -> Route {
        Route {
            service: service.into(),
            sender: Id::from_raw(account),
            person: Id::from_raw("person"),
            association: Id::from_raw("association"),
            scope: "scope".into(),
            recipient: "recipient".into(),
            revision: "revision".into(),
        }
    }
    fn client(principal: &str, service: &str, account: &str, calls: &Calls) -> MessagingClient {
        MessagingClient::new(
            principal,
            Arc::new(Host {
                calls: calls.clone(),
                identity: ServiceAccount::new(service, account),
            }),
        )
    }
    #[test]
    fn exact_service_and_account_dispatch_never_falls_back() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let services = [
            ServiceAccount::new("signal", "personal"),
            ServiceAccount::new("signal", "work"),
            ServiceAccount::new("whatsapp", "personal"),
        ];
        let router = MessagingRouter::new(
            "owner",
            services
                .iter()
                .map(|id| {
                    (
                        id.clone(),
                        client("owner", &id.service, &id.account, &calls),
                    )
                })
                .collect(),
        )
        .expect("owned router")
        .client();
        let history = router.history();
        for id in &services {
            let route = route(&id.service, &id.account);
            assert!(router.supports(&route));
            let mut read = Box::pin(history.read(route.clone()));
            assert!(matches!(
                read.as_mut().poll(&mut Context::from_waker(Waker::noop())),
                Poll::Ready(Ok(_))
            ));
            let mut send = Box::pin(router.send(SendRequest {
                route,
                operation: "operation".into(),
                text: zeroize::Zeroizing::new("fixture text".into()),
            }));
            assert!(matches!(
                send.as_mut().poll(&mut Context::from_waker(Waker::noop())),
                Poll::Ready(Ok(_))
            ));
        }
        let before = calls.lock().expect("fixture calls lock").clone();
        for missing in [route("sms", "personal"), route("signal", "missing")] {
            assert!(!router.supports(&missing));
            let mut send = Box::pin(router.send(SendRequest {
                route: missing.clone(),
                operation: "missing".into(),
                text: zeroize::Zeroizing::new("fixture text".into()),
            }));
            assert!(matches!(
                send.as_mut().poll(&mut Context::from_waker(Waker::noop())),
                Poll::Ready(Err(SendError::Unavailable))
            ));
            let mut read = Box::pin(history.read(missing));
            assert!(matches!(
                read.as_mut().poll(&mut Context::from_waker(Waker::noop())),
                Poll::Ready(Err(crate::HistoryError::Unavailable))
            ));
        }
        assert_eq!(*calls.lock().expect("fixture calls lock"), before);
        assert_eq!(
            before,
            services
                .iter()
                .flat_map(|id| vec![(id.service.clone(), id.account.clone()); 2])
                .collect::<Vec<_>>()
        );
    }
    #[test]
    fn duplicate_and_cross_person_registrations_are_refused() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let id = ServiceAccount::new("signal", "personal");
        let owned = client("owner", "signal", "personal", &calls);
        assert!(matches!(
            MessagingRouter::new(
                "owner",
                vec![(id.clone(), owned.clone()), (id.clone(), owned)]
            ),
            Err(RegistrationError::DuplicateAccount)
        ));
        assert!(matches!(
            MessagingRouter::new(
                "owner",
                vec![(id, client("other", "signal", "personal", &calls))]
            ),
            Err(RegistrationError::WrongPrincipal)
        ));
        assert!(calls.lock().expect("fixture calls lock").is_empty());
    }
}
