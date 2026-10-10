//! Raw TCP, UDP and TLS over TCP, for protocols HTTP, WebSocket and
//! WebTransport do not carry (Gemini, IMAP, a device's own protocol).
//!
//! Desktop only: a page cannot open raw connections, so every constructor
//! answers [`Unsupported`](crate::Unsupported) there ([`Feature::Sockets`]).
//! An app that cannot work without them declares `platforms` without `web`.
//!
//! [`Feature::Sockets`]: crate::Feature::Sockets

use std::net::SocketAddr;
use std::time::Duration;

use crate::NetError;

#[cfg(not(target_family = "wasm"))]
use imp_native as imp;
#[cfg(target_family = "wasm")]
use imp_web as imp;

/// How a [`TlsStream`] decides to trust the server.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Trust {
    /// Verify the certificate chain and name against the system trust bundle.
    #[default]
    System,
    /// Accept any certificate whose handshake signature is valid, for
    /// trust-on-first-use protocols (Gemini). The caller pins
    /// [`TlsStream::peer_certificate`] itself.
    AnyCertificate,
}

/// A TCP connection.
pub struct TcpStream(imp::Tcp);

/// A TLS connection over TCP.
pub struct TlsStream(imp::Tls);

/// A listening TCP socket.
pub struct TcpListener(imp::Listener);

/// A UDP socket.
pub struct UdpSocket(imp::Udp);

macro_rules! opaque_debug {
    ($($name:ident),*) => {$(
        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!($name)).finish_non_exhaustive()
            }
        }
    )*};
}
opaque_debug!(TcpStream, TlsStream, TcpListener, UdpSocket);

impl TcpStream {
    /// Connect to `host:port`, trying each resolved address in turn.
    ///
    /// # Errors
    ///
    /// [`NetError::Unsupported`] in a page, [`NetError::Connect`].
    pub async fn connect(host: &str, port: u16) -> Result<Self, NetError> {
        imp::Tcp::connect(host, port).await.map(Self)
    }

    /// [`connect`](Self::connect) with a deadline.
    ///
    /// # Errors
    ///
    /// As [`connect`](Self::connect), and [`NetError::Timeout`].
    pub async fn connect_timeout(
        host: &str,
        port: u16,
        timeout: Duration,
    ) -> Result<Self, NetError> {
        imp::deadline(Some(timeout), imp::Tcp::connect(host, port))
            .await
            .map(Self)
    }

    /// Read into `buffer`; `Ok(0)` at the end of the stream.
    ///
    /// # Errors
    ///
    /// [`NetError::Connect`] when the connection failed.
    pub async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, NetError> {
        self.0.read(buffer).await
    }

    /// Write all of `data`.
    ///
    /// # Errors
    ///
    /// [`NetError::Connect`] when the connection failed.
    pub async fn write_all(&mut self, data: &[u8]) -> Result<(), NetError> {
        self.0.write_all(data).await
    }

    /// Close the sending half.
    ///
    /// # Errors
    ///
    /// [`NetError::Connect`] when the connection failed.
    pub async fn shutdown(&mut self) -> Result<(), NetError> {
        self.0.shutdown().await
    }

    /// The remote address.
    ///
    /// # Errors
    ///
    /// When the socket is no longer connected.
    pub fn peer_addr(&self) -> Result<SocketAddr, NetError> {
        self.0.peer_addr()
    }

    /// Start TLS on this connection, naming the server `host`.
    ///
    /// # Errors
    ///
    /// [`NetError::Protocol`] when the handshake or verification fails.
    pub async fn into_tls(self, host: &str, trust: Trust) -> Result<TlsStream, NetError> {
        imp::Tls::handshake(self.0, host, trust)
            .await
            .map(TlsStream)
    }
}

impl TlsStream {
    /// Connect to `host:port` and complete a TLS handshake.
    ///
    /// # Errors
    ///
    /// As [`TcpStream::connect`] and [`TcpStream::into_tls`].
    pub async fn connect(host: &str, port: u16, trust: Trust) -> Result<Self, NetError> {
        TcpStream::connect(host, port)
            .await?
            .into_tls(host, trust)
            .await
    }

    /// The server's end-entity certificate (DER), for pinning.
    #[must_use]
    pub fn peer_certificate(&self) -> Option<Vec<u8>> {
        self.0.peer_certificate()
    }

    /// Read into `buffer`; `Ok(0)` at the end of the stream.
    ///
    /// # Errors
    ///
    /// [`NetError::Connect`] when the connection failed.
    pub async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, NetError> {
        self.0.read(buffer).await
    }

