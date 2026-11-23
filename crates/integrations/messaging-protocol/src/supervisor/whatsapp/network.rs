//! Recheck native account admission at the actual SDK network boundary.
use super::{
    Arc, HttpClient, HttpRequest, HttpResponse, Network, Transport, TransportEvent,
    TransportFactory,
};
use crate::supervisor::AccountAuthority;

fn check(authority: &dyn AccountAuthority) -> whatsapp_rust::anyhow::Result<()> {
    authority
        .check()
        .map_err(|_| whatsapp_rust::anyhow::anyhow!("native account access unavailable"))
}

impl Network {
    /// Keep the host's current signed admission and grant on every HTTP exchange,
    /// transport creation and transport write. Revocation permits teardown only.
    /// This neither authorizes endpoints nor creates network access; the supplied
    /// native providers remain responsible for their existing egress policy.
    #[must_use]
    pub fn guarded(self, authority: Arc<dyn AccountAuthority>) -> Self {
        Self {
            http: Arc::new(GuardedHttp {
                source: self.http,
                authority: authority.clone(),
            }),
            transport: Arc::new(GuardedFactory {
                source: self.transport,
                authority,
            }),
        }
    }
}
struct GuardedHttp {
    source: Arc<dyn HttpClient>,
    authority: Arc<dyn AccountAuthority>,
}
#[whatsapp_rust::async_trait]
impl HttpClient for GuardedHttp {
    async fn execute(&self, request: HttpRequest) -> whatsapp_rust::anyhow::Result<HttpResponse> {
        check(self.authority.as_ref())?;
        let response = self.source.execute(request).await?;
        check(self.authority.as_ref())?;
        Ok(response)
    }
}
struct GuardedFactory {
    source: Arc<dyn TransportFactory>,
    authority: Arc<dyn AccountAuthority>,
}
#[whatsapp_rust::async_trait]
impl TransportFactory for GuardedFactory {
    async fn create_transport(
        &self,
    ) -> whatsapp_rust::anyhow::Result<(
        Arc<dyn Transport>,
        whatsapp_rust::async_channel::Receiver<TransportEvent>,
    )> {
        check(self.authority.as_ref())?;
        let (transport, events) = self.source.create_transport().await?;
        if let Err(error) = check(self.authority.as_ref()) {
            transport.disconnect().await;
            return Err(error);
        }
        Ok((
            Arc::new(GuardedTransport {
                source: transport,
                authority: self.authority.clone(),
            }),
            events,
        ))
    }
}
struct GuardedTransport {
    source: Arc<dyn Transport>,
    authority: Arc<dyn AccountAuthority>,
}
#[whatsapp_rust::async_trait]
impl Transport for GuardedTransport {
    async fn send(&self, data: whatsapp_rust::bytes::Bytes) -> whatsapp_rust::anyhow::Result<()> {
        if let Err(error) = check(self.authority.as_ref()) {
            self.source.disconnect().await;
            return Err(error);
        }
        let result = self.source.send(data).await;
        if let Err(error) = check(self.authority.as_ref()) {
            self.source.disconnect().await;
            return Err(error);
        }
        result
    }
    async fn disconnect(&self) {
        self.source.disconnect().await;
    }
}

#[cfg(test)]
#[path = "network_tests.rs"]
pub(super) mod tests;
