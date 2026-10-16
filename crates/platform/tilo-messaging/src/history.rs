//! Read-only private history capability lent by an authenticated host.
//! Selecting metadata cannot manufacture this capability or credential access.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tilo_contact_directory::routing::Route;
use zeroize::Zeroizing;

pub const MAX_HISTORY_MESSAGES: usize = 128;
const MAX_TEXT_BYTES: usize = 32 * 1024;
const MAX_PAGE_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Incoming,
    Outgoing,
}

/// A protocol submission or server acknowledgment never implies delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Submitted,
    ServerAcknowledged,
    RecipientDelivered,
    RecipientRead,
    Uncertain,
}

/// Private transient projection; never a replicated application resource.
#[derive(Clone, PartialEq, Eq)]
pub struct Message {
    pub id: String,
    pub text: Zeroizing<String>,
    pub direction: Direction,
    pub timestamp_ms: u64,
    pub delivery: Option<Delivery>,
}

impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Message(<private>)")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct HistoryPage {
    pub route: Route,
    pub messages: Vec<Message>,
    pub more: bool,
}

impl std::fmt::Debug for HistoryPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HistoryPage(<private>)")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryError {
    Unavailable,
    Revoked,
    InvalidRoute,
    InvalidPage,
}

pub type HistoryFuture =
    Pin<Box<dyn Future<Output = Result<HistoryPage, HistoryError>> + Send + 'static>>;

/// The host binds this object to the admitted principal and credential grant.
/// Every read must revalidate the grant, Contacts association and current
/// provider identity revision, enforce a deadline, and return only this route.
/// Apps must not receive account stores, vault keys or raw adapter connections.
pub trait HistoryHost: Send + Sync {
    /// Private data changed; the observer receives no message or routing data.
    /// The returned guard unregisters when the mounted view closes.
    fn observe(
        &self,
        _changed: Arc<dyn Fn() + Send + Sync>,
    ) -> Option<Box<dyn std::any::Any + Send + Sync>> {
        None
    }
    fn read(&self, route: Route, limit: usize) -> HistoryFuture;
}

/// Read-only service supplied through the existing host loan mechanism.
#[derive(Clone)]
pub struct HistoryClient {
    host: Arc<dyn HistoryHost>,
    principal: String,
}

impl HistoryClient {
    /// The mounting host supplies an already authorized, principal-bound service.
    /// This constructor neither opens a socket nor authorizes an account.
    pub fn new(principal: impl Into<String>, host: Arc<dyn HistoryHost>) -> Self {
        Self {
            host,
            principal: principal.into(),
        }
    }

    /// The verified person root this host capability acts for, never a label.
    pub fn principal(&self) -> &str {
        &self.principal
    }

    pub fn observe(
        &self,
        changed: Arc<dyn Fn() + Send + Sync>,
    ) -> Option<Box<dyn std::any::Any + Send + Sync>> {
        self.host.observe(changed)
    }
    /// Read a bounded page and refuse another identity/account/route or malformed
    /// data before any private content becomes visible in the consumer.
    ///
    /// # Errors
    /// Returns the host's refusal or a mismatched/oversized history response.
    pub async fn read(&self, route: Route) -> Result<HistoryPage, HistoryError> {
        if self.principal.trim().is_empty()
            || self.principal.len() > 256
            || self.principal.chars().any(char::is_control)
        {
            return Err(HistoryError::InvalidRoute);
        }
        validate_route(&route)?;
        let page = self.host.read(route.clone(), MAX_HISTORY_MESSAGES).await?;
        validate_page(&page, &route)?;
        Ok(page)
    }
}

pub(crate) fn validate_route(route: &Route) -> Result<(), HistoryError> {
    if !["tilo", "signal", "whatsapp", "sms"].contains(&route.service.as_str())
        || [
            route.person.as_str(),
            route.association.as_str(),
            route.sender.as_str(),
            &route.scope,
            &route.recipient,
            &route.revision,
        ]
        .iter()
        .any(|value| value.trim().is_empty() || value.len() > 4096)
    {
        return Err(HistoryError::InvalidRoute);
    }
    Ok(())
}

