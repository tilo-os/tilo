//! A host provisioned without a certificate: before the relay can start, the
//! browser listener serves the one route that installs it.
//!
//! The host's device key exists already (its public half was handed out of
//! band to whoever holds the person's root). The root's holder — a page that
//! holds the root — signs a `DeviceCert` for it and posts it to
//! `/peer/v1/install` with the install token the host was provisioned with.
//! The certificate must be for this key, verify, let this device host and
//! not admit devices; it is then written as a joined device's
//! ([`tilo_user_identity::install_device_cert`]) and [`await_certificate`]
//! returns, so the caller starts the relay on the same address.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use hyper::{Method, Request, Response, StatusCode, header};
use tilo_relay_protocol::web::{
    API_PREFIX, INSTALL_ROUTE, INSTALL_TOKEN_HEADER, Installed, MAX_REQUEST_BYTES,
};
use tilo_user_identity::{DeviceCert, IdentityPaths};
use tokio::sync::mpsc;

use super::{Body, WebConfig, body, reply, status, tls};
use crate::daemon::RelayError;

struct Waiting {
    paths: IdentityPaths,
    token: Vec<u8>,
    allowed_origins: Vec<String>,
    installed: mpsc::Sender<DeviceCert>,
}

/// The listener of a host awaiting its certificate. Dropping it stops
/// serving.
pub struct InstallListener {
    /// HTTPS address.
    pub addr: SocketAddr,
    /// Plain loopback HTTP address, if any.
    pub plain_addr: Option<SocketAddr>,
    done: mpsc::Receiver<DeviceCert>,
    tasks: tokio::task::JoinSet<()>,
}

impl InstallListener {
    /// Serve `/install` on `config`'s addresses for the device key under
    /// `paths`, accepting `token`.
    ///
    /// # Errors
    ///
    /// Returns [`RelayError::Iroh`] (the transport error kind) for a token
    /// shorter than 16 characters, or when the certificate cannot be loaded
    /// or an address cannot be bound.
    pub fn bind(config: &WebConfig, paths: IdentityPaths, token: &str) -> Result<Self, RelayError> {
        let failed = |message: String| RelayError::Iroh(format!("install listener: {message}"));
        if token.trim().len() < 16 {
            return Err(failed(
                "the install token must have at least 16 characters".into(),
            ));
        }
        if config
            .plain_listen
            .is_some_and(|plain| !plain.ip().is_loopback())
        {
            return Err(failed(
                "the plain HTTP listener must be a loopback address".into(),
            ));
        }
        let identity = tls::Identity::load(&config.tls).map_err(failed)?;
        let acceptor =
            tokio_rustls::TlsAcceptor::from(Arc::new(identity.https_config().map_err(failed)?));
        let tcp = bind(config.listen).map_err(failed)?;
        let addr = tcp
            .local_addr()
            .map_err(|error| failed(error.to_string()))?;
        let (installed, done) = mpsc::channel(1);
        let waiting = Arc::new(Waiting {
            paths,
            token: token.trim().as_bytes().to_vec(),
            allowed_origins: config.allowed_origins.clone(),
            installed,
        });
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(accept_tls(tcp, acceptor, waiting.clone()));
        let mut plain_addr = None;
        if let Some(plain) = config.plain_listen {
            let plain = bind(plain).map_err(failed)?;
            plain_addr = Some(
                plain
                    .local_addr()
                    .map_err(|error| failed(error.to_string()))?,
            );
            tasks.spawn(accept_plain(plain, waiting));
        }
        Ok(Self {
            addr,
            plain_addr,
            done,
            tasks,
        })
    }

    /// Wait for the certificate, then stop serving.
    ///
    /// # Errors
    ///
    /// Returns [`RelayError::Iroh`] when the listener stopped first.
    pub async fn installed(mut self) -> Result<DeviceCert, RelayError> {
        let cert = self
            .done
            .recv()
            .await
            .ok_or_else(|| RelayError::Iroh("install listener: stopped".into()))?;
        self.tasks.shutdown().await;
        Ok(cert)
    }
}

/// Serve `/install` on `config`'s addresses until a certificate for the
/// device key under `paths` is installed with `token`; return it.
///
/// # Errors
///
/// As [`InstallListener::bind`] and [`InstallListener::installed`].
pub async fn await_certificate(
    config: &WebConfig,
    paths: IdentityPaths,
    token: &str,
) -> Result<DeviceCert, RelayError> {
    InstallListener::bind(config, paths, token)?
        .installed()
        .await
}

