//! The network capability on Linux, macOS and the web.
//!
//! One API on every target, all of it async and every future `Send`, so an
//! app hands requests to `tilo_rt::app()` (or the toolkit's `LatestRequest`)
//! and never runs a blocking client on a thread of its own:
//!
//! - [`Client`] sends HTTP [`Request`]s and answers a [`Response`] whose body
//!   streams in [`Response::chunk`]s. reqwest on the desktop; `fetch` in a
//!   page, where the browser's cross-origin rules (CORS) apply. A host may
//!   provide a same-origin [`proxy`] that relays cross-origin requests for
//!   apps that opt in.
//! - [`ws`]: WebSocket clients. tokio-websockets on the desktop, the page's
//!   `WebSocket` on the web.
//! - [`webtransport`]: WebTransport sessions with bidirectional and
//!   unidirectional streams and datagrams. web-transport-quinn on the
//!   desktop, the browser's `WebTransport` where it has one.
//! - [`status`]: whether the device is online, and whether the connection is
//!   metered.
//! - [`sockets`]: raw TCP, UDP and TLS over TCP. Desktop only; every call
//!   answers [`Unsupported`] in a page.
//! - [`p2p`]: peer-to-peer frame streams between endpoints that dial each
//!   other by ticket. iroh QUIC on the desktop; [`Unsupported`] in a page.
//!
//! What a target cannot provide is an [`Unsupported`] error at run time and an
//! entry of its [`Feature`]'s targets, never a missing item.
//!
//! # TLS
//!
//! Tilo has one TLS backend (`ring`) and one trust source: the immutable
//! system bundle read by `tilo-native-certs`. Building those per crate is how
//! apps drifted onto bundled Mozilla roots, so desktop clients take them from
//! here ([`client_config`], [`system_roots`]). The reqwest builders in
//! [`http`](mod@http) remain for desktop-only code that has not moved to
//! [`Client`].

mod caps;
#[cfg(feature = "http")]
mod client;
mod error;
#[cfg(feature = "p2p")]
pub mod p2p;
pub mod proxy;
#[cfg(feature = "sockets")]
pub mod sockets;
pub mod status;
#[cfg(feature = "webtransport")]
pub mod webtransport;
#[cfg(feature = "ws")]
pub mod ws;

#[cfg(not(target_family = "wasm"))]
mod native;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod web;
#[cfg(all(
    target_family = "wasm",
    not(all(target_arch = "wasm32", target_os = "unknown"))
))]
compile_error!("tilo-net has backends for native targets and wasm32-unknown-unknown only");

pub use ::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
pub use bytes::Bytes;
pub use caps::{Availability, Feature, Unsupported, availability, available};
#[cfg(feature = "http")]
pub use client::{Client, ClientBuilder, Redirects, Request, RequestBuilder, Response};
pub use error::NetError;
pub use url::Url;

#[cfg(not(target_family = "wasm"))]
pub use native::tls::{
    client_config, client_config_with_roots, crypto_provider, install_crypto_provider,
    pinned_client_config, system_roots,
};

/// Every future of the public API is `Send` on every target, so apps can
/// hand it to `tilo_rt::app()`. This fails to compile otherwise.
#[allow(dead_code, reason = "a compile-time check, never called")]
mod send_check {
    fn send<T: Send>(_: &T) {}

    #[cfg(feature = "http")]
    fn http(client: &crate::Client, response: crate::Response) {
        send(&client.get("https://example.com/").send());
        send(&response.bytes());
    }

    #[cfg(feature = "ws")]
    fn ws(socket: crate::ws::WebSocket) {
        send(&socket.close());
    }

    #[cfg(feature = "webtransport")]
    fn webtransport(stream: crate::webtransport::RecvStream) {
        send(&stream.read_to_end(1));
    }

    fn status(watch: &mut crate::status::Watch) {
        send(&watch.changed());
    }

    #[cfg(feature = "p2p")]
    fn p2p() {
        send(&crate::p2p::Endpoint::bind(b"", 1, 1, |_| Some(())));
    }
}

/// reqwest builders on Tilo's TLS defaults, for desktop-only code.
///
/// reqwest is built without a bundled crypto provider and verifies through
/// `rustls-platform-verifier`, which on Linux reads the same system bundle as
/// [`system_roots`]. Its builders panic unless a process default provider is
/// installed, so every client starts from these functions.
///
/// Portable code uses [`Client`] instead: these exist only on the desktop,
/// and blocking clients cannot run in a page. Unlike [`Client`] they do not
/// decompress bodies, as before.
#[cfg(all(feature = "http", not(target_family = "wasm")))]
pub mod http {
    /// An async client builder with the crypto provider installed.
    #[allow(
        clippy::disallowed_methods,
        reason = "this is the sanctioned constructor; it installs the provider first"
    )]
    pub fn client() -> reqwest::ClientBuilder {
        crate::install_crypto_provider();
        reqwest::Client::builder().gzip(false).brotli(false)
    }

    /// A blocking client builder with the crypto provider installed.
    #[cfg(feature = "blocking")]
    #[allow(
        clippy::disallowed_methods,
        reason = "this is the sanctioned constructor; it installs the provider first"
    )]
    pub fn blocking_client() -> reqwest::blocking::ClientBuilder {
        crate::install_crypto_provider();
        reqwest::blocking::Client::builder()
            .gzip(false)
            .brotli(false)
    }
}
