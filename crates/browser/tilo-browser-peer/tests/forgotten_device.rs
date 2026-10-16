//! A host that forgets a guest: a page's session tells reachability from a
//! refusal by the host's own reason. A proxy's error page in front of the
//! host never counts against the device, however often it answers; a host
//! whose state was reset (its identity and address kept) refuses the guest
//! with `NotAdmitted` on the first handshake, and the session stops with
//! `not-admitted` for the page to join again.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the test holds its host for the length of a step"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tilo_browser_peer::{Host, HttpClient, HttpFuture, PeerSnapshot, TransportPhase};
use tilo_relay_protocol::web::SessionRefusal;
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{enrol, start_host_admitting, start_peer, wait_for};

/// The host behind a proxy that answers the first `error_pages` handshakes
/// itself with an HTML error page; counts the host's typed refusals.
struct Proxy {
    inner: Arc<support::https::DevHttps>,
    error_pages: AtomicUsize,
    refusals: AtomicUsize,
}

impl HttpClient for Proxy {
    fn post(&self, path: &str, session: Option<&str>, body: Vec<u8>) -> HttpFuture<'_> {
        let path = path.to_owned();
        let session = session.map(str::to_owned);
        Box::pin(async move {
            let handshake = path.ends_with("/session") || path.ends_with("/session/finish");
            if handshake
                && self
                    .error_pages
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                        left.checked_sub(1)
                    })
                    .is_ok()
            {
                return Ok((403, b"<html><body>403 Forbidden</body></html>".to_vec()));
            }
            let (status, reply) = self.inner.post(&path, session.as_deref(), body).await?;
            if handshake && status == 403 && postcard::from_bytes::<SessionRefusal>(&reply).is_ok()
            {
                self.refusals.fetch_add(1, Ordering::Relaxed);
            }
            Ok((status, reply))
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reset_host_refuses_its_forgotten_guest_at_once_and_a_proxy_never_counts() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let (guest_root, _) = UserIdentity::generate("Guest", 1);
    let guest = enrol(&guest_root, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host_admitting(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
        None,
        &[guest.cert.user_public_key],
    )
    .await;
    let listen = host.web.addr;
    // More proxy pages than the refusals an unexplained refusal takes.
    let proxy = Arc::new(Proxy {
        inner: host.https(),
        error_pages: AtomicUsize::new(4),
        refusals: AtomicUsize::new(0),
    });
    let peer = start_peer(
        Host::Https(proxy.clone()),
        guest.cert.clone(),
        host_device.cert.clone(),
        guest.key.clone(),
    );
    wait_for("the guest to get past the proxy and in", || {
        let status = peer.status();
        async move { (status.phase == TransportPhase::Connected).then_some(()) }
    })
    .await;
    assert_eq!(proxy.error_pages.load(Ordering::Relaxed), 0);
    assert_eq!(proxy.refusals.load(Ordering::Relaxed), 0);

    // The host's state is reset: the same identity, address and web
    // certificate, and nothing of the guest.
    drop(host.web);
    host.relay.shutdown().await;
    let reset = tempfile::tempdir().unwrap();
    std::fs::copy(
        state.path().join("web-dev-identity"),
        reset.path().join("web-dev-identity"),
    )
    .unwrap();
    let host = start_host_admitting(&root, &host_device, reset.path(), listen, None, &[]).await;
    let failed = wait_for("the reset host to refuse the guest", || {
        let snapshot = PeerSnapshot::from_status(&peer.status());
        async move { (snapshot.phase == "failed").then_some(snapshot) }
    })
    .await;
    assert_eq!(failed.error_code, Some("not-admitted"));
    assert_eq!(
        proxy.refusals.load(Ordering::Relaxed),
        1,
        "the host's reason decides on its first refusal"
    );
    peer.close();
    host.relay.shutdown().await;
}