fn bind(listen: SocketAddr) -> Result<tokio::net::TcpListener, String> {
    let socket = if listen.is_ipv4() {
        tokio::net::TcpSocket::new_v4()
    } else {
        tokio::net::TcpSocket::new_v6()
    }
    .map_err(|error| error.to_string())?;
    // The relay binds this address again once the certificate is in.
    socket
        .set_reuseaddr(true)
        .map_err(|error| error.to_string())?;
    socket.bind(listen).map_err(|error| error.to_string())?;
    socket.listen(64).map_err(|error| error.to_string())
}

async fn accept_tls(
    tcp: tokio::net::TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
    waiting: Arc<Waiting>,
) {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let Ok((stream, _)) = tcp.accept().await else {
            continue;
        };
        while connections.try_join_next().is_some() {}
        let acceptor = acceptor.clone();
        let waiting = waiting.clone();
        connections.spawn(async move {
            if let Ok(Ok(tls)) =
                tokio::time::timeout(Duration::from_secs(10), acceptor.accept(stream)).await
            {
                serve(tls, waiting).await;
            }
        });
    }
}

async fn accept_plain(tcp: tokio::net::TcpListener, waiting: Arc<Waiting>) {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let Ok((stream, _)) = tcp.accept().await else {
            continue;
        };
        while connections.try_join_next().is_some() {}
        connections.spawn(serve(stream, waiting.clone()));
    }
}

async fn serve<Io>(io: Io, waiting: Arc<Waiting>)
where
    Io: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let service = hyper::service::service_fn(move |request| {
        let waiting = waiting.clone();
        async move { Ok::<_, Infallible>(handle(&waiting, request).await) }
    });
    let _ = hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
        .serve_connection(hyper_util::rt::TokioIo::new(io), service)
        .await;
}

async fn handle(waiting: &Waiting, request: Request<hyper::body::Incoming>) -> Response<Body> {
    let origin = request.headers().get(header::ORIGIN).cloned();
    let mut response = if request.method() == Method::OPTIONS {
        reply(StatusCode::NO_CONTENT, Vec::new())
    } else if request.method() != Method::POST {
        status(StatusCode::METHOD_NOT_ALLOWED, "POST only")
    } else if request.uri().path().strip_prefix(API_PREFIX) != Some(INSTALL_ROUTE) {
        status(
            StatusCode::SERVICE_UNAVAILABLE,
            "this host is waiting for its certificate",
        )
    } else {
        install(waiting, request)
            .await
            .unwrap_or_else(|response| response)
    };
    super::cors(
        &waiting.allowed_origins,
        &format!("content-type, {INSTALL_TOKEN_HEADER}"),
        origin.as_ref(),
        &mut response,
    );
    response
}

/// Whether `presented` is the token, compared in constant time.
fn token_matches(token: &[u8], presented: &[u8]) -> bool {
    token.len() == presented.len()
        && token
            .iter()
            .zip(presented)
            .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

async fn install(
    waiting: &Waiting,
    request: Request<hyper::body::Incoming>,
) -> Result<Response<Body>, Response<Body>> {
    let presented = request
        .headers()
        .get(INSTALL_TOKEN_HEADER)
        .map(|value| value.as_bytes().to_vec())
        .unwrap_or_default();
    if !token_matches(&waiting.token, &presented) {
        return Err(status(StatusCode::FORBIDDEN, "wrong install token"));
    }
    let (_, bytes) = body(request, MAX_REQUEST_BYTES).await?;
    let cert: DeviceCert = serde_json::from_slice(&bytes)
        .map_err(|_| status(StatusCode::BAD_REQUEST, "expected a DeviceCert as JSON"))?;
    if !cert.capabilities.can_act_as_host {
        return Err(status(
            StatusCode::BAD_REQUEST,
            "the certificate does not let this device host",
        ));
    }
    let paths = waiting.paths.clone();
    let written = cert.clone();
    tokio::task::spawn_blocking(move || {
        tilo_user_identity::install_device_cert(&paths, &written, tilo_clock::unix_now_ns())
    })
    .await
    .map_err(|_| status(StatusCode::INTERNAL_SERVER_ERROR, "install failed"))?
    .map_err(|error| status(StatusCode::BAD_REQUEST, &error.to_string()))?;
    let answer = Installed {
        device_public_key: hex::encode(cert.device_public_key),
        user_public_key: hex::encode(cert.user_public_key),
    };
    let _ = waiting.installed.try_send(cert);
    let mut response = reply(
        StatusCode::OK,
        serde_json::to_vec(&answer).unwrap_or_default(),
    );
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    Ok(response)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in tests these are assertions"
)]
mod tests {
    use super::*;
    use crate::web::TlsSource;
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::{DeviceCapabilities, RevocationList, sign_device_cert};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    const TOKEN: &str = "an-install-token-0123";

