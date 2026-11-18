//! The secured connection to an IMAP server, opened the way its account
//! says: TLS from the first byte, or the cleartext greeting, `STARTTLS` and
//! then TLS. The sign-in probe opens it to prove a credential, and the mail
//! session a host carries (tilo-mail-session) opens it the same way to read
//! and file mail, so what was proved is what is used.
//!
//! Nothing signs in before the connection is secure, and nothing a server
//! said in the clear is believed after the upgrade.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use accounts::email::Security;
use tilo_net::address::Lookup;
use tokio::net::TcpStream;
use tokio::time::timeout;
use web_time::{Duration, Instant};

/// A connection to an IMAP server once it is secure.
pub type SecureStream = tokio_rustls::client::TlsStream<TcpStream>;

/// Where a connection made for a server's name may go, and what looks that
/// name up. The name is looked up once and the connection goes to exactly an
/// address of that answer, so no second look-up, which might answer
/// otherwise, lies between the judging of an address and the connection.
#[derive(Clone)]
pub struct Reach {
    lookup: Lookup,
    public_only: bool,
}

impl Reach {
    /// Any address the name answers with: the mail of a person's own device,
    /// which may live on that device's own network, and of a server whose
    /// operator lets it call its own network.
    #[must_use]
    pub fn any() -> Self {
        Self {
            lookup: tilo_net::address::system_lookup(),
            public_only: false,
        }
    }

    /// The public internet only: a name that answers with an address of this
    /// machine or its network is connected to nowhere.
    #[must_use]
    pub fn public() -> Self {
        Self {
            lookup: tilo_net::address::system_lookup(),
            public_only: true,
        }
    }

    /// `self`, with names looked up by `lookup` instead of the system's
    /// resolver.
    #[must_use]
    pub fn looked_up_by(mut self, lookup: Lookup) -> Self {
        self.lookup = lookup;
        self
    }

    /// Whether only the public internet is reached.
    #[must_use]
    pub fn is_public(&self) -> bool {
        self.public_only
    }

    /// The addresses to connect to for `named`, from one look-up.
    async fn addresses(&self, named: &str) -> std::io::Result<Vec<IpAddr>> {
        (self.lookup)(named.to_owned()).await
    }
}

impl std::fmt::Debug for Reach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reach")
            .field("public_only", &self.public_only)
            .finish_non_exhaustive()
    }
}

/// Reach `named:port` within `to`. The look-up is bounded by the same `step`
/// as the connection; connect timeouts alone do not bound DNS stalls.
///
/// # Errors
/// The name did not resolve, answered with an address `to` does not reach,
/// or no address of it answered in time.
pub async fn reach(
    to: &Reach,
    named: &str,
    port: u16,
    step: Duration,
) -> Result<TcpStream, String> {
    let addresses = timeout(step, to.addresses(named))
        .await
        .map_err(|_| "server name resolution timed out".to_string())?
        .map_err(|error| {
            if tilo_net::address::not_public(&error) {
                error.to_string()
            } else {
                "server name could not be resolved".to_string()
            }
        })?;
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
        match timeout(
            remaining,
            TcpStream::connect(SocketAddr::new(address, port)),
        )
        .await
        {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => last_error = Some(error.to_string()),
            Err(_) => last_error = Some("connection timed out".into()),
        }
    }
    Err(format!(
        "could not reach {named}:{port}: {}",
        last_error.unwrap_or_else(|| "connection timed out".into())
    ))
}

/// Open the secured connection to the IMAP server at `named:port`, reached
/// within `to`, with its
/// greeting read: what a sign-in is sent over next. Each step of it may take
/// `step`.
///
/// # Errors
/// The server could not be reached, did not greet as an IMAP server, would
/// not upgrade a connection that has to, or presented a certificate `tls`
/// does not trust.
pub async fn open(
    to: &Reach,
    named: &str,
    port: u16,
    security: Security,
    tls: Arc<rustls::ClientConfig>,
    step: Duration,
) -> Result<async_imap::Client<SecureStream>, String> {
    let server_name = rustls::pki_types::ServerName::try_from(named.to_owned())
        .map_err(|_| "invalid IMAP server name".to_string())?;
    let tcp = reach(to, named, port, step).await?;
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

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, TcpListener};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{Reach, reach};

    const STEP: std::time::Duration = std::time::Duration::from_secs(8);
    /// A mail server's name, made up for this file.
    const NAMED: &str = "mail.example.test";

    /// A look-up that answers every name with this machine, and counts how
    /// often it was asked.
    fn this_machine(asked: &Arc<AtomicUsize>) -> tilo_net::address::Lookup {
        let asked = Arc::clone(asked);
        Arc::new(move |_name: String| {
            asked.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]) })
        })
    }

    /// How many connections were made to `listener`, which waits for none: a
    /// connection that was made is counted.
    fn connections(listener: &TcpListener) -> usize {
        std::iter::from_fn(|| listener.accept().ok()).count()
    }

    #[test]
    fn a_name_that_answers_with_this_machine_is_connected_to_nowhere_on_the_public_internet() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a port here");
        listener
            .set_nonblocking(true)
            .expect("a listener that waits for nobody");
        let port = listener.local_addr().expect("its address").port();
        let asked = Arc::new(AtomicUsize::new(0));
        let own = |name: &str| {
            Err(format!(
                "{name} answers with an address of this machine or its network"
            ))
        };
        let asking = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("sockets and a clock");
        asking.block_on(async {
            let public = Reach::public().looked_up_by(this_machine(&asked));
            let named = reach(&public, NAMED, port, STEP).await.map(drop);
            assert_eq!(named, own(NAMED));
            assert_eq!(connections(&listener), 0, "nothing is connected to");
            assert_eq!(asked.load(Ordering::SeqCst), 1, "one look-up");

            // An address written out is judged as itself and never looked up.
            let written = reach(&public, "127.0.0.1", port, STEP).await.map(drop);
            assert_eq!(written, own("127.0.0.1"));
            assert_eq!(connections(&listener), 0, "nothing is connected to");
            assert_eq!(
                asked.load(Ordering::SeqCst),
                1,
                "an address is not looked up"
            );

            // Where any address is reached, what the one look-up answered is
            // what is connected to.
            let any = Reach::any().looked_up_by(this_machine(&asked));
            let reached = reach(&any, NAMED, port, STEP).await;
            assert!(reached.is_ok(), "{reached:?}");
            assert_eq!(connections(&listener), 1, "one connection, to this machine");
            assert_eq!(asked.load(Ordering::SeqCst), 2, "one look-up for it");
        });
    }
}
