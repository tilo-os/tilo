//! Bounded, injectable account connection probes used by resource actions. The
//! probes speak the protocols through the same clients the apps use:
//! `async-imap` (as Mail does) for IMAP and the shared `tilo-dav` client for
//! `CalDAV`/`WebDAV`. An email account's SMTP sign-in is spoken a line at a
//! time (`smtp`), so the probe carries no mail transport.
//!
//! They run wherever the account's server can be reached: on the device that
//! keeps the account, and on the machine that runs a person's workspace when
//! the account is added from a page. What a page's Mail reads and sends there
//! is its own crate (tilo-mail-session).

use std::sync::Arc;
use web_time::Duration;

use tilo_dav::{Credentials, DavClient, Depth, StatusCode, props};
use tokio::time::timeout;

#[cfg(unix)]
pub mod connect;
#[cfg(test)]
mod dav_reach_tests;
pub mod imap;
pub mod mail_servers;
mod smtp;
/// A mail server with real sockets that takes one made-up login: what a
/// rehearsal signs in to.
#[cfg(any(test, feature = "test-support"))]
pub mod stand_in;

pub trait AccountTester: Send + Sync {
    /// Resolve real calendars during setup. Test doubles must opt in explicitly.
    fn discover_calendars(
        &self,
        _account: &accounts::Account,
        _secret: &accounts::Secret,
    ) -> Result<Vec<tilo_dav::discovery::CalendarCollection>, String> {
        Err("Calendar discovery is unavailable".into())
    }

    /// Work out where the mail of `address` lives, from the address alone
    /// ([`mail_servers::find`]). `allowed` says whether a server of that name may be
    /// asked anything. Test doubles must opt in explicitly.
    fn find_mail_servers(
        &self,
        _address: &str,
        _allowed: &dyn Fn(&str) -> bool,
    ) -> Result<mail_servers::MailServers, String> {
        Err("Mail server discovery is unavailable".into())
    }

    fn test(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<String, String>;
}

/// Sign in to the IMAP server `client` opened: with the account's bearer
/// token by XOAUTH2 where the provider signed it in on its own page, and
/// with its password otherwise. Errors never carry the credential.
async fn imap_signed_in(
    client: async_imap::Client<imap::SecureStream>,
    user: &str,
    secret: &str,
    oauth: bool,
) -> Result<async_imap::Session<imap::SecureStream>, String> {
    let signed_in = if oauth {
        let bearer = imap::Bearer {
            user: user.to_owned(),
            access_token: secret.to_owned(),
        };
        timeout(
            LiveAccountTester::NETWORK_TIMEOUT,
            client.authenticate("XOAUTH2", bearer),
        )
        .await
    } else {
        timeout(
            LiveAccountTester::NETWORK_TIMEOUT,
            client.login(user, secret),
        )
        .await
    };
    signed_in
        .map_err(|_| "IMAP server did not answer before the timeout".to_string())?
        .map_err(|(error, _)| match error {
            async_imap::error::Error::No(_) | async_imap::error::Error::Bad(_) => {
                "IMAP server rejected the saved credential".to_string()
            }
            async_imap::error::Error::Validate(_) => {
                "login contains unsupported control characters".to_string()
            }
            _ => "IMAP server closed the connection unexpectedly".to_string(),
        })
}

#[derive(Default)]
pub struct LiveAccountTester;

impl LiveAccountTester {
    const NETWORK_TIMEOUT: Duration = Duration::from_secs(8);

    fn imap_login_name(value: &str) -> Result<&str, String> {
        if value.chars().any(char::is_control) {
            return Err("login contains unsupported control characters".into());
        }
        Ok(value)
    }

