//! A private native host can register a newly approved account without replacing
//! the capability lent to its already open, initially empty Messages window.
use crate::{
    HistoryFuture, HistoryHost, MessagingClient, MessagingHost, MessagingRouter, RegistrationError,
    SendFuture, SendRequest, ServiceAccount,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, RwLock, Weak},
};
use tilo_contact_directory::routing::Route;

type Guard = Box<dyn std::any::Any + Send + Sync>;
struct Observer {
    changed: Arc<dyn Fn() + Send + Sync>,
    guards: Mutex<Vec<Guard>>,
}
/// Native host control, separate from the app-lent client. Registering metadata
/// is insufficient: each client retains its original runtime authority checks.
pub trait MessagingRegistry: Send + Sync {
    fn client(self: Arc<Self>) -> MessagingClient;
    /// # Errors
    /// Refuses another principal or an already registered exact service/account.
    fn register(
        &self,
        identity: ServiceAccount,
        client: MessagingClient,
    ) -> Result<(), RegistrationError>;
}
struct Registry {
    principal: String,
    current: RwLock<MessagingRouter>,
    services: Mutex<BTreeMap<ServiceAccount, MessagingClient>>,
    observers: Mutex<Vec<Weak<Observer>>>,
}
/// Start empty; no accounts, grants or resources are synthesized.
/// # Errors
/// Refuses an invalid principal.
pub fn messaging_registry(
    principal: &str,
) -> Result<Arc<dyn MessagingRegistry>, RegistrationError> {
    Ok(Arc::new(Registry {
        principal: principal.into(),
        current: RwLock::new(MessagingRouter::new(principal, vec![])?),
        services: Mutex::new(BTreeMap::new()),
        observers: Mutex::new(vec![]),
    }))
}
impl Registry {
    fn snapshot(&self) -> Option<MessagingClient> {
        self.current.read().ok().map(|router| router.client())
    }
}
impl MessagingRegistry for Registry {
    fn client(self: Arc<Self>) -> MessagingClient {
        MessagingClient::new(self.principal.clone(), self)
    }
    fn register(
        &self,
        identity: ServiceAccount,
        client: MessagingClient,
    ) -> Result<(), RegistrationError> {
        let mut services = self
            .services
            .lock()
            .map_err(|_| RegistrationError::InvalidIdentity)?;
        if services.contains_key(&identity) {
            return Err(RegistrationError::DuplicateAccount);
        }
        let mut next: Vec<_> = services
            .iter()
            .map(|(id, client)| (id.clone(), client.clone()))
            .collect();
        next.push((identity.clone(), client.clone()));
        let router = MessagingRouter::new(&self.principal, next)?;
        let observed = self
            .observers
            .lock()
            .map_err(|_| RegistrationError::InvalidIdentity)?;
        let observers: Vec<_> = observed.iter().filter_map(Weak::upgrade).collect();
        *self
            .current
            .write()
            .map_err(|_| RegistrationError::InvalidIdentity)? = router;
        services.insert(identity, client.clone());
        drop(services);
        drop(observed);
        // Foreign host callbacks and observer notifications run outside registry
        // locks. Keep observer admission locked through account publication:
        // an observer either joins this notification set or reads the new
        // registry snapshot. Concurrent attachment can duplicate a wake,
        // but cannot leave an open account without its history observer.
        for observer in observers {
            if let Some(guard) = client.history().observe(observer.changed.clone())
                && let Ok(mut guards) = observer.guards.lock()
            {
                guards.push(guard);
            }
            (observer.changed)();
        }
        Ok(())
    }
}
impl MessagingHost for Registry {
    fn groups(&self, account: &ServiceAccount) -> Option<crate::GroupClient> {
        self.snapshot()?.groups(account)
    }
    fn setup(&self, account: &ServiceAccount) -> Option<crate::SetupClient> {
        self.snapshot()?.setup(account)
    }
    fn account_state(&self, account: &ServiceAccount) -> crate::AccountState {
        self.snapshot()
            .map_or(crate::AccountState::Unavailable, |client| {
                client.account_state(account)
            })
    }
    fn supports(&self, route: &Route) -> bool {
        self.snapshot().is_some_and(|client| client.supports(route))
    }
    fn send(&self, request: SendRequest) -> SendFuture {
        let client = self.snapshot();
        Box::pin(async move {
            client
                .ok_or(crate::SendError::Unavailable)?
                .send(request)
                .await
        })
    }
}
impl HistoryHost for Registry {
    fn read(&self, route: Route, limit: usize) -> HistoryFuture {
        let client = self.snapshot();
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
    fn observe(&self, changed: Arc<dyn Fn() + Send + Sync>) -> Option<Guard> {
        let observer = Arc::new(Observer {
            changed,
            guards: Mutex::new(vec![]),
        });
        let mut observers = self.observers.lock().ok()?;
        observers.retain(|owner| owner.strong_count() > 0);
        if observers.len() >= 64 {
            return None;
        }
        observers.push(Arc::downgrade(&observer));
        drop(observers);
        if let Some(guard) = self.snapshot()?.history().observe(observer.changed.clone()) {
            observer.guards.lock().ok()?.push(guard);
        }
        Some(Box::new(observer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Host {
        changed: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    }
    impl MessagingHost for Host {
        fn supports(&self, route: &Route) -> bool {
            route.sender.as_str() == "first" && route.service == "signal"
        }
        fn account_state(&self, _: &ServiceAccount) -> crate::AccountState {
            crate::AccountState::Ready
        }
        fn send(&self, _: SendRequest) -> SendFuture {
            Box::pin(async { Err(crate::SendError::Unavailable) })
        }
    }
    impl HistoryHost for Host {
        fn read(&self, _: Route, _: usize) -> HistoryFuture {
            Box::pin(async { Err(crate::HistoryError::Unavailable) })
        }
        fn observe(&self, changed: Arc<dyn Fn() + Send + Sync>) -> Option<Guard> {
            *self.changed.lock().unwrap() = Some(changed);
            Some(Box::new(()))
        }
    }
    #[test]
    fn initially_empty_loan_receives_the_first_host_registration_and_history_wake() {
        let registry = messaging_registry("principal").unwrap();
        let client = registry.clone().client();
        let identity = ServiceAccount::new("signal", "first");
        assert_eq!(
            client.account_state(&identity),
            crate::AccountState::Unavailable
        );
        let changes = Arc::new(AtomicUsize::new(0));
        let observed = changes.clone();
        let _guard = client
            .history()
            .observe(Arc::new(move || {
                observed.fetch_add(1, Ordering::Relaxed);
            }))
            .unwrap();
        let host = Arc::new(Host {
            changed: Mutex::new(None),
        });
        let service = MessagingClient::new("principal", host.clone());
        registry
            .register(identity.clone(), service.clone())
            .unwrap();
        assert_eq!(client.account_state(&identity), crate::AccountState::Ready);
        assert_eq!(changes.load(Ordering::Relaxed), 1);
        let callback = host.changed.lock().unwrap().as_ref().unwrap().clone();
        callback();
        assert_eq!(
            changes.load(Ordering::Relaxed),
            2,
            "an observer attached before the first account receives its actual history changes"
        );
        assert_eq!(
            registry.register(identity.clone(), service),
            Err(RegistrationError::DuplicateAccount)
        );
        assert_eq!(client.account_state(&identity), crate::AccountState::Ready);
        assert_eq!(
            client.account_state(&ServiceAccount::new("whatsapp", "first")),
            crate::AccountState::Unavailable
        );
        assert_eq!(
            registry.register(
                ServiceAccount::new("signal", "foreign"),
                MessagingClient::new("other", host)
            ),
            Err(RegistrationError::WrongPrincipal)
        );
        assert_eq!(
            client.account_state(&ServiceAccount::new("signal", "foreign")),
            crate::AccountState::Unavailable
        );
    }
    #[test]
    fn concurrent_first_account_publication_keeps_the_actual_history_subscription() {
        for _ in 0..64 {
            let registry = messaging_registry("principal").unwrap();
            let host = Arc::new(Host {
                changed: Mutex::new(None),
            });
            let identity = ServiceAccount::new("signal", "first");
            let start = Arc::new(std::sync::Barrier::new(2));
            let writer = registry.clone();
            let service = MessagingClient::new("principal", host.clone());
            let ready = start.clone();
            let registered = std::thread::spawn(move || {
                ready.wait();
                writer
                    .register(ServiceAccount::new("signal", "first"), service)
                    .unwrap();
            });
            let client = registry.clone().client();
            let changes = Arc::new(AtomicUsize::new(0));
            let observed = changes.clone();
            start.wait();
            let guard = client
                .history()
                .observe(Arc::new(move || {
                    observed.fetch_add(1, Ordering::SeqCst);
                }))
                .unwrap();
            registered.join().unwrap();
            assert_eq!(client.account_state(&identity), crate::AccountState::Ready);
            let before = changes.load(Ordering::SeqCst);
            let callback = host.changed.lock().unwrap().as_ref().unwrap().clone();
            callback();
            assert!(
                changes.load(Ordering::SeqCst) > before,
                "a real history wake must reach an observer admitted during first-account publication"
            );
            drop(guard);
        }
    }
}