    /// Read to the end of the stream, refusing more than `limit` bytes.
    ///
    /// # Errors
    ///
    /// [`NetError::TooLarge`], and those of [`read`](Self::read).
    pub async fn read_to_end(&mut self, limit: usize) -> Result<Vec<u8>, NetError> {
        let mut all = Vec::new();
        let mut buffer = vec![0; 16 * 1024];
        loop {
            let read = self.read(&mut buffer).await?;
            if read == 0 {
                return Ok(all);
            }
            if all.len().saturating_add(read) > limit {
                return Err(NetError::TooLarge { limit });
            }
            all.extend_from_slice(buffer.get(..read).unwrap_or_default());
        }
    }

    /// Write all of `data`.
    ///
    /// # Errors
    ///
    /// [`NetError::Connect`] when the connection failed.
    pub async fn write_all(&mut self, data: &[u8]) -> Result<(), NetError> {
        self.0.write_all(data).await
    }

    /// Send `close_notify` and close the sending half.
    ///
    /// # Errors
    ///
    /// [`NetError::Connect`] when the connection failed.
    pub async fn shutdown(&mut self) -> Result<(), NetError> {
        self.0.shutdown().await
    }
}

impl TcpListener {
    /// Listen on `address`.
    ///
    /// # Errors
    ///
    /// [`NetError::Unsupported`] in a page, [`NetError::Connect`] when the
    /// address is taken or not local.
    pub async fn bind(address: SocketAddr) -> Result<Self, NetError> {
        imp::Listener::bind(address).await.map(Self)
    }

    /// The next connection and its remote address.
    ///
    /// # Errors
    ///
    /// [`NetError::Connect`] when accepting failed.
    pub async fn accept(&self) -> Result<(TcpStream, SocketAddr), NetError> {
        let (tcp, peer) = self.0.accept().await?;
        Ok((TcpStream(tcp), peer))
    }

    /// The bound address (with the port the system chose for port 0).
    ///
    /// # Errors
    ///
    /// When the socket has no local address.
    pub fn local_addr(&self) -> Result<SocketAddr, NetError> {
        self.0.local_addr()
    }
}

impl UdpSocket {
    /// Bind to `address`.
    ///
    /// # Errors
    ///
    /// [`NetError::Unsupported`] in a page, [`NetError::Connect`].
    pub async fn bind(address: SocketAddr) -> Result<Self, NetError> {
        imp::Udp::bind(address).await.map(Self)
    }

    /// Send to and receive from only `address`.
    ///
    /// # Errors
    ///
    /// [`NetError::Connect`] when there is no route.
    pub async fn connect(&self, address: SocketAddr) -> Result<(), NetError> {
        self.0.connect(address).await
    }

    /// Send one datagram to the connected address.
    ///
    /// # Errors
    ///
    /// [`NetError::Connect`].
    pub async fn send(&self, data: &[u8]) -> Result<usize, NetError> {
        self.0.send(data).await
    }

    /// Receive one datagram from the connected address.
    ///
    /// # Errors
    ///
    /// [`NetError::Connect`].
    pub async fn recv(&self, buffer: &mut [u8]) -> Result<usize, NetError> {
        self.0.recv(buffer).await
    }

    /// Send one datagram to `address`.
    ///
    /// # Errors
    ///
    /// [`NetError::Connect`].
    pub async fn send_to(&self, data: &[u8], address: SocketAddr) -> Result<usize, NetError> {
        self.0.send_to(data, address).await
    }

    /// Receive one datagram and its sender.
    ///
    /// # Errors
    ///
    /// [`NetError::Connect`].
    pub async fn recv_from(&self, buffer: &mut [u8]) -> Result<(usize, SocketAddr), NetError> {
        self.0.recv_from(buffer).await
    }

    /// The bound address.
    ///
    /// # Errors
    ///
    /// When the socket has no local address.
    pub fn local_addr(&self) -> Result<SocketAddr, NetError> {
        self.0.local_addr()
    }
}

#[cfg(not(target_family = "wasm"))]
mod imp_native {
    use std::net::SocketAddr;
    use std::sync::Arc;

    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{DigitallySignedStruct, SignatureScheme};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use crate::NetError;
    pub(super) use crate::native::deadline;
    use crate::sockets::Trust;

