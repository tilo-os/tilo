//! Host-lent sending authority, separate from readable account metadata.
use crate::{Delivery, HistoryClient, HistoryHost};
use std::{future::Future, pin::Pin, sync::Arc};
use tilo_contact_directory::routing::Route;
use zeroize::Zeroizing;

pub struct SendRequest {
    pub route: Route,
    pub operation: String,
    pub text: Zeroizing<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendError {
    Unavailable,
    Revoked,
    InvalidRequest,
    AlreadyReserved,
    Uncertain,
}
pub struct SendReceipt {
    pub route: Route,
    pub operation: String,
    pub delivery: Delivery,
}
pub type SendFuture =
    Pin<Box<dyn Future<Output = Result<SendReceipt, SendError>> + Send + 'static>>;
/// The host must revalidate its current grant and provider identity before I/O.
/// Cancellation of a caller never authorizes replay of an admitted operation.
pub trait MessagingHost: HistoryHost {
    fn groups(&self, _account: &crate::ServiceAccount) -> Option<crate::GroupClient> {
        None
    }
    /// An exact account's private native setup capability, if installed.
    fn setup(&self, _account: &crate::ServiceAccount) -> Option<crate::SetupClient> {
        None
    }
    /// Non-secret native lifecycle. A ready report never authorizes a route.
    fn account_state(&self, _account: &crate::ServiceAccount) -> crate::AccountState {
        crate::AccountState::Unknown
    }
    fn supports(&self, _route: &Route) -> bool {
        false
    }
    fn send(&self, request: SendRequest) -> SendFuture;
}
#[derive(Clone)]
pub struct MessagingClient {
    host: Arc<dyn MessagingHost>,
    history: HistoryClient,
}
impl MessagingClient {
    pub fn groups(&self, account: &crate::ServiceAccount) -> Option<crate::GroupClient> {
        self.host
            .groups(account)
            .filter(|groups| groups.principal() == self.principal() && groups.account() == account)
    }
    /// The authenticated mounting host supplies this capability, never metadata.
    pub fn new(principal: impl Into<String>, host: Arc<dyn MessagingHost>) -> Self {
        Self {
            history: HistoryClient::new(principal, host.clone()),
            host,
        }
    }
    pub fn setup(&self, account: &crate::ServiceAccount) -> Option<crate::SetupClient> {
        self.host
            .setup(account)
            .filter(|setup| setup.principal() == self.principal() && setup.account() == account)
    }
    pub fn account_state(&self, account: &crate::ServiceAccount) -> crate::AccountState {
        self.host.account_state(account)
    }
    pub fn supports(&self, route: &Route) -> bool {
        self.host.supports(route)
    }
    pub fn history(&self) -> HistoryClient {
        self.history.clone()
    }
    pub fn principal(&self) -> &str {
        self.history.principal()
    }
    /// Submit once on the exact route. An ambiguous answer is never retried.
    /// # Errors
    /// Refuses invalid requests, unavailable authority and mismatched replies.
    pub async fn send(&self, request: SendRequest) -> Result<SendReceipt, SendError> {
        if request.operation.is_empty()
            || request.operation.len() > 512
            || request
                .operation
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
            || request.text.trim().is_empty()
            || request.text.len() > 32 * 1024
            || crate::history::validate_route(&request.route).is_err()
        {
            return Err(SendError::InvalidRequest);
        }
        let route = request.route.clone();
        let operation = request.operation.clone();
        let receipt = self.host.send(request).await?;
        if receipt.route != route
            || receipt.operation != operation
            || !matches!(
                receipt.delivery,
                Delivery::Submitted | Delivery::ServerAcknowledged | Delivery::Uncertain
            )
        {
            return Err(SendError::Uncertain);
        }
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HistoryError, HistoryFuture};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll, Waker};
    use tilo_resource::Id;

    struct Host {
        calls: AtomicUsize,
        forged: u8,
    }
    impl HistoryHost for Host {
        fn read(&self, _: Route, _: usize) -> HistoryFuture {
            Box::pin(std::future::ready(Err(HistoryError::Unavailable)))
        }
    }
    impl MessagingHost for Host {
        fn send(&self, request: SendRequest) -> SendFuture {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut route = request.route;
            let mut operation = request.operation;
            let mut delivery = Delivery::ServerAcknowledged;
            match self.forged {
                1 => route.sender = Id::from_raw("other-account"),
                2 => route.recipient = "other-recipient".into(),
                3 => operation = "other-operation".into(),
                4 => delivery = Delivery::RecipientDelivered,
                5 => delivery = Delivery::RecipientRead,
                _ => {}
            }
            Box::pin(std::future::ready(Ok(SendReceipt {
                route,
                operation,
                delivery,
            })))
        }
    }
    fn request() -> SendRequest {
        SendRequest {
            route: Route {
                person: Id::from_raw("person"),
                association: Id::from_raw("association"),
                sender: Id::from_raw("account"),
                service: "signal".into(),
                scope: "scope".into(),
                recipient: "recipient".into(),
                revision: "revision".into(),
            },
            operation: "operation".into(),
            text: Zeroizing::new("private message".into()),
        }
    }
    fn send(host: Arc<Host>, request: SendRequest) -> Poll<Result<Delivery, SendError>> {
        let client = MessagingClient::new("principal", host);
        let mut future = Box::pin(client.send(request));
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .map(|result| result.map(|receipt| receipt.delivery))
    }
    #[test]
    fn send_replies_cannot_switch_route_operation_or_claim_recipient_delivery() {
        for forged in 0..=5 {
            let host = Arc::new(Host {
                calls: AtomicUsize::new(0),
                forged,
            });
            let result = send(host.clone(), request());
            assert_eq!(
                result,
                Poll::Ready(if forged == 0 {
                    Ok(Delivery::ServerAcknowledged)
                } else {
                    Err(SendError::Uncertain)
                })
            );
            assert_eq!(
                host.calls.load(Ordering::SeqCst),
                1,
                "an uncertain answer is never retried"
            );
        }
    }
    #[test]
    fn invalid_sends_are_refused_before_the_host_is_called() {
        let host = Arc::new(Host {
            calls: AtomicUsize::new(0),
            forged: 0,
        });
        for invalid in 0..4 {
            let mut request = request();
            match invalid {
                0 => request.text.clear(),
                1 => request.operation = "two operations".into(),
                2 => request.text = Zeroizing::new("x".repeat(32 * 1024 + 1)),
                _ => request.route.scope.clear(),
            }
            assert_eq!(
                send(host.clone(), request),
                Poll::Ready(Err(SendError::InvalidRequest))
            );
        }
        assert_eq!(host.calls.load(Ordering::SeqCst), 0);
    }
}
