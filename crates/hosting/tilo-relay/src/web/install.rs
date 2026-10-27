//! A host provisioned without a certificate: before the relay can start, the
//! browser listener serves the one route that installs it.
//!
//! The host's device key exists already (its public half was handed out of
//! band to whoever holds the person's root). The root's holder — a page that
//! holds the root — signs a `DeviceCert` for it and posts it to
//! `/peer/v1/install` (as JSON or its canonical encoding) with the install
//! token the host was provisioned with.
//! The certificate must be for this key, verify, let this device host and
//! not admit devices; it is then written as a joined device's
//! ([`tilo_user_identity::install_device_cert`]) and [`await_certificate`]
//! returns, so the caller starts the relay on the same address.
//!
//! A node runs a session beside it ([`SessionDevice`]): the shell that runs
//! the person's apps and agents, with a device key of its own. `GET` on the
//! route names that key, signed by this device's key, and the page certifies
//! it with the node's, as one of the person's devices: never a host, never
//! one that admits devices. Its certificate is written to the session's
//! file for the session to join with.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::Signer as _;
use hyper::{Method, Request, Response, StatusCode, header};
use tilo_relay_protocol::web::{
    API_PREFIX, INSTALL_ROUTE, INSTALL_TOKEN_HEADER, InstallCertificates, InstallInfo, Installed,
    MAX_REQUEST_BYTES, NodeSession, node_session_message,
};
use tilo_user_identity::{DeviceCert, IdentityPaths};
use tokio::sync::{mpsc, watch};

use super::{Body, WebConfig, body, reply, status, tls};
use crate::daemon::RelayError;

/// The session beside a node, certified at install with it.
#[derive(Clone, Debug)]
pub struct SessionDevice {
    pub public_key: [u8; 32],
    /// Where its certificate is written, in its canonical encoding.
    pub cert_out: std::path::PathBuf,
    /// Where the person's user key is written beside it, when the session's
    /// identity tree is named: the session then starts as an enrolled device
    /// of that person, not as a new person of its own. Only public halves
    /// are written; the session's private key is never read.
    pub user_key_out: Option<std::path::PathBuf>,
}

struct Waiting {
    paths: IdentityPaths,
    /// This device's key.
    device: [u8; 32],
    /// The session beside it, with this device's signature vouching for it.
    session: Option<(SessionDevice, [u8; 64])>,
    token: Vec<u8>,
    allowed_origins: Vec<String>,
    installed: mpsc::Sender<DeviceCert>,
    /// Turns true once the certificate is in: stop accepting, finish what
    /// is in flight.
    closing: watch::Receiver<bool>,
}

/// How long the requests in flight, the install's own answer among them,
/// may take to finish once the certificate is in.
const DRAIN: Duration = Duration::from_secs(5);