    #[allow(
        clippy::needless_pass_by_value,
        reason = "used as a `map_err` function"
    )]
    fn io(error: std::io::Error) -> NetError {
        NetError::connect(error.to_string())
    }

    pub(super) struct Tcp(tokio::net::TcpStream);
    pub(super) struct Tls(tokio_rustls::client::TlsStream<tokio::net::TcpStream>);
    pub(super) struct Listener(tokio::net::TcpListener);
    pub(super) struct Udp(tokio::net::UdpSocket);

    impl Tcp {
        pub(super) async fn connect(host: &str, port: u16) -> Result<Self, NetError> {
            let host = host.trim_start_matches('[').trim_end_matches(']');
            tokio::net::TcpStream::connect((host, port))
                .await
                .map(Self)
                .map_err(|error| NetError::connect(format!("{host}:{port}: {error}")))
        }

        pub(super) async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, NetError> {
            self.0.read(buffer).await.map_err(io)
        }

        pub(super) async fn write_all(&mut self, data: &[u8]) -> Result<(), NetError> {
            self.0.write_all(data).await.map_err(io)
        }

        pub(super) async fn shutdown(&mut self) -> Result<(), NetError> {
            self.0.shutdown().await.map_err(io)
        }

        pub(super) fn peer_addr(&self) -> Result<SocketAddr, NetError> {
            self.0.peer_addr().map_err(io)
        }
    }

    impl Tls {
        pub(super) async fn handshake(
            tcp: Tcp,
            host: &str,
            trust: Trust,
        ) -> Result<Self, NetError> {
            let config = match trust {
                Trust::System => crate::native::tls::client_config()
                    .map_err(|error| NetError::Protocol(format!("TLS: {error}")))?,
                Trust::AnyCertificate => {
                    let provider = crate::native::tls::crypto_provider();
                    rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
                        .with_safe_default_protocol_versions()
                        .map_err(|error| NetError::Protocol(format!("TLS: {error}")))?
                        .dangerous()
                        .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
                        .with_no_client_auth()
                }
            };
            let name = ServerName::try_from(
                host.trim_start_matches('[')
                    .trim_end_matches(']')
                    .to_owned(),
            )
            .map_err(|error| NetError::InvalidUrl(format!("{host}: {error}")))?;
            let stream = tokio_rustls::TlsConnector::from(Arc::new(config))
                .connect(name, tcp.0)
                .await
                .map_err(|error| NetError::Protocol(format!("TLS: {error}")))?;
            Ok(Self(stream))
        }

        pub(super) fn peer_certificate(&self) -> Option<Vec<u8>> {
            let (_, connection) = self.0.get_ref();
            connection
                .peer_certificates()
                .and_then(<[CertificateDer<'_>]>::first)
                .map(|cert| cert.as_ref().to_vec())
        }

        pub(super) async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, NetError> {
            match self.0.read(buffer).await {
                Ok(read) => Ok(read),
                // Many servers close without `close_notify`; the body is
                // still complete for protocols that end at the close.
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(0),
                Err(error) => Err(io(error)),
            }
        }

        pub(super) async fn write_all(&mut self, data: &[u8]) -> Result<(), NetError> {
            self.0.write_all(data).await.map_err(io)?;
            self.0.flush().await.map_err(io)
        }

        pub(super) async fn shutdown(&mut self) -> Result<(), NetError> {
            self.0.shutdown().await.map_err(io)
        }
    }

    impl Listener {
        pub(super) async fn bind(address: SocketAddr) -> Result<Self, NetError> {
            tokio::net::TcpListener::bind(address)
                .await
                .map(Self)
                .map_err(io)
        }

        pub(super) async fn accept(&self) -> Result<(Tcp, SocketAddr), NetError> {
            let (stream, peer) = self.0.accept().await.map_err(io)?;
            Ok((Tcp(stream), peer))
        }

        pub(super) fn local_addr(&self) -> Result<SocketAddr, NetError> {
            self.0.local_addr().map_err(io)
        }
    }

    impl Udp {
        pub(super) async fn bind(address: SocketAddr) -> Result<Self, NetError> {
            tokio::net::UdpSocket::bind(address)
                .await
                .map(Self)
                .map_err(io)
        }

        pub(super) async fn connect(&self, address: SocketAddr) -> Result<(), NetError> {
            self.0.connect(address).await.map_err(io)
        }

        pub(super) async fn send(&self, data: &[u8]) -> Result<usize, NetError> {
            self.0.send(data).await.map_err(io)
        }

        pub(super) async fn recv(&self, buffer: &mut [u8]) -> Result<usize, NetError> {
            self.0.recv(buffer).await.map_err(io)
        }

        pub(super) async fn send_to(
            &self,
            data: &[u8],
            address: SocketAddr,
        ) -> Result<usize, NetError> {
            self.0.send_to(data, address).await.map_err(io)
        }

        pub(super) async fn recv_from(
            &self,
            buffer: &mut [u8],
        ) -> Result<(usize, SocketAddr), NetError> {
            self.0.recv_from(buffer).await.map_err(io)
        }

        pub(super) fn local_addr(&self) -> Result<SocketAddr, NetError> {
            self.0.local_addr().map_err(io)
        }
    }

    /// Any certificate, but the handshake signature must verify under it, so
    /// the peer holds the key of the certificate the caller pins.
    #[derive(Debug)]
    struct AnyCertificate(Arc<rustls::crypto::CryptoProvider>);

    impl ServerCertVerifier for AnyCertificate {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }
}

#[cfg(target_family = "wasm")]
mod imp_web {
    //! A page has no sockets: the types are uninhabited, and every
    //! constructor answers `Unsupported`.
    #![allow(
        clippy::unused_async,
        clippy::needless_pass_by_ref_mut,
        reason = "the same signatures as the desktop backend"
    )]

    use std::net::SocketAddr;
    use std::time::Duration;

    use crate::sockets::Trust;
    use crate::{Feature, NetError};

    #[derive(Clone, Copy)]
    enum Never {}

    fn unsupported<T>() -> Result<T, NetError> {
        Err(crate::caps::unsupported(Feature::Sockets).into())
    }

    pub(super) async fn deadline<T>(
        _timeout: Option<Duration>,
        future: impl Future<Output = Result<T, NetError>>,
    ) -> Result<T, NetError> {
        future.await
    }

    pub(super) struct Tcp(Never);
    pub(super) struct Tls(Never);
    pub(super) struct Listener(Never);
    pub(super) struct Udp(Never);

    impl Tcp {
        pub(super) async fn connect(_host: &str, _port: u16) -> Result<Self, NetError> {
            unsupported()
        }
        pub(super) async fn read(&mut self, _buffer: &mut [u8]) -> Result<usize, NetError> {
            match self.0 {}
        }
        pub(super) async fn write_all(&mut self, _data: &[u8]) -> Result<(), NetError> {
            match self.0 {}
        }
        pub(super) async fn shutdown(&mut self) -> Result<(), NetError> {
            match self.0 {}
        }
        pub(super) fn peer_addr(&self) -> Result<SocketAddr, NetError> {
            match self.0 {}
        }
    }

    impl Tls {
        pub(super) async fn handshake(
            tcp: Tcp,
            _host: &str,
            _trust: Trust,
        ) -> Result<Self, NetError> {
            match tcp.0 {}
        }
        pub(super) fn peer_certificate(&self) -> Option<Vec<u8>> {
            match self.0 {}
        }
        pub(super) async fn read(&mut self, _buffer: &mut [u8]) -> Result<usize, NetError> {
            match self.0 {}
        }
        pub(super) async fn write_all(&mut self, _data: &[u8]) -> Result<(), NetError> {
            match self.0 {}
        }
        pub(super) async fn shutdown(&mut self) -> Result<(), NetError> {
            match self.0 {}
        }
    }

    impl Listener {
        pub(super) async fn bind(_address: SocketAddr) -> Result<Self, NetError> {
            unsupported()
        }
        pub(super) async fn accept(&self) -> Result<(Tcp, SocketAddr), NetError> {
            match self.0 {}
        }
        pub(super) fn local_addr(&self) -> Result<SocketAddr, NetError> {
            match self.0 {}
        }
    }

    impl Udp {
        pub(super) async fn bind(_address: SocketAddr) -> Result<Self, NetError> {
            unsupported()
        }
        pub(super) async fn connect(&self, _address: SocketAddr) -> Result<(), NetError> {
            match self.0 {}
        }
        pub(super) async fn send(&self, _data: &[u8]) -> Result<usize, NetError> {
            match self.0 {}
        }
        pub(super) async fn recv(&self, _buffer: &mut [u8]) -> Result<usize, NetError> {
            match self.0 {}
        }
        pub(super) async fn send_to(
            &self,
            _data: &[u8],
            _address: SocketAddr,
        ) -> Result<usize, NetError> {
            match self.0 {}
        }
        pub(super) async fn recv_from(
            &self,
            _buffer: &mut [u8],
        ) -> Result<(usize, SocketAddr), NetError> {
            match self.0 {}
        }
        pub(super) fn local_addr(&self) -> Result<SocketAddr, NetError> {
            match self.0 {}
        }
    }
}

#[cfg(test)]
#[cfg(not(target_family = "wasm"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tcp_round_trip_on_loopback() {
        let listener = TcpListener::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 5];
            let read = stream.read(&mut buffer).await.unwrap();
            stream.write_all(&buffer[..read]).await.unwrap();
        });
        let mut client = TcpStream::connect("127.0.0.1", address.port())
            .await
            .unwrap();
        client.write_all(b"hello").await.unwrap();
        let mut echoed = [0; 5];
        let read = client.read(&mut echoed).await.unwrap();
        assert_eq!(&echoed[..read], b"hello");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn udp_round_trip_on_loopback() {
        let a = UdpSocket::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let b = UdpSocket::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        a.send_to(b"ping", b.local_addr().unwrap()).await.unwrap();
        let mut buffer = [0; 8];
        let (read, from) = b.recv_from(&mut buffer).await.unwrap();
        assert_eq!(&buffer[..read], b"ping");
        assert_eq!(from, a.local_addr().unwrap());
    }
}
