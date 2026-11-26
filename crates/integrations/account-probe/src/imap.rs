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
use web_time::Duration;

/// A connection to an IMAP server once it is secure.
pub type SecureStream = tokio_rustls::client::TlsStream<TcpStream>;

/// The SASL `XOAUTH2` client response Google and Microsoft take in place of
/// a password: `user=<name>\x01auth=Bearer <token>\x01\x01`.
#[must_use]
pub fn xoauth2_token(user: &str, access_token: &str) -> String {
    format!("user={user}\x01auth=Bearer {access_token}\x01\x01")
}

/// An `XOAUTH2` sign-in: whatever the server asks, the answer is the
/// account's bearer token.
pub struct Bearer {
    pub user: String,
    pub access_token: String,
}

impl async_imap::Authenticator for Bearer {
    type Response = String;

    fn process(&mut self, _challenge: &[u8]) -> Self::Response {
        xoauth2_token(&self.user, &self.access_token)
    }
}

/// Where a connection made for a server's name may go, and what looks that
/// name up. The name is looked up once and the connection goes to exactly an
/// address of that answer, so no second look-up, which might answer
/// otherwise, lies between the judging of an address and the connection.
#[derive(Clone)]
pub struct Reach {
    lookup: Lookup,
    /// Asked anew at every connection: whether only the public internet is
    /// reached now.
    public_only: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl Reach {
    /// Any address the name answers with: the mail of a person's own device,
    /// which may live on that device's own network, and of a server whose
    /// operator lets it call its own network.
    #[must_use]
    pub fn any() -> Self {
        Self {
            lookup: tilo_net::address::system_lookup(),
            public_only: Arc::new(|| false),
        }
    }

    /// The public internet only: a name that answers with an address of this
    /// machine or its network is connected to nowhere.
    #[must_use]
    pub fn public() -> Self {
        Self {
            lookup: tilo_net::address::system_lookup(),
            public_only: Arc::new(|| true),
        }
    }

    /// The public internet only, unless `open` says, asked anew at every
    /// connection, that any address is reached: a server whose operator may
    /// let it call its own network at any time.
    #[must_use]
    pub fn public_unless(open: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        Self {
            lookup: tilo_net::address::system_lookup(),
            public_only: Arc::new(move || !open()),
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
        (self.public_only)()
    }

    /// Whether `named` would be connected to within this reach, as the judge
    /// of its addresses answers now: what refuses a server before anybody is
    /// asked for its password. A connection asks the judge again and goes
    /// only where it is answered then. Looks the name up and waits for the
    /// answer, so it is called off the threads that carry out turns.
    #[must_use]
    pub fn takes(&self, named: &str) -> bool {
        if !self.is_public() {
            return true;
        }
        let Ok(asking) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return false;
        };
        asking.block_on(self.addresses(named)).is_ok()
    }

    /// What a client that makes its own connections asks where a server is,
    /// within this reach ([`tilo_dav::Options::lookup`]): the same one
    /// look-up, judged the same, and whether only the public internet is
    /// reached asked anew at every connection.
    #[must_use]
    pub fn lookup(&self) -> Lookup {
        let reach = self.clone();
        Arc::new(move |named: String| {
            let reach = reach.clone();
            Box::pin(async move { reach.addresses(&named).await })
        })
    }

    /// The addresses to connect to for `named`, from one look-up: under
    /// [`Self::public`] every one of them judged, and all of them or none.
    async fn addresses(&self, named: &str) -> std::io::Result<Vec<IpAddr>> {
        if self.is_public() {
            return tilo_net::address::public_addresses(&self.lookup, named).await;
        }
        (self.lookup)(named.to_owned()).await
    }
}

impl std::fmt::Debug for Reach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reach")
            .field("public_only", &self.is_public())
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
    let addresses = addresses
        .into_iter()
        .map(|address| SocketAddr::new(address, port))
        .collect();
    race(addresses, step, TcpStream::connect)
        .await
        .map_err(|why| format!("could not reach {named}:{port}: {why}"))
}

/// How long one address of several is given before the race counts it lost, and how long the
/// next one waits before it starts (RFC 8305).
const ATTEMPT: Duration = Duration::from_secs(6);
const STAGGER: Duration = Duration::from_millis(250);

/// `addresses` with their families taking turns, IPv6 first, each family in the order it was
/// answered in.
fn interleaved(addresses: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let (v6, v4): (Vec<_>, Vec<_>) = addresses.into_iter().partition(SocketAddr::is_ipv6);
    let (mut v6, mut v4) = (v6.into_iter(), v4.into_iter());
    let mut ordered = Vec::new();
    loop {
        let (six, four) = (v6.next(), v4.next());
        if six.is_none() && four.is_none() {
            return ordered;
        }
        ordered.extend(six);
        ordered.extend(four);
    }
}

/// Connect to the first of `addresses` that answers within `step`. A host often answers with
/// both families, and on a network that silently drops one, trying them one after another
/// costs the whole wait per dead address: they race instead, the next starting a moment after
/// the one before, and where there are several each has a deadline of its own.
async fn race<T, F, Fut>(
    addresses: Vec<SocketAddr>,
    step: Duration,
    connect: F,
) -> Result<T, &'static str>
where
    F: Fn(SocketAddr) -> Fut,
    Fut: std::future::Future<Output = std::io::Result<T>> + Send + 'static,
    T: Send + 'static,
{
    let each = if addresses.len() > 1 { ATTEMPT } else { step };
    let mut attempts = tokio::task::JoinSet::new();
    for (index, address) in interleaved(addresses).into_iter().enumerate() {
        let wait = STAGGER.saturating_mul(u32::try_from(index).unwrap_or(u32::MAX));
        let connecting = connect(address);
        attempts.spawn(async move {
            tokio::time::sleep(wait).await;
            timeout(each, connecting).await
        });
    }
    let raced = timeout(step, async {
        let mut last_error = None;
        while let Some(attempt) = attempts.join_next().await {
            match attempt {
                Ok(Ok(Ok(stream))) => return Ok(stream),
                Ok(Ok(Err(error))) => last_error = Some(unreached(&error)),
                Ok(Err(_)) | Err(_) => {}
            }
        }
        Err(last_error.unwrap_or(TIMED_OUT))
    })
    .await;
    raced.unwrap_or(Err(TIMED_OUT))
}

/// What a person reads for a server that did not answer in time.
const TIMED_OUT: &str = "it did not answer in time. Check the server name and port, and that this \
                         device is online";

/// Why a connection to a server failed, in words a person can act on: what they can check or
/// change, never the operating system's error.
pub fn unreached(error: &std::io::Error) -> &'static str {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::ConnectionRefused => {
            "nothing answers on that port. Check the server name and port"
        }
        ErrorKind::TimedOut => TIMED_OUT,
        ErrorKind::NetworkUnreachable | ErrorKind::HostUnreachable | ErrorKind::NetworkDown => {
            "no network reaches it. Check that this device is online"
        }
        ErrorKind::PermissionDenied => {
            "this device does not allow the connection. Check its firewall and network settings"
        }
        ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted => {
            "the server closed the connection. Check the port, and try again"
        }
        _ => "the connection failed. Check the server name and port, and try again",
    }
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