    /// Secure the connection the way the account says ([`imap::open`]), read
    /// the greeting, authenticate, and log out; then sign in to the account's
    /// SMTP server and leave, so an account that reads mail is one that
    /// sends it. A TCP accept alone is deliberately not success: it says
    /// nothing about the saved credential or whether this is a mail
    /// endpoint.
    fn email(
        reach: &imap::Reach,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<String, String> {
        // Without the system's authorities every server's certificate would be refused and
        // the server blamed: say which file this program lacks instead.
        if let Some(lacks) = tilo_net::lacks_authorities() {
            return Err(lacks);
        }
        Self::proved(reach, account, secret, tilo_net::system_roots())
    }

    /// [`Self::email`]'s proof as a device makes it, trusting `roots` alone.
    #[cfg(test)]
    fn email_with_roots(
        account: &accounts::Account,
        secret: &accounts::Secret,
        roots: rustls::RootCertStore,
    ) -> Result<String, String> {
        Self::proved(&imap::Reach::any(), account, secret, roots)
    }

    /// [`Self::email`]'s proof, trusting `roots`: every connection of it is
    /// made within `reach`.
    fn proved(
        reach: &imap::Reach,
        account: &accounts::Account,
        secret: &accounts::Secret,
        roots: rustls::RootCertStore,
    ) -> Result<String, String> {
        let host = account
            .config
            .get("imap_host")
            .and_then(serde_json::Value::as_str)
            .filter(|host| !host.trim().is_empty())
            .ok_or_else(|| "account has no IMAP server".to_string())?;
        let port = account
            .config
            .get("imap_port")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(993);
        let username = account
            .config
            .get("username")
            .and_then(serde_json::Value::as_str)
            .filter(|username| !username.trim().is_empty())
            .or_else(|| {
                account
                    .config
                    .get("email")
                    .and_then(serde_json::Value::as_str)
            })
            .ok_or_else(|| "account has no IMAP login name".to_string())?;
        if secret.password.is_empty() {
            return Err("account has no saved password".into());
        }
        let user = Self::imap_login_name(username.trim())?;
        let password = Self::imap_login_name(secret.password.trim())?;
        let smtp_host = account
            .config
            .get("smtp_host")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .ok_or_else(|| "account has no SMTP server".to_string())?;
        let smtp_port = account
            .config
            .get("smtp_port")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(465);

        // An account the provider signed in on its own page is proved the
        // way it will read and send: its secret is a bearer token, carried
        // by XOAUTH2 to both servers.
        // A setting that does not read is a password account, never one
        // signed in with a token.
        let oauth = accounts::email::is_oauth(&account.config);
        let tls = tilo_net::client_config_with_roots(roots)
            .map(Arc::new)
            .map_err(|_| "could not initialize secure protocol versions".to_string())?;
        let security = accounts::email::imap_security_of(&account.config);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "could not start the IMAP probe".to_string())?;
        runtime.block_on(async {
            let client = imap::open(
                reach,
                host,
                port,
                security,
                Arc::clone(&tls),
                Self::NETWORK_TIMEOUT,
            )
            .await?;
            let mut session = imap_signed_in(client, user, password, oauth).await?;
            let _ = timeout(Self::NETWORK_TIMEOUT, session.logout()).await;
            // An account that reads mail and cannot send any is found out
            // here, not at its person's first message.
            smtp::sign_in(
                reach,
                smtp_host,
                smtp_port,
                user,
                password,
                oauth,
                tls,
                Self::NETWORK_TIMEOUT,
            )
            .await?;
            Ok("IMAP and SMTP sign-in succeeded".into())
        })
    }

