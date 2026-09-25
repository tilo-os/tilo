//! Actual listener/task custody, deliberately independent of owner qualification.
use super::*;

pub struct RunningTransport {
    pub(super) receiver: IngressReceiver,
    pub(super) stop: watch::Sender<bool>,
    pub(super) tasks: JoinSet<Result<()>>,
    pub(super) endpoint: quinn::Endpoint,
    pub(super) failed: bool,
    pub(super) origin: String,
    pub(super) registrations: Arc<AtomicU8>,
    /// LIVE_V1 relay on this endpoint's `/moq/v21/live`, once installed.
    pub(super) live: LiveSlot,
}
/// Shared, install-once handle to the live relay served by `serve_h3`.
pub(crate) type LiveSlot = Arc<std::sync::OnceLock<qos_moq::live::LiveRelay>>;
impl RunningTransport {
    pub(super) async fn start_bound(
        listener: TcpListener,
        config: IngressConfig,
        endpoint: quinn::Endpoint,
        channels: (Ingress, IngressReceiver),
        relay: qos_moq::relay::Relay,
    ) -> Result<Self> {
        let uri: hyper::Uri = config.origin.parse()?;
        let authority = uri
            .authority()
            .context("origin authority missing")?
            .as_str()
            .to_owned();
        let (ingress, receiver) = channels;
        let adapter = HttpsAdapter::new(
            listener,
            &config.origin,
            config.assets,
            config.certificates.clone(),
            config.key.clone_key(),
            ingress.clone(),
        )?;
        let adapter = if let Some(gateway) = config.local_enrollment {
            adapter.with_local_enrollment(gateway)?
        } else {
            adapter
        };
        // The same routes over HTTP/3 on this port's QUIC endpoint, advertised
        // with Alt-Svc; HTTP/2 stays the fallback. `QOS_HTTPS_H3=0` disables.
        let (adapter, h3) = if crate::https::h3::enabled() {
            let adapter = adapter.with_alt_svc();
            let service = adapter.h3_service();
            (adapter, Some(service))
        } else {
            (adapter, None)
        };
        let (stop, shutdown) = watch::channel(false);
        let mut tasks = JoinSet::new();
        let registrations = Arc::new(AtomicU8::new(0));
        let (tcp_ready, tcp_started) = tokio::sync::oneshot::channel();
        let tcp_registration = registrations.clone();
        tasks.spawn(async move {
            let _registration = ServiceRegistration::enter(tcp_registration, 1);
            let _ = tcp_ready.send(());
            adapter.serve(shutdown).await.map_err(Into::into)
        });
        let serving = endpoint.clone();
        let shutdown = stop.subscribe();
        let origin = config.origin;
        let serving_origin = origin.clone();
        let udp_registration = registrations.clone();
        let (udp_ready, udp_started) = tokio::sync::oneshot::channel();
        // The same slot serves the HTTPS fallback carrier (TCP and HTTP/3).
        let live: LiveSlot = ingress.live.clone();
        let serving_live = live.clone();
        tasks.spawn(async move {
            let _registration = ServiceRegistration::enter(udp_registration, 2);
            let _ = udp_ready.send(());
            serve_h3(
                serving,
                authority,
                serving_origin,
                relay,
                serving_live,
                ingress,
                h3,
                shutdown,
            )
            .await
        });
        let running = Self {
            receiver,
            stop,
            tasks,
            endpoint,
            failed: false,
            origin,
            registrations,
            live,
        };
        tcp_started.await.context("HTTPS service failed to start")?;
        udp_started.await.context("H3 service failed to start")?;
        ensure!(running.is_live(), "transport failed during startup");
        Ok(running)
    }
    /// Install the LIVE_V1 relay on this endpoint (once). Only an installed
    /// relay lets the owner advertise `MEDIA_LIVE_V1`.
    pub fn install_live(&self, relay: qos_moq::live::LiveRelay) -> bool {
        self.live.set(relay).is_ok()
    }
    /// Whether `/moq/v21/live` is served on this endpoint.
    pub fn live_installed(&self) -> bool {
        self.is_live() && self.live.get().is_some()
    }
    /// Liveness only. The concrete owner separately qualifies installed handlers.
    pub fn is_live(&self) -> bool {
        !self.failed && !*self.stop.borrow() && self.registrations.load(Ordering::Acquire) == 3
    }
    pub fn origin(&self) -> &str {
        &self.origin
    }
    pub fn host_bytes(&self) -> crate::host_bytes::HostBytes {
        self.receiver.host_bytes()
    }
    pub fn host_work(&self) -> crate::store::HostWorkInventory {
        self.receiver.host_work()
    }
    pub async fn recv(&mut self) -> Option<BufferedRequest> {
        if !self.is_live() {
            return None;
        }
        tokio::select! {biased; result=self.tasks.join_next()=>{let _=result;self.failed=true;let _=self.stop.send(true);None},request=self.receiver.recv()=>request}
    }
    pub fn try_recv(&mut self) -> Option<BufferedRequest> {
        self.is_live().then(|| self.receiver.try_recv()).flatten()
    }
    pub async fn shutdown(mut self) -> Result<()> {
        let _ = self.stop.send(true);
        self.endpoint.close(0u32.into(), b"host shutdown");
        if timeout(Duration::from_secs(3), async {
            while self.tasks.join_next().await.is_some() {}
        })
        .await
        .is_err()
        {
            self.tasks.abort_all();
            while self.tasks.join_next().await.is_some() {}
        }
        self.endpoint.wait_idle().await;
        Ok(())
    }
}
impl Drop for RunningTransport {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.endpoint.close(0u32.into(), b"host stopped");
        self.tasks.abort_all();
    }
}
