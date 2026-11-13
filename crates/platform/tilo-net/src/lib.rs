//! The network capability on Linux, macOS and the web.
//!
//! One async API on every target, every future `Send`, so an app hands requests
//! to `tilo_rt::app()` instead of running a blocking client on a thread of its
//! own. What a target cannot provide is an [`Unsupported`] error at run time,
//! never a missing item.

mod caps;
#[cfg(feature = "http")]
mod client;
mod error;
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

/// Whether `url` is on an origin browsers treat as secure: `https`, or plain
/// `http` on this machine, which never leaves it.
#[must_use]
pub fn secure_origin(url: &Url) -> bool {
    match url.scheme() {
        "https" => true,
        "http" => match url.host() {
            Some(url::Host::Domain(name)) => {
                let name = name.trim_end_matches('.').to_ascii_lowercase();
                name == "localhost" || name.ends_with(".localhost")
            }
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            None => false,
        },
        _ => false,
    }
}

#[cfg(test)]
mod secure_origin_tests {
    use super::{Url, secure_origin};

    #[test]
    fn https_anywhere_and_http_only_on_this_machine() {
        for secure in [
            "https://tilo.sh",
            "https://eu1.tilo.sh:8443",
            "http://localhost:24110",
            "http://tilo.localhost:24120",
            "http://127.0.0.1:24100",
            "http://127.1.2.3",
            "http://[::1]:80",
        ] {
            assert!(secure_origin(&Url::parse(secure).unwrap()), "{secure}");
        }
        for open in [
            "http://tilo.sh",
            "http://localhost.tilo.sh",
            "http://10.0.0.1",
            "ws://localhost",
            "ftp://localhost",
        ] {
            assert!(!secure_origin(&Url::parse(open).unwrap()), "{open}");
        }
    }
}

#[cfg(not(target_family = "wasm"))]
pub use native::tls::{
    client_config, client_config_also_trusting, client_config_with_roots, crypto_provider,
    install_crypto_provider, pinned_client_config, server_verifier_also_trusting, system_roots,
};

/// Every future of the public API is `Send` on every target; this fails to
/// compile otherwise.
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
}

/// reqwest builders on Tilo's TLS defaults, for desktop-only code. They panic
/// unless a process default provider is installed, so every client starts
/// from these functions.
#[cfg(all(feature = "http", not(target_family = "wasm")))]
pub mod http {
    /// The one blocking client a caller keeps for its requests: its own type, so
    /// a caller need not name `reqwest`.
    #[cfg(feature = "blocking")]
    pub type BlockingClient = reqwest::blocking::Client;

    #[allow(
        clippy::disallowed_methods,
        reason = "this is the sanctioned constructor; it installs the provider first"
    )]
    pub fn client() -> reqwest::ClientBuilder {
        crate::install_crypto_provider();
        let builder = reqwest::Client::builder().gzip(false).brotli(false);
        #[cfg(target_os = "android")]
        let builder = builder.tls_certs_only(system_certificates());
        builder
    }

    #[cfg(feature = "blocking")]
    #[allow(
        clippy::disallowed_methods,
        reason = "this is the sanctioned constructor; it installs the provider first"
    )]
    pub fn blocking_client() -> reqwest::blocking::ClientBuilder {
        crate::install_crypto_provider();
        let builder = reqwest::blocking::Client::builder()
            .gzip(false)
            .brotli(false);
        #[cfg(target_os = "android")]
        let builder = builder.tls_certs_only(system_certificates());
        builder
    }

    /// Android's platform verifier aborts without the app's JVM context, so
    /// clients there verify against Android's system store.
    #[cfg(target_os = "android")]
    fn system_certificates() -> Vec<reqwest::Certificate> {
        rustls_native_certs::load_native_certs()
            .certs
            .iter()
            .filter_map(|der| reqwest::Certificate::from_der(der).ok())
            .collect()
    }
}
