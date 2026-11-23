//! The network capability on Linux, macOS and the web.
//!
//! One async API on every target, every future `Send`, so an app hands requests
//! to `tilo_rt::app()` instead of running a blocking client on a thread of its
//! own. What a target cannot provide is an [`Unsupported`] error at run time,
//! never a missing item.

pub mod address;
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
#[cfg(all(feature = "http", not(target_family = "wasm")))]
pub use client::{Lookup, refused_lookup};
pub use error::NetError;
pub use url::Url;

/// Whether `url` names this machine: `localhost`, any name under it
/// ([`loopback_name`]), or a loopback IPv4 or IPv6 address.
#[must_use]
pub fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(name)) => loopback_name(name),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

/// Whether `url` is on an origin browsers treat as secure: `https`, or plain
/// `http` on this machine, which never leaves it.
#[must_use]
pub fn secure_origin(url: &Url) -> bool {
    match url.scheme() {
        "https" => true,
        "http" => is_loopback_host(url),
        _ => false,
    }
}

/// Whether `url` is a credential-free endpoint on a [`secure_origin`]: `https`,
/// or plain `http` on this machine, with a host and neither username nor password.
#[must_use]
pub fn secure_endpoint(url: &Url) -> bool {
    secure_origin(url)
        && url.host().is_some()
        && url.username().is_empty()
        && url.password().is_none()
}

/// Whether `name` is a name of this machine: `localhost` and every name under it, which a
/// browser resolves to loopback itself. The local stack serves the site as `tilo.localhost`.
#[must_use]
pub fn loopback_name(name: &str) -> bool {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    name == "localhost" || name.ends_with(".localhost")
}

#[cfg(test)]
mod secure_origin_tests {
    use super::{Url, loopback_name, secure_endpoint, secure_origin};

    #[test]
    fn a_name_under_localhost_is_this_machine() {
        for name in ["localhost", "tilo.localhost", "a.b.localhost", "LOCALHOST."] {
            assert!(loopback_name(name), "{name}");
        }
        for name in [
            "localhost.tilo.sh",
            "tilo.sh",
            "notlocalhost",
            "localhost.example",
        ] {
            assert!(!loopback_name(name), "{name}");
        }
    }

    #[test]
    fn https_anywhere_and_http_only_on_this_machine() {
        for secure in [
            "https://tilo.sh",
            "https://eu1.tilo.sh:8443",
            "http://localhost:24110",
            "http://localhost./",
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
            "http://0.0.0.0/",
            "http://[::]/",
            "http://[::ffff:127.0.0.1]/",
            "ws://localhost",
            "ftp://localhost",
        ] {
            assert!(!secure_origin(&Url::parse(open).unwrap()), "{open}");
        }
    }

    #[test]
    fn secure_endpoints_allow_local_http_and_refuse_credentials() {
        for safe in [
            "https://tilo.sh",
            "https://eu1.tilo.sh:8443/entries",
            "http://localhost:24110/",
            "http://localhost./",
            "http://tilo.localhost:24120/",
            "http://127.0.0.1:24100/",
            "http://127.1.2.3:8080/",
            "http://[::1]:8080/",
        ] {
            assert!(secure_endpoint(&Url::parse(safe).unwrap()), "{safe}");
        }
        for refused in [
            "http://tilo.sh/",
            "http://localhost.tilo.sh/",
            "http://10.0.0.1/",
            "http://0.0.0.0/",
            "http://[::]/",
            "http://[::ffff:127.0.0.1]/",
            "https://placeholder:placeholder@tilo.sh/",
            "https://placeholder@tilo.sh/",
            "https://:placeholder@tilo.sh/",
            "http://placeholder:placeholder@localhost:8080/",
            "http://placeholder:placeholder@127.0.0.1:8080/",
            "http://placeholder:placeholder@[::1]:8080/",
            "file:///tmp/app.tapp",
            "ws://localhost/",
            "ftp://localhost/",
        ] {
            assert!(!secure_endpoint(&Url::parse(refused).unwrap()), "{refused}");
        }
    }
}

