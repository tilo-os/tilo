//! The secured connection to an IMAP server, opened the way its account
//! says: TLS from the first byte, or the cleartext greeting, `STARTTLS` and
//! then TLS. The sign-in probe opens it to prove a credential, and the mail
//! session a host carries (tilo-mail-session) opens it the same way to read
//! and file mail, so what was proved is what is used.
//!
//! Nothing signs in before the connection is secure, and nothing a server
//! said in the clear is believed after the upgrade.

use std::sync::Arc;

use accounts::email::Security;
use tokio::net::TcpStream;
use tokio::time::timeout;
use web_time::{Duration, Instant};

/// A connection to an IMAP server once it is secure.
pub type SecureStream = tokio_rustls::client::TlsStream<TcpStream>;

/// Reach `host:port`. The resolver is bounded by the same `step` as the
/// connection; connect timeouts alone do not bound DNS stalls.
///
/// # Errors
/// The name did not resolve, or no address of it answered in time.
pub async fn reach(host: &str, port: u16, step: Duration) -> Result<TcpStream, String> {
    let addresses: Vec<_> = timeout(step, tokio::net::lookup_host((host, port)))
        .await
        .map_err(|_| "server name resolution timed out".to_string())?
        .map_err(|_| "server name could not be resolved".to_string())?
        .collect();
    if addresses.is_empty() {
        return Err("server name returned no address".into());
    }
    let deadline = Instant::now().checked_add(step);
    let mut last_error = None;
    for address in addresses {
        let Some(remaining) =
            deadline.and_then(|deadline| deadline.checked_duration_since(Instant::now()))
        else {
            break;
        };
        match timeout(remaining, TcpStream::connect(address)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => last_error = Some(error.to_string()),
            Err(_) => last_error = Some("connection timed out".into()),
        }
    }
    Err(format!(
        "could not reach {host}:{port}: {}",
        last_error.unwrap_or_else(|| "connection timed out".into())
    ))
}

/// Open the secured connection to the IMAP server at `host:port`, with its
/// greeting read: what a sign-in is sent over next. Each step of it may take
/// `step`.
///
/// # Errors
/// The server could not be reached, did not greet as an IMAP server, would
/// not upgrade a connection that has to, or presented a certificate `tls`
/// does not trust.
pub async fn open(
    host: &str,
    port: u16,
    security: Security,
    tls: Arc<rustls::ClientConfig>,
    step: Duration,
) -> Result<async_imap::Client<SecureStream>, String> {
    let server_name = rustls::pki_types::ServerName::try_from(host.to_owned())
        .map_err(|_| "invalid IMAP server name".to_string())?;
    let tcp = reach(host, port, step).await?;
    let tcp = match security {
        Security::Tls => tcp,
        Security::StartTls => upgrade(tcp, step).await?,
    };
    let stream = timeout(
        step,
        tokio_rustls::TlsConnector::from(tls).connect(server_name, tcp),
    )
    .await
    .map_err(|_| "IMAP server did not answer before the timeout".to_string())?
    .map_err(|error| insecure("IMAP", &error))?;
    let mut client = async_imap::Client::new(stream);
    if security == Security::Tls {
        // A connection that upgraded was greeted in the clear, and a server
        // does not greet it again.
        greeted(&mut client, step).await?;
    }
    Ok(client)
}

/// Why the secure connection to a server that speaks `protocol` was not
/// made, saying so when it is the server's certificate: the one cause a
/// person who runs their own server can act on.
pub(crate) fn insecure(protocol: &str, error: &std::io::Error) -> String {
    let untrusted = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
        .is_some_and(|error| matches!(error, rustls::Error::InvalidCertificate(_)));
    if untrusted {
        format!(
            "could not establish the secure {protocol} connection: the server's certificate is not trusted"
        )
    } else {
        format!("could not establish the secure {protocol} connection")
    }
}

/// Ask a server reached in the clear for TLS: its greeting, then `STARTTLS`.
/// What the server, or anyone between, sent in the clear after its answer is
/// dropped with this client's buffer.
async fn upgrade(tcp: TcpStream, step: Duration) -> Result<TcpStream, String> {
    let mut client = async_imap::Client::new(tcp);
    greeted(&mut client, step).await?;
    timeout(step, client.run_command_and_check_ok("STARTTLS", None))
        .await
        .map_err(|_| "IMAP server did not answer before the timeout".to_string())?
        .map_err(|error| match error {
            async_imap::error::Error::No(_) | async_imap::error::Error::Bad(_) => {
                "IMAP server does not offer STARTTLS on this port".to_string()
            }
            _ => "IMAP server closed the connection unexpectedly".to_string(),
        })?;
    Ok(client.into_inner())
}

/// Read the server's greeting, which has to say it is ready.
async fn greeted<T>(client: &mut async_imap::Client<T>, step: Duration) -> Result<(), String>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + std::fmt::Debug + Send,
{
    let greeting = timeout(step, client.read_response())
        .await
        .map_err(|_| "IMAP server response timed out".to_string())?
        .ok_or_else(|| "IMAP server closed the connection unexpectedly".to_string())?
        .map_err(|_| "server did not provide a valid IMAP greeting".to_string())?;
    if matches!(
        greeting.parsed(),
        async_imap::imap_proto::Response::Data {
            status: async_imap::imap_proto::Status::Ok,
            ..
        }
    ) {
        Ok(())
    } else {
        Err("server did not provide a valid IMAP greeting".into())
    }
}