fn validate_page(page: &HistoryPage, route: &Route) -> Result<(), HistoryError> {
    if &page.route != route || page.messages.len() > MAX_HISTORY_MESSAGES {
        return Err(HistoryError::InvalidPage);
    }
    let mut ids = BTreeSet::new();
    let mut bytes = 0usize;
    for message in &page.messages {
        bytes = bytes
            .saturating_add(message.text.len())
            .saturating_add(message.id.len());
        if message.id.is_empty()
            || message.id.len() > 512
            || !ids.insert(&message.id)
            || message.timestamp_ms == 0
            || message.text.len() > MAX_TEXT_BYTES
            || bytes > MAX_PAGE_BYTES
            || (message.direction == Direction::Incoming && message.delivery.is_some())
            || (message.direction == Direction::Outgoing && message.delivery.is_none())
        {
            return Err(HistoryError::InvalidPage);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::{Context, Poll, Waker};
    use tilo_contact_directory::routing;
    use tilo_resource::Id;

    struct Host(HistoryPage);
    impl HistoryHost for Host {
        fn read(&self, _: Route, limit: usize) -> HistoryFuture {
            assert_eq!(limit, MAX_HISTORY_MESSAGES);
            Box::pin(std::future::ready(Ok(self.0.clone())))
        }
    }

    fn route() -> Route {
        routing::Route {
            person: Id::from_raw("person"),
            association: Id::from_raw("association"),
            sender: Id::from_raw("account"),
            service: "signal".into(),
            scope: "scope".into(),
            recipient: "+12025550123".into(),
            revision: "identity-generation".into(),
        }
    }
    fn page() -> HistoryPage {
        HistoryPage {
            route: route(),
            more: false,
            messages: vec![Message {
                id: "message".into(),
                text: Zeroizing::new("private text".into()),
                direction: Direction::Outgoing,
                timestamp_ms: 1,
                delivery: Some(Delivery::Submitted),
            }],
        }
    }
    fn read(page: HistoryPage) -> Poll<Result<HistoryPage, HistoryError>> {
        let client = HistoryClient::new("fixture-owner", Arc::new(Host(page)));
        let mut future = Box::pin(client.read(route()));
        let mut cx = Context::from_waker(Waker::noop());
        future.as_mut().poll(&mut cx)
    }

    #[test]
    fn host_responses_must_match_every_identity_component() {
        assert_eq!(read(page()), Poll::Ready(Ok(page())));
        for component in 0..7 {
            let mut forged = page();
            match component {
                0 => forged.route.person = Id::from_raw("other-person"),
                1 => forged.route.association = Id::from_raw("other-association"),
                2 => forged.route.sender = Id::from_raw("other-account"),
                3 => forged.route.service = "whatsapp".into(),
                4 => forged.route.scope = "other-scope".into(),
                5 => forged.route.recipient = "+12025550999".into(),
                _ => forged.route.revision = "changed-identity".into(),
            }
            assert_eq!(read(forged), Poll::Ready(Err(HistoryError::InvalidPage)));
        }
    }

    #[test]
    fn duplicate_oversized_or_falsely_delivered_incoming_rows_are_refused() {
        let mut invalid = page();
        invalid.messages.extend(invalid.messages.clone());
        assert_eq!(read(invalid), Poll::Ready(Err(HistoryError::InvalidPage)));
        let mut invalid = page();
        for message in &mut invalid.messages {
            message.text = Zeroizing::new("x".repeat(MAX_TEXT_BYTES + 1));
        }
        assert_eq!(read(invalid), Poll::Ready(Err(HistoryError::InvalidPage)));
        let mut invalid = page();
        for message in &mut invalid.messages {
            message.direction = Direction::Incoming;
        }
        assert_eq!(read(invalid), Poll::Ready(Err(HistoryError::InvalidPage)));
        assert_eq!(format!("{:?}", page()), "HistoryPage(<private>)");
    }
}
