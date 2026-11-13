//! A packet relay on this machine's loopback, for a test whose endpoint needs a home relay.
//!
//! An iroh endpoint asks its relays where they see it from and takes the one that answers as its
//! home; a relay that answers nothing leaves the endpoint waiting out that probe. [`relay`] is the
//! relay such a test uses instead: one packet relay on `127.0.0.1`, answering on a port the
//! machine picked, without QUIC address discovery, so an endpoint takes it as home from the HTTP
//! probe alone. A test whose subject is the relay itself — its certificate, the keys it admits —
//! starts the same relay with those settings through [`Relay::with`].

use std::net::Ipv4Addr;

use iroh_relay::server::{RelayConfig, Server, ServerConfig};

/// A packet relay on this machine's loopback, closed when dropped.
pub struct Relay {
    server: Server,
    url: iroh::RelayUrl,
}

impl Relay {
    /// Start a relay of the caller's making on a free loopback port, inside the caller's runtime.
    ///
    /// # Panics
    ///
    /// If the relay cannot start, which is the failure of the test that needs it.
    #[allow(
        clippy::expect_used,
        reason = "a fixture that cannot start fails the test that needs it"
    )]
    pub async fn with(relay: RelayConfig) -> Self {
        let scheme = if relay.tls.is_some() { "https" } else { "http" };
        let mut config = ServerConfig::default();
        config.relay = Some(relay);
        let server = Server::spawn(config)
            .await
            .expect("a loopback relay starts");
        let addr = match scheme {
            "https" => server.https_addr().expect("the relay serves HTTPS"),
            _ => server.http_addr().expect("the relay serves HTTP"),
        };
        let url = format!("{scheme}://{addr}")
            .parse()
            .expect("a loopback relay URL");
        Self { server, url }
    }

    /// The running relay, for what it counts and answers.
    #[must_use]
    pub fn server(&self) -> &Server {
        &self.server
    }

    /// Where the relay is reached, as a relay setting names it.
    #[must_use]
    pub fn url(&self) -> &iroh::RelayUrl {
        &self.url
    }

    /// The relays an endpoint binds with to take this one as home: the map names it without QUIC
    /// address discovery, so the HTTP probe alone settles it.
    #[must_use]
    pub fn relays(&self) -> iroh::RelayMap {
        let mut relay = iroh::RelayConfig::from(self.url.clone());
        relay.quic = None;
        std::iter::once(relay).collect()
    }
}

/// The relay this test's endpoints take as home: a plain one on a free loopback port.
///
/// # Panics
///
/// If the relay cannot start, which is the failure of the test that needs it.
pub async fn relay() -> Relay {
    Relay::with(RelayConfig::new((Ipv4Addr::LOCALHOST, 0))).await
}
