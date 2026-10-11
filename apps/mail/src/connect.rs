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
    use futures_util::StreamExt as _;

    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| classify_conn_failure(&format!("lookup {host}: {e}")))?
        .collect();
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