    #[test]
    fn the_token_must_match_exactly() {
        assert!(token_matches(b"0123456789abcdef", b"0123456789abcdef"));
        assert!(!token_matches(b"0123456789abcdef", b"0123456789abcdeF"));
        assert!(!token_matches(b"0123456789abcdef", b"0123456789abcde"));
        assert!(!token_matches(b"0123456789abcdef", b""));
    }

    async fn request(
        addr: SocketAddr,
        method: &str,
        path: &str,
        headers: &str,
        body: &str,
    ) -> String {
        let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(request.as_bytes()).await.unwrap();
        let mut reply = Vec::new();
        socket.read_to_end(&mut reply).await.unwrap();
        String::from_utf8_lossy(&reply).into_owned()
    }

    fn cert(root: &SigningKey, device: [u8; 32], capabilities: DeviceCapabilities) -> String {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device,
            device_label: "node".into(),
            capabilities,
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        serde_json::to_string(&cert).unwrap()
    }

    /// A host without a certificate takes one only with its token, only for
    /// its own key and only one that lets it host; then it starts serving.
    #[tokio::test]
    async fn a_waiting_host_installs_its_certificate_and_starts() {
        let dir = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(dir.path().join("identity"));
        let (device, _) = tilo_user_identity::load_or_create_device_key(&paths).unwrap();
        let own = device.verifying_key().to_bytes();
        let root = SigningKey::from_bytes(&[7; 32]);
        let config = WebConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            tls: TlsSource::Development {
                hosts: vec!["localhost".into()],
                store: Some(dir.path().join("tls")),
            },
            allowed_origins: vec!["https://console.example".into()],
            plain_listen: Some("127.0.0.1:0".parse().unwrap()),
        };
        assert!(InstallListener::bind(&config, paths.clone(), "short").is_err());
        let listener =
            InstallListener::bind(&config, paths.clone(), &format!("{TOKEN}\n")).unwrap();
        let addr = listener.plain_addr.unwrap();
        let token = format!("{INSTALL_TOKEN_HEADER}: {TOKEN}\r\n");
        let node = tilo_user_identity::cloud_enrollment::host_capabilities;

        let reply = request(addr, "POST", "/peer/v1/host", "", "").await;
        assert!(reply.starts_with("HTTP/1.1 503"), "{reply}");
        let preflight = request(
            addr,
            "OPTIONS",
            "/peer/v1/install",
            "Origin: https://console.example\r\n",
            "",
        )
        .await;
        assert!(preflight.contains(INSTALL_TOKEN_HEADER), "{preflight}");
        let good = cert(&root, own, node());
        let reply = request(addr, "POST", "/peer/v1/install", "", &good).await;
        assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
        let reply = request(
            addr,
            "POST",
            "/peer/v1/install",
            &format!("{INSTALL_TOKEN_HEADER}: not-the-install-token\r\n"),
            &good,
        )
        .await;
        assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
        for refused in [
            cert(&root, [9; 32], node()),
            cert(&root, own, DeviceCapabilities::member()),
            cert(&root, own, DeviceCapabilities::primary()),
            "{}".into(),
        ] {
            let reply = request(addr, "POST", "/peer/v1/install", &token, &refused).await;
            assert!(reply.starts_with("HTTP/1.1 400"), "{reply}");
        }
        assert!(!paths.device_cert.exists());

        let reply = request(addr, "POST", "/peer/v1/install", &token, &good).await;
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert!(reply.contains(&hex::encode(own)), "{reply}");
        let installed = listener.installed().await.unwrap();
        assert_eq!(installed.device_public_key, own);
        assert!(
            tokio::net::TcpStream::connect(addr).await.is_err(),
            "stopped serving"
        );

        let relay = crate::daemon::Relay::start(
            crate::RelayConfig {
                state_root: dir.path().join("state"),
                identity_root: dir.path().join("identity"),
                ..Default::default()
            },
            crate::daemon::RelayIdentity {
                user_public_key: tilo_user_identity::persist::read_public_key(&paths.user_pubkey)
                    .unwrap(),
                device_signing_key: device,
                device_cert: tilo_user_identity::persist::read_device_cert(&paths.device_cert)
                    .unwrap(),
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();
        relay.shutdown().await;
    }
}
