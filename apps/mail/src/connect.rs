//! Opening the TCP connection to a mail server.
//!
//! A host often resolves to both IPv6 and IPv4 addresses, and on a network
//! where one family is silently dropped, trying the addresses one after the
//! other costs the operating system's whole connect timeout per dead address.
//! [`tcp`] races them instead (RFC 8305): the next address starts a moment
//! after the previous one, the first to connect wins, and each attempt has its
//! own short deadline.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::TcpStream;

use crate::validate::classify_conn_failure;

/// How long one address gets before it counts as dead.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(6);
/// How long an attempt runs alone before the next address joins the race.
const STAGGER: Duration = Duration::from_millis(250);

/// `addrs` ordered IPv6 first, then alternating families, so a dead family
/// never hides a working one.
pub(crate) fn interleaved(addrs: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let (mut v6, mut v4): (Vec<_>, Vec<_>) = addrs.into_iter().partition(SocketAddr::is_ipv6);
    v6.reverse();
    v4.reverse();
    let mut ordered = Vec::with_capacity(v6.len() + v4.len());
    while !(v6.is_empty() && v4.is_empty()) {
        ordered.extend(v6.pop());
        ordered.extend(v4.pop());
    }
    ordered
}

/// Connect to `host:port`, racing its addresses.
///
/// # Errors
///
/// A user-facing reason when the name does not resolve or no address accepts.
pub(crate) async fn tcp(host: &str, port: u16) -> Result<TcpStream, String> {
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| classify_conn_failure(&format!("lookup {host}: {e}")))?
        .collect();
    race(addrs).await
}

/// Connect to the first of `addrs` that accepts, racing them as [`tcp`] does.
async fn race(addrs: Vec<SocketAddr>) -> Result<TcpStream, String> {
    use futures_util::StreamExt as _;

    let mut attempts: futures_util::stream::FuturesUnordered<_> = interleaved(addrs)
        .into_iter()
        .enumerate()
        .map(|(index, addr)| async move {
            tokio::time::sleep(STAGGER * u32::try_from(index).unwrap_or(u32::MAX)).await;
            match tokio::time::timeout(ATTEMPT_TIMEOUT, TcpStream::connect(addr)).await {
                Ok(connected) => connected.map_err(|e| e.to_string()),
                Err(_) => Err("connection timed out".to_owned()),
            }
        })
        .collect();
    let mut last = String::from("lookup returned no address");
    while let Some(attempt) = attempts.next().await {
        match attempt {
            Ok(stream) => return Ok(stream),
            Err(error) => last = error,
        }
    }
    Err(classify_conn_failure(&last))
}

/// An SMTP session with `host:port` on a connection [`tcp`] raced, greeted
/// and past `EHLO`: implicit TLS (`implicit`, the system roots) on 465,
/// otherwise a required STARTTLS upgrade with `starttls`.
///
/// `lettre` would connect by itself, but it tries a host's addresses one
/// after another, each with the whole command timeout, so on a network that
/// drops one address family an account check or a send waited ten seconds
/// for every dead address before the SMTP server was even asked.
///
/// # Errors
///
/// A user-facing reason when no address accepts, TLS fails, or the server
/// does not answer as SMTP.
pub(crate) async fn smtp(
    host: &str,
    port: u16,
    implicit: std::sync::Arc<rustls::ClientConfig>,
    starttls: lettre::transport::smtp::client::TlsParameters,
) -> Result<lettre::transport::smtp::client::AsyncSmtpConnection, String> {
    use lettre::transport::smtp::client::AsyncSmtpConnection;
    use lettre::transport::smtp::extension::ClientId;

    let tcp = tcp(host, port).await?;
    let hello = ClientId::default();
    let failed = |e: lettre::transport::smtp::Error| classify_conn_failure(&e.to_string());
    if port == 465 {
        let name = rustls::pki_types::ServerName::try_from(host.to_owned())
            .map_err(|e| format!("invalid SMTP host name: {e}"))?;
        let tls = tokio_rustls::TlsConnector::from(implicit)
            .connect(name, tcp)
            .await
            .map_err(|e| classify_conn_failure(&e.to_string()))?;
        return AsyncSmtpConnection::connect_with_transport(Box::new(Tls(tls)), &hello)
            .await
            .map_err(failed);
    }
    let mut connection = AsyncSmtpConnection::connect_with_transport(Box::new(tcp), &hello)
        .await
        .map_err(failed)?;
    if !connection.can_starttls() {
        return Err("the SMTP server does not offer STARTTLS".into());
    }
    connection
        .starttls(starttls, &hello)
        .await
        .map_err(failed)?;
    Ok(connection)
}

/// A TLS connection `lettre` can run SMTP over.
#[derive(Debug)]
struct Tls(tokio_rustls::client::TlsStream<TcpStream>);

impl tokio::io::AsyncRead for Tls {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().0).poll_read(cx, buf)
    }
}

impl tokio::io::AsyncWrite for Tls {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.get_mut().0).poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().0).poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().0).poll_shutdown(cx)
    }
}

impl lettre::transport::smtp::client::AsyncTokioStream for Tls {
    fn peer_addr(&self) -> std::io::Result<SocketAddr> {
        self.0.get_ref().0.peer_addr()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(text: &str) -> SocketAddr {
        text.parse().unwrap()
    }

    #[test]
    fn address_families_alternate_starting_with_ipv6() {
        let ordered = interleaved(vec![
            addr("192.0.2.1:993"),
            addr("192.0.2.2:993"),
            addr("[2001:db8::1]:993"),
            addr("[2001:db8::2]:993"),
            addr("[2001:db8::3]:993"),
        ]);
        assert_eq!(
            ordered,
            [
                addr("[2001:db8::1]:993"),
                addr("192.0.2.1:993"),
                addr("[2001:db8::2]:993"),
                addr("192.0.2.2:993"),
                addr("[2001:db8::3]:993"),
            ]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_listening_address_connects() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(tcp("127.0.0.1", port).await.is_ok());
    }

    /// An address that never answers (a documentation network nothing
    /// routes back from) does not hold up a live one behind it.
    #[tokio::test(flavor = "current_thread")]
    async fn a_silent_address_does_not_hold_up_a_live_one() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let live = listener.local_addr().unwrap();
        let started = std::time::Instant::now();
        let stream = race(vec![addr("192.0.2.1:25"), live]).await.unwrap();
        assert_eq!(stream.peer_addr().unwrap(), live);
        assert!(
            started.elapsed() < ATTEMPT_TIMEOUT,
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_refused_port_reports_a_reason() {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let error = tcp("127.0.0.1", port).await.unwrap_err();
        assert!(error.contains("refused"), "{error}");
    }
}