/// The listener of a host awaiting its certificate. Dropping it stops
/// serving.
pub struct InstallListener {
    pub addr: SocketAddr,
    /// Plain loopback HTTP address, if any.
    pub plain_addr: Option<SocketAddr>,
    done: mpsc::Receiver<DeviceCert>,
    close: watch::Sender<bool>,
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
        Self::bind_with_session(config, paths, token, None)
    }

    /// [`Self::bind`] for a node with `session` beside it.
    ///
    /// # Errors
    ///
    /// As [`Self::bind`], and when the device key cannot be read.
    pub fn bind_with_session(
        config: &WebConfig,
        paths: IdentityPaths,
        token: &str,
        session: Option<SessionDevice>,
    ) -> Result<Self, RelayError> {
        let failed = |message: String| RelayError::Iroh(format!("install listener: {message}"));
        let key = tilo_user_identity::persist::read_device_key(&paths.device_privkey)
            .map_err(|error| failed(format!("device key: {error}")))?;
        let device = key.verifying_key().to_bytes();
        let session = session.map(|session| {
            let signature = key
                .sign(&node_session_message(&device, &session.public_key))
                .to_bytes();
            (session, signature)
        });
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
        let (close, closing) = watch::channel(false);
        let waiting = Arc::new(Waiting {
            paths,
            device,
            session,
            token: token.trim().as_bytes().to_vec(),
            allowed_origins: config.allowed_origins.clone(),
            installed,
            closing,
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
            close,
            tasks,
        })
    }

    /// Wait for the certificate, then stop serving: stop accepting, and
    /// let the requests in flight finish (the installing page reads its
    /// answer) before the address is free for the relay.
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
        let _ = self.close.send(true);
        let drained = tokio::time::timeout(DRAIN, async {
            while self.tasks.join_next().await.is_some() {}
        })
        .await;
        if drained.is_err() {
            self.tasks.shutdown().await;
        }
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
    session: Option<SessionDevice>,
) -> Result<DeviceCert, RelayError> {
    InstallListener::bind_with_session(config, paths, token, session)?
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

/// The next connection, or `None` once the listener is closing.
async fn next(
    tcp: &tokio::net::TcpListener,
    closing: &mut watch::Receiver<bool>,
) -> Option<std::io::Result<(tokio::net::TcpStream, SocketAddr)>> {
    tokio::select! {
        accepted = tcp.accept() => Some(accepted),
        _ = closing.wait_for(|closing| *closing) => None,
    }
}

async fn accept_tls(
    tcp: tokio::net::TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
    waiting: Arc<Waiting>,
) {
    let mut closing = waiting.closing.clone();
    let mut connections = tokio::task::JoinSet::new();
    while let Some(accepted) = next(&tcp, &mut closing).await {
        let Ok((stream, _)) = accepted else {
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
    drop(tcp);
    while connections.join_next().await.is_some() {}
}

async fn accept_plain(tcp: tokio::net::TcpListener, waiting: Arc<Waiting>) {
    let mut closing = waiting.closing.clone();
    let mut connections = tokio::task::JoinSet::new();
    while let Some(accepted) = next(&tcp, &mut closing).await {
        let Ok((stream, _)) = accepted else {
            continue;
        };
        while connections.try_join_next().is_some() {}
        connections.spawn(serve(stream, waiting.clone()));
    }
    drop(tcp);
    while connections.join_next().await.is_some() {}
}

async fn serve<Io>(io: Io, waiting: Arc<Waiting>)
where
    Io: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let mut closing = waiting.closing.clone();
    let service = hyper::service::service_fn(move |request| {
        let waiting = waiting.clone();
        async move { Ok::<_, Infallible>(handle(&waiting, request).await) }
    });
    let builder =
        hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
    let connection = builder.serve_connection(hyper_util::rt::TokioIo::new(io), service);
    tokio::pin!(connection);
    tokio::select! {
        _ = connection.as_mut() => return,
        _ = closing.wait_for(|closing| *closing) => {}
    }
    // Answer what was asked on this connection, then close it.
    connection.as_mut().graceful_shutdown();
    let _ = connection.await;
}

async fn handle(waiting: &Waiting, request: Request<hyper::body::Incoming>) -> Response<Body> {
    let origin = request.headers().get(header::ORIGIN).cloned();
    let mut response = if request.method() == Method::OPTIONS {
        reply(StatusCode::NO_CONTENT, Vec::new())
    } else if request.uri().path().strip_prefix(API_PREFIX) != Some(INSTALL_ROUTE) {
        status(
            StatusCode::SERVICE_UNAVAILABLE,
            "this host is waiting for its certificate",
        )
    } else if request.method() == Method::GET {
        info(waiting, &request)
    } else if request.method() != Method::POST {
        status(StatusCode::METHOD_NOT_ALLOWED, "GET or POST only")
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

/// The host's certificate in an install body, and the session's if any.
fn parse_certificates(bytes: &[u8]) -> Option<(DeviceCert, Option<DeviceCert>)> {
    use base64::Engine as _;
    if bytes.first() != Some(&b'{') {
        return tilo_user_identity::canonical::from_canonical_bytes(bytes)
            .ok()
            .map(|cert| (cert, None));
    }
    if let Ok(cert) = serde_json::from_slice::<DeviceCert>(bytes) {
        return Some((cert, None));
    }
    let both: InstallCertificates = serde_json::from_slice(bytes).ok()?;
    let decode = |text: &str| {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(text.trim())
            .ok()?;
        tilo_user_identity::canonical::from_canonical_bytes::<DeviceCert>(&bytes).ok()
    };
    let session = match both.session_certificate.as_deref() {
        Some(text) => Some(decode(text)?),
        None => None,
    };
    Some((decode(&both.certificate)?, session))
}

fn authorized<B>(waiting: &Waiting, request: &Request<B>) -> bool {
    let presented = request
        .headers()
        .get(INSTALL_TOKEN_HEADER)
        .map(|value| value.as_bytes().to_vec())
        .unwrap_or_default();
    token_matches(&waiting.token, &presented)
}

fn wrong_token() -> Response<Body> {
    status(StatusCode::FORBIDDEN, "wrong install token")
}

fn json(value: &impl serde::Serialize) -> Response<Body> {
    let mut response = reply(
        StatusCode::OK,
        serde_json::to_vec(value).unwrap_or_default(),
    );
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    response
}

/// `GET`: this device's key and the session's, vouched for.
fn info<B>(waiting: &Waiting, request: &Request<B>) -> Response<Body> {
    if !authorized(waiting, request) {
        return wrong_token();
    }
    json(&InstallInfo {
        device_public_key: hex::encode(waiting.device),
        session: waiting
            .session
            .as_ref()
            .map(|(session, signature)| NodeSession {
                device_public_key: hex::encode(session.public_key),
                signature: hex::encode(signature),
            }),
    })
}

/// The session's certificate in `offered`, when it may run beside the node
/// `node` certifies: for the session's key, by the same root, verifying
/// now, and neither a host nor a device that admits others.
fn session_certificate(
    waiting: &Waiting,
    node: &DeviceCert,
    offered: Option<DeviceCert>,
) -> Result<Option<(DeviceCert, SessionDevice)>, String> {
    let (Some((session, _)), Some(cert)) = (&waiting.session, offered) else {
        return Ok(None);
    };
    if cert.device_public_key != session.public_key {
        return Err("the session certificate names another device".into());
    }
    if cert.user_public_key != node.user_public_key {
        return Err("the session certificate is from another root".into());
    }
    if cert.capabilities.can_act_as_host || cert.capabilities.can_admit_devices {
        return Err("the session certificate must not host or admit devices".into());
    }
    tilo_user_identity::verify_device_cert_at(&cert, tilo_clock::unix_now_ns())
        .map_err(|error| error.to_string())?;
    Ok(Some((cert, session.clone())))
}

/// Write the person's user key into the session's identity tree, refusing
/// a tree that already belongs to another person.
fn write_session_user_key(
    path: &std::path::Path,
    user_public_key: &[u8; 32],
) -> Result<(), tilo_user_identity::persist::PersistError> {
    if path.is_file() {
        let held = tilo_user_identity::persist::read_public_key(path)?;
        if &held != user_public_key {
            return Err(tilo_user_identity::persist::PersistError::Io(
                std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "the session already belongs to another identity",
                ),
            ));
        }
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    tilo_user_identity::persist::write_public_key(path, user_public_key)?;
    Ok(())
}

async fn install(
    waiting: &Waiting,
    request: Request<hyper::body::Incoming>,
) -> Result<Response<Body>, Response<Body>> {
    if !authorized(waiting, &request) {
        return Err(wrong_token());
    }
    let (_, bytes) = body(request, MAX_REQUEST_BYTES).await?;
    // JSON as a page's root issues it, its canonical encoding, or both
    // certificates of a node and its session.
    let (cert, offered) = parse_certificates(&bytes).ok_or_else(|| {
        status(
            StatusCode::BAD_REQUEST,
            "expected a DeviceCert as JSON or canonical bytes, or InstallCertificates",
        )
    })?;
    if !cert.capabilities.can_act_as_host {
        return Err(status(
            StatusCode::BAD_REQUEST,
            "the certificate does not let this device host",
        ));
    }
    let session = session_certificate(waiting, &cert, offered)
        .map_err(|reason| status(StatusCode::BAD_REQUEST, &reason))?;
    let paths = waiting.paths.clone();
    let written = cert.clone();
    tokio::task::spawn_blocking(move || {
        tilo_user_identity::install_device_cert(&paths, &written, tilo_clock::unix_now_ns())?;
        if let Some((cert, session)) = session {
            if let Some(user_key) = &session.user_key_out {
                write_session_user_key(user_key, &cert.user_public_key)?;
            }
            tilo_user_identity::persist::write_device_cert(&session.cert_out, &cert)?;
        }
        Ok::<_, tilo_user_identity::persist::PersistError>(())
    })
    .await
    .map_err(|_| status(StatusCode::INTERNAL_SERVER_ERROR, "install failed"))?
    .map_err(|error| status(StatusCode::BAD_REQUEST, &error.to_string()))?;
    let answer = Installed {
        device_public_key: hex::encode(cert.device_public_key),
        user_public_key: hex::encode(cert.user_public_key),
    };
    let _ = waiting.installed.try_send(cert);
    Ok(json(&answer))
}

#[cfg(test)]
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
        body: impl AsRef<[u8]>,
    ) -> String {
        let body = body.as_ref();
        let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket.write_all(request.as_bytes()).await.unwrap();
        socket.write_all(body).await.unwrap();
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

    fn config(dir: &std::path::Path) -> WebConfig {
        WebConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            tls: TlsSource::Development {
                hosts: vec!["localhost".into()],
                store: Some(dir.join("tls")),
            },
            allowed_origins: vec!["https://console.example".into()],
            plain_listen: Some("127.0.0.1:0".parse().unwrap()),
            public_origin: None,
            live_origin: None,
            limits: crate::web::WebLimits::default(),
            country: None,
            fetch_proxy: None,
        }
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
        let config = config(dir.path());
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

        let reply = request(addr, "POST", "/peer/v1/install", &token, [7_u8, 7, 7]).await;
        assert!(reply.starts_with("HTTP/1.1 400"), "{reply}");
        let canonical = tilo_user_identity::canonical::canonical_bytes(
            &serde_json::from_str::<DeviceCert>(&good).unwrap(),
        )
        .unwrap();
        let reply = request(addr, "POST", "/peer/v1/install", &token, canonical).await;
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert!(reply.contains(&hex::encode(own)), "{reply}");
        let installed = listener.installed().await.unwrap();
        assert_eq!(installed.device_public_key, own);
        assert!(
            tokio::net::TcpStream::connect(addr).await.is_err(),
            "stopped serving"
        );

        crate::daemon::Relay::start(
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
        .unwrap()
        .shutdown()
        .await;
    }

    /// The relay waits for the certificate while a page installs it: the
    /// page still reads its whole answer before the listener stops.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_installing_page_reads_its_answer() {
        let dir = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(dir.path().join("identity"));
        let (device, _) = tilo_user_identity::load_or_create_device_key(&paths).unwrap();
        let own = device.verifying_key().to_bytes();
        let root = SigningKey::from_bytes(&[7; 32]);
        let listener = InstallListener::bind(&config(dir.path()), paths, TOKEN).unwrap();
        let addr = listener.plain_addr.unwrap();
        let waiting = tokio::spawn(listener.installed());
        let good = cert(
            &root,
            own,
            tilo_user_identity::cloud_enrollment::host_capabilities(),
        );
        let token = format!("{INSTALL_TOKEN_HEADER}: {TOKEN}\r\n");
        let reply = request(addr, "POST", "/peer/v1/install", &token, &good).await;
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert!(
            reply.ends_with("\r\n0\r\n\r\n"),
            "the whole chunked answer: {reply}"
        );
        assert!(reply.contains(&hex::encode(own)), "{reply}");
        let installed = waiting.await.unwrap().unwrap();
        assert_eq!(installed.device_public_key, own);
        assert!(
            tokio::net::TcpStream::connect(addr).await.is_err(),
            "stopped serving"
        );
    }

    /// A node with a session beside it names the session's key, vouched for
    /// by its own, and takes the session's certificate with its own: one of
    /// the same person's devices, never a host. The session's certificate
    /// is written for the session to join with.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_node_takes_its_sessions_certificate_with_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(dir.path().join("identity"));
        let (device, _) = tilo_user_identity::load_or_create_device_key(&paths).unwrap();
        let own = device.verifying_key().to_bytes();
        let session = SigningKey::from_bytes(&[5; 32]).verifying_key().to_bytes();
        let cert_out = dir.path().join("session/delivered.cert");
        let root = SigningKey::from_bytes(&[7; 32]);
        let listener = InstallListener::bind_with_session(
            &config(dir.path()),
            paths.clone(),
            TOKEN,
            Some(SessionDevice {
                public_key: session,
                cert_out: cert_out.clone(),
                user_key_out: None,
            }),
        )
        .unwrap();
        let addr = listener.plain_addr.unwrap();
        let token = format!("{INSTALL_TOKEN_HEADER}: {TOKEN}\r\n");

        let reply = request(addr, "GET", "/peer/v1/install", "", "").await;
        assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
        let reply = request(addr, "GET", "/peer/v1/install", &token, "").await;
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        let body = &reply[reply.find('{').unwrap()..=reply.rfind('}').unwrap()];
        let info: InstallInfo = serde_json::from_str(body).unwrap();
        assert_eq!(info.device_public_key, hex::encode(own));
        let vouched = info.session.unwrap();
        assert_eq!(vouched.device_public_key, hex::encode(session));
        let signature: [u8; 64] = hex::decode(vouched.signature).unwrap().try_into().unwrap();
        device
            .verifying_key()
            .verify_strict(
                &node_session_message(&own, &session),
                &ed25519_dalek::Signature::from_bytes(&signature),
            )
            .unwrap();

        let node = |root: &SigningKey| {
            serde_json::from_str::<DeviceCert>(&cert(
                root,
                own,
                tilo_user_identity::cloud_enrollment::host_capabilities(),
            ))
            .unwrap()
        };
        let member = |root: &SigningKey, device: [u8; 32], capabilities| {
            serde_json::from_str::<DeviceCert>(&cert(root, device, capabilities)).unwrap()
        };
        let encoded = |cert: &DeviceCert| {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD
                .encode(tilo_user_identity::canonical::canonical_bytes(cert).unwrap())
        };
        let both = |offered: DeviceCert| {
            serde_json::to_string(&InstallCertificates {
                certificate: encoded(&node(&root)),
                session_certificate: Some(encoded(&offered)),
            })
            .unwrap()
        };
        let stranger = SigningKey::from_bytes(&[8; 32]);
        for refused in [
            member(&root, [9; 32], DeviceCapabilities::member()),
            member(&stranger, session, DeviceCapabilities::member()),
            member(&root, session, DeviceCapabilities::primary()),
            member(&root, session, DeviceCapabilities::host()),
        ] {
            let reply = request(addr, "POST", "/peer/v1/install", &token, both(refused)).await;
            assert!(reply.starts_with("HTTP/1.1 400"), "{reply}");
        }
        assert!(!paths.device_cert.exists() && !cert_out.exists());

        let issued = member(&root, session, DeviceCapabilities::member());
        let reply = request(
            addr,
            "POST",
            "/peer/v1/install",
            &token,
            both(issued.clone()),
        )
        .await;
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert_eq!(listener.installed().await.unwrap().device_public_key, own);
        assert_eq!(
            tilo_user_identity::persist::read_device_cert(&cert_out).unwrap(),
            issued
        );
    }
}