    /// A DAV account's sign-in at its server, reached `within` a reach where
    /// one is given: a server's, judged at each connection. A device's own
    /// tester gives none and reaches any address.
    fn dav(
        within: Option<&imap::Reach>,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<String, String> {
        let url = account
            .config
            .get(if account.type_id.as_str() == "webdav" {
                "base_url"
            } else {
                "server_url"
            })
            .and_then(|value| value.as_str())
            .ok_or_else(|| "account has no server URL".to_string())?;
        let parsed = tilo_dav::Url::parse(url).map_err(|_| "account has an invalid server URL")?;
        if !accounts::is_secure_endpoint(parsed.as_str()) {
            return Err("refusing to send a credential without HTTPS".into());
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err("server URL must not contain credentials".into());
        }
        let username = account
            .config
            .get("username")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "account has no username".to_string())?;
        let client = DavClient::new(&tilo_dav::Options {
            user_agent: None,
            timeout: Self::NETWORK_TIMEOUT,
            connect_timeout: Some(Self::NETWORK_TIMEOUT),
            // Never replay Basic credentials to a redirect target. Discovery
            // belongs in setup; a stored endpoint must answer at its exact
            // origin.
            redirects: tilo_dav::Redirects::Refuse,
            lookup: within.map(imap::Reach::lookup),
        })
        .map_err(|_| {
            tilo_net::lacks_authorities()
                .unwrap_or_else(|| "could not initialize the secure HTTP client".to_owned())
        })?
        .with_credentials(Credentials::new(username, secret.password.clone()));
        let response = client
            .propfind(parsed, Depth::Zero, &[props::RESOURCETYPE])
            .map_err(|error| format!("connection failed: {error}"))?;
        if response.status().is_success() {
            Ok("Server accepted the signed-in DAV request".into())
        } else if response.status() == StatusCode::UNAUTHORIZED {
            Err("server rejected the saved credential".into())
        } else {
            Err(format!(
                "server returned HTTP {}",
                response.status().as_u16()
            ))
        }
    }
}

impl AccountTester for LiveAccountTester {
    fn discover_calendars(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<Vec<tilo_dav::discovery::CalendarCollection>, String> {
        Self::calendars(None, account, secret)
    }

    fn test(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<String, String> {
        Self::tested(None, account, secret)
    }
}

impl LiveAccountTester {
    /// The calendars of a `CalDAV` account, asked of its server `within` a
    /// reach where one is given: a server's, judged at each connection. A
    /// device's own tester gives none and reaches any address.
    fn calendars(
        within: Option<&imap::Reach>,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<Vec<tilo_dav::discovery::CalendarCollection>, String> {
        let url = account
            .config
            .get("server_url")
            .and_then(serde_json::Value::as_str)
            .ok_or("account has no server URL")?;
        let url = tilo_dav::Url::parse(url).map_err(|_| "invalid server URL")?;
        if !accounts::is_secure_endpoint(url.as_str()) {
            return Err("refusing to send a credential without HTTPS".into());
        }
        let username = account
            .config
            .get("username")
            .and_then(serde_json::Value::as_str)
            .ok_or("account has no username")?;
        let client = DavClient::new(&tilo_dav::Options {
            timeout: Duration::from_secs(2),
            connect_timeout: Some(Duration::from_secs(2)),
            redirects: tilo_dav::Redirects::Refuse,
            lookup: within.map(imap::Reach::lookup),
            ..tilo_dav::Options::default()
        })
        .map_err(|_| {
            tilo_net::lacks_authorities()
                .unwrap_or_else(|| "could not initialize the secure HTTP client".to_owned())
        })?
        .with_credentials(Credentials::new(username, secret.password.clone()));
        tilo_dav::discovery::calendars(&client, &url)
    }

    /// Whether `account` signs in with `secret`, over connections `within` a
    /// reach where one is given.
    fn tested(
        within: Option<&imap::Reach>,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<String, String> {
        match account.type_id.as_str() {
            "email" => Self::email(
                &within.cloned().unwrap_or_else(imap::Reach::any),
                account,
                secret,
            ),
            "caldav" => {
                let configured = account
                    .config
                    .get("calendars")
                    .and_then(serde_json::Value::as_array)
                    .filter(|calendars| !calendars.is_empty())
                    .ok_or("Choose calendars in Edit account before checking this connection")?;
                let discovered = Self::calendars(within, account, secret)?;
                if !configured.iter().all(|calendar| {
                    calendar
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|id| discovered.iter().any(|found| found.href == id))
                }) {
                    return Err("A selected calendar is no longer accessible. Edit the account to choose calendars again.".into());
                }
                Ok(format!("Found {} selected calendars", configured.len()))
            }
            "webdav" | "nextcloud" => Self::dav(within, account, secret),
            _ => Err("unsupported account type".into()),
        }
    }
}

/// A server's proof of a mail account's sign-in, trusting `extra` beside the system's own
/// authorities, with every connection going only where `reach` lets it: a test's mail
/// server, whose certificate no authority signed ([`stand_in`]). It proves mail accounts
/// alone.
#[cfg(any(test, feature = "test-support"))]
pub struct TrustingMailTester {
    pub reach: imap::Reach,
    pub extra: Vec<rustls::pki_types::CertificateDer<'static>>,
}

#[cfg(any(test, feature = "test-support"))]
impl AccountTester for TrustingMailTester {
    fn test(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<String, String> {
        let mut roots = tilo_net::system_roots();
        for certificate in &self.extra {
            roots
                .add(certificate.clone())
                .map_err(|error| format!("a trusted certificate is not readable: {error}"))?;
        }
        LiveAccountTester::proved(&self.reach, account, secret, roots)
    }
}

/// What proves accounts on a server: [`LiveAccountTester`], with every
/// connection it makes for an account, for mail, a calendar or files, going
/// only where its reach lets it ([`imap::Reach::public_unless`]), and which
/// also works out an address's mail servers over the network
/// ([`mail_servers::find`]). Only a person's server asks that, so an app that
/// proves sign-ins takes [`LiveAccountTester`] and carries no DNS resolver.
pub struct FindingAccountTester(pub imap::Reach);

impl AccountTester for FindingAccountTester {
    fn discover_calendars(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<Vec<tilo_dav::discovery::CalendarCollection>, String> {
        LiveAccountTester::calendars(Some(&self.0), account, secret)
    }

    fn find_mail_servers(
        &self,
        address: &str,
        allowed: &dyn Fn(&str) -> bool,
    ) -> Result<mail_servers::MailServers, String> {
        mail_servers::find(
            address,
            &mail_servers::Live::new(allowed).within(self.0.clone()),
        )
    }

    fn test(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<String, String> {
        LiveAccountTester::tested(Some(&self.0), account, secret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use test_http::{Reply, Responder};

    /// The sign-in proof against the stand-in mail server, over real sockets.
    mod stand_in_tests;

    /// An IMAP server that is TLS from the first byte, with the identity
    /// `server`. Answers the `LOGIN` it was sent.
    pub(crate) fn imap_server(
        server: rustls::ServerConfig,
        greeting: &'static str,
        login_reply: &'static str,
    ) -> (u16, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let port = listener.local_addr().expect("listener address").port();
        let worker = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().expect("client");
            let connection = rustls::ServerConnection::new(Arc::new(server)).expect("TLS server");
            let mut stream = rustls::StreamOwned::new(connection, tcp);
            stream.write_all(greeting.as_bytes()).expect("greeting");
            stream.flush().expect("greeting flush");
            let mut reader = BufReader::new(stream);
            let mut login = String::new();
            if reader.read_line(&mut login).unwrap_or(0) == 0 {
                return login;
            }
            let tag = login.split(' ').next().unwrap_or_default().to_owned();
            let stream = reader.get_mut();
            stream
                .write_all(login_reply.replace("TAG", &tag).as_bytes())
                .expect("login response");
            stream.flush().expect("login flush");
            let mut logout = String::new();
            if reader.read_line(&mut logout).unwrap_or(0) > 0 {
                let tag = logout.split(' ').next().unwrap_or_default().to_owned();
                let stream = reader.get_mut();
                let _ = write!(stream, "* BYE logging out\r\n{tag} OK LOGOUT completed\r\n");
                let _ = stream.flush();
            }
            login
        });
        (port, worker)
    }

    /// A test server's TLS identity, and the roots that trust exactly it.
    pub(crate) fn server_tls() -> (rustls::ServerConfig, rustls::RootCertStore) {
        let certified =
            rcgen::generate_simple_self_signed(vec!["localhost".into()]).expect("test certificate");
        let cert_der = certified.cert.der().clone();
        let key_der =
            rustls::pki_types::PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der());
        let server = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .with_no_client_auth()
        .with_single_cert(
            vec![cert_der.clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(key_der),
        )
        .expect("server certificate");
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert_der).expect("trust test certificate");
        (server, roots)
    }

    /// An IMAP server on a port that starts in the clear. It greets, and
    /// when it `offers` the upgrade it takes the sign-in over TLS after
    /// `STARTTLS`. Answers every line it was sent in the clear, and the
    /// `LOGIN` it was sent once the connection was secure.
    fn imap_starttls_server(
        server: rustls::ServerConfig,
        offers: bool,
    ) -> (u16, std::thread::JoinHandle<(Vec<String>, String)>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let port = listener.local_addr().expect("listener address").port();
        let worker = std::thread::spawn(move || {
            let (mut tcp, _) = listener.accept().expect("client");
            tcp.write_all(b"* OK [CAPABILITY IMAP4rev1 STARTTLS LOGINDISABLED] ready\r\n")
                .expect("greeting");
            let mut clear = Vec::new();
            let mut reader = BufReader::new(tcp.try_clone().expect("a second handle"));
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return (clear, String::new());
            }
            let asked = line.trim_end().to_owned();
            let tag = asked.split(' ').next().unwrap_or_default().to_owned();
            clear.push(asked);
            if !offers {
                let _ = write!(tcp, "{tag} BAD unknown command\r\n");
                // Whatever a client that was refused still says in the clear.
                line.clear();
                while reader.read_line(&mut line).unwrap_or(0) > 0 {
                    clear.push(line.trim_end().to_owned());
                    line.clear();
                }
                return (clear, String::new());
            }
            write!(tcp, "{tag} OK begin TLS negotiation now\r\n").expect("upgrade answer");
            // The client waits for that answer before its handshake, so
            // nothing of it is left in the cleartext reader.
            drop(reader);
            let connection = rustls::ServerConnection::new(Arc::new(server)).expect("TLS server");
            let mut secure = BufReader::new(rustls::StreamOwned::new(connection, tcp));
            let mut login = String::new();
            if secure.read_line(&mut login).unwrap_or(0) == 0 {
                return (clear, login);
            }
            let tag = login.split(' ').next().unwrap_or_default().to_owned();
            let stream = secure.get_mut();
            write!(stream, "{tag} OK LOGIN completed\r\n").expect("login response");
            stream.flush().expect("login flush");
            let mut logout = String::new();
            if secure.read_line(&mut logout).unwrap_or(0) > 0 {
                let tag = logout.split(' ').next().unwrap_or_default().to_owned();
                let stream = secure.get_mut();
                let _ = write!(stream, "* BYE logging out\r\n{tag} OK LOGOUT completed\r\n");
                let _ = stream.flush();
            }
            (clear, login)
        });
        (port, worker)
    }

    /// How a test SMTP server answers: whether it offers the upgrade, the
    /// sign-ins it offers once secure, and whether it takes the credential.
    #[derive(Clone, Copy)]
    pub(crate) struct Smtp {
        upgrades: bool,
        mechanisms: &'static str,
        accepts: bool,
    }

    impl Smtp {
        pub(crate) const ACCEPTS: Self = Self {
            upgrades: true,
            mechanisms: "PLAIN LOGIN",
            accepts: true,
        };
    }

    /// Every line an SMTP server was sent: in the clear, and once secure.
    pub(crate) struct SmtpSaid {
        clear: Vec<String>,
        secure: Vec<String>,
    }

    /// An SMTP server on a port that starts in the clear, as a submission
    /// port does, with the identity `server`.
    pub(crate) fn smtp_server(
        server: rustls::ServerConfig,
        serving: Smtp,
    ) -> (u16, std::thread::JoinHandle<SmtpSaid>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let port = listener.local_addr().expect("listener address").port();
        let worker = std::thread::spawn(move || {
            let mut said = SmtpSaid {
                clear: Vec::new(),
                secure: Vec::new(),
            };
            let (mut tcp, _) = listener.accept().expect("client");
            tcp.write_all(b"220 localhost ESMTP ready\r\n")
                .expect("greeting");
            let mut reader = BufReader::new(tcp.try_clone().expect("a second handle"));
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return said;
                }
                let asked = line.trim_end().to_owned();
                let upper = asked.to_ascii_uppercase();
                said.clear.push(asked);
                if upper.starts_with("EHLO") {
                    let offer = if serving.upgrades {
                        "250-STARTTLS\r\n"
                    } else {
                        ""
                    };
                    let _ = write!(tcp, "250-localhost\r\n{offer}250 8BITMIME\r\n");
                } else if upper.starts_with("STARTTLS") && serving.upgrades {
                    let _ = write!(tcp, "220 2.0.0 ready to start TLS\r\n");
                    break;
                } else if upper.starts_with("QUIT") {
                    let _ = write!(tcp, "221 2.0.0 bye\r\n");
                    return said;
                } else {
                    let _ = write!(tcp, "502 5.5.1 not here\r\n");
                }
            }
            // The client waits for that answer before its handshake, so
            // nothing of it is left in the cleartext reader.
            drop(reader);
            let connection = rustls::ServerConnection::new(Arc::new(server)).expect("TLS server");
            let mut secure = BufReader::new(rustls::StreamOwned::new(connection, tcp));
            let verdict = if serving.accepts {
                "235 2.7.0 authentication successful\r\n"
            } else {
                "535 5.7.8 authentication credentials invalid\r\n"
            };
            loop {
                line.clear();
                if secure.read_line(&mut line).unwrap_or(0) == 0 {
                    return said;
                }
                let asked = line.trim_end().to_owned();
                let upper = asked.to_ascii_uppercase();
                said.secure.push(asked);
                let reply = if upper.starts_with("EHLO") {
                    format!(
                        "250-localhost\r\n250-AUTH {}\r\n250 8BITMIME\r\n",
                        serving.mechanisms
                    )
                } else if upper.starts_with("AUTH PLAIN") {
                    verdict.to_owned()
                } else if upper.starts_with("AUTH LOGIN") {
                    // The name, then the password, each asked for.
                    for prompt in ["334 VXNlcm5hbWU6\r\n", "334 UGFzc3dvcmQ6\r\n"] {
                        let stream = secure.get_mut();
                        let _ = stream.write_all(prompt.as_bytes());
                        let _ = stream.flush();
                        line.clear();
                        if secure.read_line(&mut line).unwrap_or(0) == 0 {
                            return said;
                        }
                        said.secure.push(line.trim_end().to_owned());
                    }
                    verdict.to_owned()
                } else if upper.starts_with("QUIT") {
                    "221 2.0.0 bye\r\n".to_owned()
                } else {
                    "502 5.5.1 not here\r\n".to_owned()
                };
                let stream = secure.get_mut();
                let _ = stream.write_all(reply.as_bytes());
                let _ = stream.flush();
                if upper.starts_with("QUIT") {
                    return said;
                }
            }
        });
        (port, worker)
    }

    /// A port of this machine nothing listens on.
    pub(crate) fn closed_port() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        listener.local_addr().expect("listener address").port()
    }

    fn email_account(port: u16, smtp_port: u16) -> accounts::Account {
        let mut account = accounts::Account::new("mail", "email");
        account.config = serde_json::json!({
            "email": "ada@example.test",
            "username": "ada@example.test",
            "imap_host": "localhost",
            "imap_port": port,
            "smtp_host": "localhost",
            "smtp_port": smtp_port
        });
        account
    }

    tilo_test_env::scenario! {
        fn account_probes_sign_in_to_real_servers_and_refuse_the_rest;

        phase email_probe_authenticates_and_refuses_untrusted_servers;
        // The IMAP probe signs in over verified TLS, reports rejected credentials,
        // refuses bad greetings and untrusted certificates, and never sends a
        // login carrying control characters.
        {
            // (case, greeting, LOGIN reply, password, trust the server certificate,
            //  expected result, substrings the server must see in LOGIN (None:
            //  nothing may be sent))
            type ImapCase = (
                &'static str,
                &'static str,
                &'static str,
                &'static str,
                bool,
                Result<String, String>,
                Option<&'static [&'static str]>,
            );
            let served: [ImapCase; 4] = [
                (
                    "tls sign-in succeeds",
                    "* OK [CAPABILITY IMAP4rev1] ready\r\n",
                    "TAG OK LOGIN completed\r\n",
                    "app-password",
                    true,
                    Ok("IMAP and SMTP sign-in succeeded".into()),
                    Some(&[" LOGIN ", "\"ada@example.test\"", "\"app-password\""]),
                ),
                (
                    "rejected credential",
                    "* OK ready\r\n",
                    "TAG NO [AUTHENTICATIONFAILED] Invalid credentials\r\n",
                    "wrong",
                    true,
                    Err("IMAP server rejected the saved credential".into()),
                    Some(&[]),
                ),
                (
                    "non-OK greeting sends no credential",
                    "* BYE too busy\r\n",
                    "",
                    "app-password",
                    true,
                    Err("server did not provide a valid IMAP greeting".into()),
                    None,
                ),
                (
                    "untrusted certificate",
                    "* OK ready\r\n",
                    "",
                    "app-password",
                    false,
                    Err("could not establish the secure IMAP connection: the server's certificate is not trusted".into()),
                    Some(&[]),
                ),
            ];
            for (case, greeting, reply, password, trusted, want, login_must_contain) in served {
                let (tls, roots) = server_tls();
                let (port, worker) = imap_server(tls.clone(), greeting, reply);
                // Only a sign-in the IMAP server took goes on to the SMTP
                // server.
                let smtp = want.is_ok().then(|| smtp_server(tls, Smtp::ACCEPTS));
                let smtp_port = smtp.as_ref().map_or(9, |(port, _)| *port);
                let roots = if trusted {
                    roots
                } else {
                    rustls::RootCertStore::empty()
                };
                assert_eq!(
                    LiveAccountTester::email_with_roots(
                        &email_account(port, smtp_port),
                        &accounts::Secret::password(password),
                        roots,
                    ),
                    want,
                    "case {case}"
                );
                if let Some((_, smtp)) = smtp {
                    let said = smtp.join().expect("SMTP worker");
                    assert!(
                        said.secure.iter().any(|line| line.starts_with("AUTH PLAIN ")),
                        "case {case}: {:?}",
                        said.secure
                    );
                }
                if !trusted {
                    // The handshake fails on the server side too; its worker is not joined.
                    drop(worker);
                    continue;
                }
                let login = worker.join().expect("server worker");
                match login_must_contain {
                    Some(parts) => {
                        for part in parts {
                            assert!(login.contains(part), "case {case}: {login}");
                        }
                    }
                    None => assert!(
                        login.is_empty(),
                        "case {case}: no credential after a refused greeting"
                    ),
                }
            }

            assert_eq!(
                LiveAccountTester::email_with_roots(
                    &email_account(9, 9),
                    &accounts::Secret::password("pass\r\nA1 DELETE INBOX"),
                    rustls::RootCertStore::empty(),
                ),
                Err("login contains unsupported control characters".into())
            );
        }

        phase a_port_that_starts_in_the_clear_is_upgraded_before_the_sign_in;
        // On a port that starts in the clear the probe asks for STARTTLS
        // first: the credential goes over TLS, and to a server that will
        // not upgrade it does not go at all.
        {
            for offers in [true, false] {
                let (tls, roots) = server_tls();
                let (port, worker) = imap_starttls_server(tls.clone(), offers);
                // Only a sign-in the IMAP server took goes on to the SMTP
                // server.
                let smtp = offers.then(|| smtp_server(tls, Smtp::ACCEPTS));
                let smtp_port = smtp.as_ref().map_or(9, |(port, _)| *port);
                let mut account = email_account(port, smtp_port);
                account.config["imap_tls"] = serde_json::json!(false);
                let result = LiveAccountTester::email_with_roots(
                    &account,
                    &accounts::Secret::password("app-password"),
                    roots,
                );
                let (clear, login) = worker.join().expect("server worker");
                if let Some((_, smtp)) = smtp {
                    smtp.join().expect("SMTP worker");
                }
                assert!(
                    clear.iter().any(|line| line.ends_with(" STARTTLS")),
                    "the upgrade is asked for: {clear:?}"
                );
                assert!(
                    clear
                        .iter()
                        .all(|line| !line.contains("app-password") && !line.contains("LOGIN")),
                    "nothing signs in in the clear: {clear:?}"
                );
                if offers {
                    assert_eq!(result, Ok("IMAP and SMTP sign-in succeeded".into()));
                    assert!(login.contains("\"app-password\""), "{login}");
                } else {
                    assert_eq!(
                        result,
                        Err("IMAP server does not offer STARTTLS on this port".into())
                    );
                    assert!(login.is_empty(), "{login}");
                }
            }
        }

        phase the_smtp_sign_in_is_proved_with_the_imap_one;
        // An account that reads mail and cannot send any is refused when it
        // is added. The probe signs in to the SMTP server too, only once
        // that connection is secure, and sends no message.
        {
            use base64::Engine as _;

            let accepting = || {
                let (tls, roots) = server_tls();
                let (port, imap) =
                    imap_server(tls.clone(), "* OK ready\r\n", "TAG OK LOGIN completed\r\n");
                (tls, roots, port, imap)
            };
            let cases: [(&str, Smtp, Result<String, String>); 4] = [
                (
                    "AUTH PLAIN",
                    Smtp::ACCEPTS,
                    Ok("IMAP and SMTP sign-in succeeded".into()),
                ),
                (
                    "AUTH LOGIN alone",
                    Smtp {
                        mechanisms: "LOGIN",
                        ..Smtp::ACCEPTS
                    },
                    Ok("IMAP and SMTP sign-in succeeded".into()),
                ),
                (
                    "rejected credential",
                    Smtp {
                        accepts: false,
                        ..Smtp::ACCEPTS
                    },
                    Err("SMTP server rejected the saved credential".into()),
                ),
                (
                    "no upgrade offered",
                    Smtp {
                        upgrades: false,
                        ..Smtp::ACCEPTS
                    },
                    Err("SMTP server does not offer STARTTLS on this port".into()),
                ),
            ];
            for (case, serving, want) in cases {
                let (tls, roots, port, imap) = accepting();
                let (smtp_port, smtp) = smtp_server(tls, serving);
                assert_eq!(
                    LiveAccountTester::email_with_roots(
                        &email_account(port, smtp_port),
                        &accounts::Secret::password("app-password"),
                        roots,
                    ),
                    want,
                    "case {case}"
                );
                imap.join().expect("IMAP worker");
                let said = smtp.join().expect("SMTP worker");
                let signs_in = |line: &String| line.to_ascii_uppercase().starts_with("AUTH");
                assert!(
                    !said.clear.iter().any(signs_in),
                    "case {case}: nothing signs in in the clear: {:?}",
                    said.clear
                );
                assert_eq!(
                    said.secure.iter().any(signs_in),
                    serving.upgrades,
                    "case {case}: {:?}",
                    said.secure
                );
                assert!(
                    said.clear.iter().chain(&said.secure).all(|line| {
                        let line = line.to_ascii_uppercase();
                        !line.starts_with("MAIL") && !line.starts_with("RCPT") && line != "DATA"
                    }),
                    "case {case}: the probe sends no message"
                );
                if let Some(token) = said
                    .secure
                    .iter()
                    .find_map(|line| line.strip_prefix("AUTH PLAIN "))
                {
                    assert_eq!(
                        base64::engine::general_purpose::STANDARD
                            .decode(token)
                            .expect("a base64 token"),
                        b"\0ada@example.test\0app-password"
                    );
                }
            }

            // A server that is not there is said, with where it was looked for.
            let (_, roots, port, imap) = accepting();
            let closed = closed_port();
            let unreached = LiveAccountTester::email_with_roots(
                &email_account(port, closed),
                &accounts::Secret::password("app-password"),
                roots,
            )
            .expect_err("no SMTP server answers there");
            assert!(
                unreached.starts_with(&format!("could not reach localhost:{closed}")),
                "{unreached}"
            );
            imap.join().expect("IMAP worker");

            // An account that names no SMTP server is refused before a
            // credential goes anywhere.
            let mut account = email_account(9, 9);
            account
                .config
                .as_object_mut()
                .expect("an account's settings")
                .remove("smtp_host");
            assert_eq!(
                LiveAccountTester::email_with_roots(
                    &account,
                    &accounts::Secret::password("app-password"),
                    rustls::RootCertStore::empty(),
                ),
                Err("account has no SMTP server".into())
            );
        }

        phase a_calendar_account_without_selected_collections_is_not_connected;
        {
            let server = Responder::silent();
            let mut account = accounts::Account::new("empty-calendar", "caldav");
            account.config = serde_json::json!({
                "server_url": format!("http://localhost:{}/dav/", server.port()),
                "username": "ada", "calendars": []
            });
            assert!(
                LiveAccountTester
                    .test(&account, &accounts::Secret::password("secret"))
                    .unwrap_err()
                    .contains("Choose calendars")
            );
            server.assert_untouched();
        }

        phase dav_probe_authenticates_and_never_replays_credentials;
        // The DAV probe signs in with PROPFIND, reports rejection, and never
        // replays authorization to a redirect target.
        {
            let server = Responder::start(vec![Reply::status(207).body(
                r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/dav/</d:href></d:response></d:multistatus>"#,
            )]);
            let account = dav_account(&format!("http://localhost:{}/dav/", server.port()));
            assert_eq!(
                LiveAccountTester::dav(None, &account, &accounts::Secret::password("secret")),
                Ok("Server accepted the signed-in DAV request".into())
            );
            let request = server.next_request();
            assert_eq!(request.method, "PROPFIND");
            assert_eq!(request.header("depth"), Some("0"));
            assert!(request.body_text().contains("<D:resourcetype/>"));

            let server = Responder::start(vec![Reply::status(401)]);
            let account = dav_account(&format!("http://localhost:{}/dav/", server.port()));
            assert_eq!(
                LiveAccountTester::dav(None, &account, &accounts::Secret::password("wrong")),
                Err("server rejected the saved credential".into())
            );

            // A redirect is reported, not followed with the credential.
            let target = Responder::silent();
            let target_url = format!("http://localhost:{}/stolen", target.port());
            let redirect = Responder::start(vec![Reply::status(302).header("Location", &target_url)]);
            let account = dav_account(&format!("http://localhost:{}/dav", redirect.port()));
            let result = LiveAccountTester::dav(None, &account, &accounts::Secret::password("secret"));
            assert_eq!(result, Err("server returned HTTP 302".into()));
            let first_request = redirect.next_request();
            assert!(
                first_request
                    .header("authorization")
                    .is_some_and(|value| value.starts_with("Basic ")),
                "the configured endpoint itself is authenticated"
            );
            target.assert_untouched();
        }
    }

    fn dav_account(url: &str) -> accounts::Account {
        let mut account = accounts::Account::new("dav", "webdav");
        account.config = serde_json::json!({"base_url": url, "username": "ada"});
        account
    }
}
