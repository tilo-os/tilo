//! Reaching a subscription: the bytes a site answers a feed with over HTTP, or a capsule over
//! Gemini's own TLS connection.
//!
//! Nothing here draws or waits on a frame: a refresh calls [`read`] on a worker thread, and the
//! app takes the answer when it is done.

use std::io::{Read, Write};
use std::net::ToSocketAddrs as _;
use std::time::Duration;

use crate::parse;
use crate::{Failure, Found};

/// How long one fetch may take, as the reader's own patience: a site that has not answered by
/// then is unreachable for this refresh.
const TIMEOUT: Duration = Duration::from_secs(20);

/// The most of a feed read: a feed larger than this is not one a person reads.
const MAX_FEED_BYTES: u64 = 16 * 1024 * 1024;

/// Read the feed at `url`, a Gemini server's certificate having to match `pin` when it has one.
///
/// # Errors
///
/// Why the site could not be reached, refused the read, answered something that is not a feed, or
/// presented a certificate that is not the one pinned.
pub fn read(url: &str, pin: &str) -> Result<Found, Failure> {
    if url.trim().starts_with("gemini://") {
        gemini(url.trim(), pin)
    } else {
        http(url.trim())
    }
}

/// Read `url` over HTTP.
fn http(url: &str) -> Result<Found, Failure> {
    let client = tilo_net::http::blocking_client()
        .user_agent(concat!(
            "Tilo-Feeds/",
            env!("CARGO_PKG_VERSION"),
            " (+https://tilo.so)"
        ))
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| Failure::unavailable(format!("no http client: {e}")))?;
    let response = client
        .get(url)
        .send()
        .map_err(|e| Failure::unreachable(format!("request: {e}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(Failure::refused(
            status.as_u16(),
            format!("HTTP {}", status.as_u16()),
        ));
    }
    let mut body = Vec::new();
    response
        .take(MAX_FEED_BYTES)
        .read_to_end(&mut body)
        .map_err(|e| Failure::unreachable(format!("body: {e}")))?;
    parse::feed(&body, url)
}

/// Read a `gemini://` capsule: TLS on port 1965, the certificate it presents pinned on first
/// contact.
fn gemini(url: &str, expected_pin: &str) -> Result<Found, Failure> {
    let rest = url.strip_prefix("gemini://").unwrap_or(url);
    let authority = rest.split('/').next().unwrap_or(rest);
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h.trim_start_matches('[').trim_end_matches(']'),
            p.parse::<u16>().unwrap_or(1965),
        ),
        None => (authority, 1965),
    };
    if host.is_empty() {
        return Err(Failure::unreachable("no host in the address"));
    }
    tilo_net::install_crypto_provider();
    let config = tilo_net::pinned_client_config(|_| true)
        .map_err(|e| Failure::unavailable(format!("no TLS config: {e}")))?;
    let server = rustls::pki_types::ServerName::try_from(host.to_owned())
        .map_err(|e| Failure::unreachable(format!("this host is not a server name: {e}")))?;
    let address = (host, port)
        .to_socket_addrs()
        .map_err(|e| Failure::unreachable(format!("lookup: {e}")))?
        .next()
        .ok_or_else(|| Failure::unreachable(format!("{host} has no address")))?;
    let mut socket = std::net::TcpStream::connect_timeout(&address, TIMEOUT)
        .map_err(|e| Failure::unreachable(format!("connect: {e}")))?;
    let _ = socket.set_read_timeout(Some(TIMEOUT));
    let _ = socket.set_write_timeout(Some(TIMEOUT));
    let mut conn = rustls::ClientConnection::new(std::sync::Arc::new(config), server)
        .map_err(|e| Failure::unreachable(format!("TLS: {e}")))?;
    // The handshake alone first: a server that cannot prove who it is gets no request.
    while conn.is_handshaking() {
        conn.complete_io(&mut socket)
            .map_err(|e| Failure::unreachable(format!("TLS: {e}")))?;
    }
    let presented = pin_of(conn.peer_certificates());
    if !expected_pin.is_empty() && presented != expected_pin {
        return Err(Failure::untrusted(
            "the server presented a different certificate than when you subscribed",
        ));
    }
    let mut tls = rustls::StreamOwned { conn, sock: socket };
    tls.write_all(format!("{url}\r\n").as_bytes())
        .map_err(|e| Failure::unreachable(format!("write: {e}")))?;
    let mut body = Vec::new();
    (&mut tls)
        .take(MAX_FEED_BYTES)
        .read_to_end(&mut body)
        .map_err(|e| Failure::unreachable(format!("read: {e}")))?;
    let text = String::from_utf8_lossy(&body).into_owned();
    // The answer is "<status><space><meta>\r\n" and then the body.
    let (header, body) = text.split_once("\r\n").unwrap_or((text.as_str(), ""));
    if !header.starts_with('2') {
        return Err(Failure::refused(0, format!("gemini status {header}")));
    }
    let mut found = parse::gemtext(body, url);
    found.pin = presented;
    Ok(found)
}

/// The pin of a certificate: BLAKE3 of its DER, as hex; empty when none was presented.
fn pin_of(certificates: Option<&[rustls::pki_types::CertificateDer<'_>]>) -> String {
    certificates
        .and_then(|chain| chain.first())
        .map(|der| blake3::hash(der.as_ref()).to_hex().to_string())
        .unwrap_or_default()
}