#[cfg(not(target_family = "wasm"))]
pub use native::tls::{
    client_config, client_config_also_trusting, client_config_with_roots, crypto_provider,
    install_crypto_provider, lacks_authorities, pinned_client_config,
    server_verifier_also_trusting, system_roots,
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
    /// One resumable, verified download from a trusted origin.
    #[cfg(feature = "blocking")]
    pub mod fetch;

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
        let builder = reqwest::Client::builder()
            .gzip(false)
            .brotli(false)
            .dns_resolver(LoopbackNames)
            .proxy(system_proxy_except_loopback());
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
            .brotli(false)
            .dns_resolver(std::sync::Arc::new(LoopbackNames))
            .proxy(system_proxy_except_loopback());
        #[cfg(target_os = "android")]
        let builder = builder.tls_certs_only(system_certificates());
        builder
    }

    /// The system/environment proxy for destinations off this machine, and
    /// direct connections for destinations [`crate::is_loopback_host`] counts
    /// as this machine (`localhost`, `*.localhost`, `127.0.0.0/8`, `::1`).
    fn system_proxy_except_loopback() -> reqwest::Proxy {
        let system = hyper_util::client::proxy::matcher::Matcher::from_system();
        reqwest::Proxy::custom(move |url| {
            if crate::is_loopback_host(url) {
                return None;
            }
            let uri = url.as_str().parse::<::http::Uri>().ok()?;
            let intercept = system.intercept(&uri)?;
            proxy_url(&intercept)
        })
    }

    fn proxy_url(intercept: &hyper_util::client::proxy::matcher::Intercept) -> Option<crate::Url> {
        use base64::Engine as _;

        let mut target = crate::Url::parse(&intercept.uri().to_string()).ok()?;
        if let Some((user, pass)) = intercept.raw_auth() {
            target.set_username(user).ok()?;
            target.set_password(Some(pass)).ok()?;
        } else if let Some(header) = intercept.basic_auth()
            && let Ok(text) = header.to_str()
            && let Some(encoded) = text.strip_prefix("Basic ")
            && let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded)
            && let Ok(credentials) = String::from_utf8(decoded)
        {
            let (user, pass) = credentials
                .split_once(':')
                .map_or((credentials.as_str(), None), |(user, pass)| {
                    (user, Some(pass))
                });
            target.set_username(user).ok()?;
            target.set_password(pass).ok()?;
        }
        Some(target)
    }

    /// [`blocking_client`] for a caller that keeps its requests off some
    /// addresses: for every name it dials only the addresses `lookup` answers,
    /// in the one lookup the connection itself makes
    /// ([`crate::ClientBuilder::lookup`]), and it takes no proxy from the
    /// environment, which would resolve a name behind that lookup. An address
    /// written into a URL is not looked up: its caller asks
    /// [`crate::address::asked_as_written`] before it sends anything there.
    #[cfg(feature = "blocking")]
    pub fn blocking_client_looking(lookup: crate::Lookup) -> reqwest::blocking::ClientBuilder {
        blocking_client()
            .dns_resolver(std::sync::Arc::new(crate::native::http::Looked(lookup)))
            .no_proxy()
    }

    /// What a client's own lookup said when it refused the name a request was
    /// for, read from that request's `error`: nothing was sent.
    #[must_use]
    pub fn refused_by_lookup(error: &(dyn std::error::Error + 'static)) -> Option<String> {
        crate::native::http::refused(error)
    }

    /// Resolution for Tilo's clients: a name under `localhost` is this machine's
    /// ([`crate::loopback_name`]), whoever the machine's resolver is, and every
    /// other name is the system resolver's, as reqwest's own is.
    struct LoopbackNames;

    impl reqwest::dns::Resolve for LoopbackNames {
        fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
            let host = name.as_str().to_owned();
            if crate::loopback_name(&host) {
                let addrs: Vec<std::net::SocketAddr> = vec![
                    std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0)),
                    std::net::SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 0)),
                ];
                let addrs: reqwest::dns::Addrs = Box::new(addrs.into_iter());
                return Box::pin(std::future::ready(Ok(addrs)));
            }
            Box::pin(async move {
                let addresses = tokio::task::spawn_blocking(move || {
                    use std::net::ToSocketAddrs as _;
                    (host.as_str(), 0).to_socket_addrs()
                })
                .await
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?;
                Ok(Box::new(addresses) as reqwest::dns::Addrs)
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use reqwest::dns::{Name, Resolve as _};

        #[test]
        #[allow(
            clippy::expect_used,
            reason = "a test's own name and runtime are fixtures"
        )]
        fn a_name_under_localhost_is_this_machine() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("a runtime");
            for host in [
                "localhost",
                "tilo.localhost",
                "a.b.localhost",
                "TILO.Localhost.",
            ] {
                let addrs: Vec<std::net::SocketAddr> = runtime
                    .block_on(super::LoopbackNames.resolve(host.parse::<Name>().expect("a name")))
                    .expect("loopback needs no resolver")
                    .collect();
                assert!(!addrs.is_empty(), "{host}");
                assert!(
                    addrs.iter().all(|address| address.ip().is_loopback()),
                    "{host}"
                );
            }
        }
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
