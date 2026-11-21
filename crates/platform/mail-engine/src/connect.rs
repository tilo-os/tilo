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

const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(6);
const STAGGER: Duration = Duration::from_millis(250);

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

pub(crate) async fn tcp(host: &str, port: u16) -> Result<TcpStream, String> {
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| classify_conn_failure(&format!("lookup {host}: {e}")))?
        .collect();
    race(addrs).await
}

async fn race(addrs: Vec<SocketAddr>) -> Result<TcpStream, String> {
    race_with(addrs, TcpStream::connect).await
}

async fn race_with<T, F, Fut>(addrs: Vec<SocketAddr>, connect: F) -> Result<T, String>
where
    F: Fn(SocketAddr) -> Fut + Sync,
    Fut: std::future::Future<Output = std::io::Result<T>> + Send,
    T: Send,
{
    use futures_util::StreamExt as _;

    let connect = &connect;
    let mut attempts: futures_util::stream::FuturesUnordered<_> = interleaved(addrs)
        .into_iter()
        .enumerate()
        .map(|(index, addr)| async move {
            tokio::time::sleep(STAGGER * u32::try_from(index).unwrap_or(u32::MAX)).await;
            match tokio::time::timeout(ATTEMPT_TIMEOUT, connect(addr)).await {
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

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn a_silent_address_does_not_hold_up_a_live_one() {
        let silent = addr("[2001:db8::1]:25");
        let live = addr("192.0.2.1:25");
        let started = tokio::time::Instant::now();
        let won = race_with(vec![silent, live], |addr| async move {
            if addr == silent {
                std::future::pending::<()>().await;
            }
            Ok(addr)
        })
        .await
        .unwrap();
        assert_eq!(won, live);
        assert_eq!(started.elapsed(), STAGGER);
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn silent_addresses_time_out_at_their_deadline() {
        let started = tokio::time::Instant::now();
        let error = race_with(vec![addr("[2001:db8::1]:25"), addr("192.0.2.1:25")], |_| {
            std::future::pending::<std::io::Result<()>>()
        })
        .await
        .unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        assert_eq!(started.elapsed(), STAGGER + ATTEMPT_TIMEOUT);
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
