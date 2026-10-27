//! Bounded, injectable account connection probes used by resource actions.
//!
//! The probes speak the protocols through the same clients the apps use:
//! `async-imap` (as Mail does) for IMAP and the shared `tilo-dav` client for
//! `CalDAV`/`WebDAV`. Both verify servers against Tilo's trust store.

use std::sync::Arc;
use ui::time::Duration;

use tilo_dav::{Credentials, DavClient, Depth, StatusCode, props};
use tokio::net::TcpStream;
use tokio::time::timeout;

pub trait AccountTester: Send + Sync {
    /// Resolve real calendars during setup. Test doubles must opt in explicitly.
    fn discover_calendars(
        &self,
        _account: &accounts::Account,
        _secret: &accounts::Secret,
    ) -> Result<Vec<tilo_dav::discovery::CalendarCollection>, String> {
        Err("Calendar discovery is unavailable".into())
    }

    fn test(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<String, String>;
}

#[derive(Default)]
pub struct LiveAccountTester;

impl LiveAccountTester {
    /// Deadline for each network step: resolving, connecting, the TLS
    /// handshake and greeting, and the login round-trip.
    const NETWORK_TIMEOUT: Duration = Duration::from_secs(8);

    async fn connect(host: &str, port: u16) -> Result<TcpStream, String> {
        // The resolver is bounded by the same deadline as the connection;
        // connect timeouts alone do not bound DNS stalls.
        let addresses: Vec<_> =
            timeout(Self::NETWORK_TIMEOUT, tokio::net::lookup_host((host, port)))
                .await
                .map_err(|_| "server name resolution timed out".to_string())?
                .map_err(|_| "server name could not be resolved".to_string())?
                .collect();
        if addresses.is_empty() {
            return Err("server name returned no address".into());
        }
        let deadline = ui::time::now() + Self::NETWORK_TIMEOUT;
        let mut last_error = None;
        for address in addresses {
            let Some(remaining) = deadline.checked_duration_since(ui::time::now()) else {
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

    fn imap_login_name(value: &str) -> Result<&str, String> {
        if value.chars().any(char::is_control) {
            return Err("login contains unsupported control characters".into());
        }
        Ok(value)
    }

    /// Complete TLS, read the greeting, authenticate, and log out. A TCP
    /// accept alone is deliberately not success: it says nothing about the
    /// saved credential or whether this is an IMAP endpoint.
    fn email(account: &accounts::Account, secret: &accounts::Secret) -> Result<String, String> {
        Self::email_with_roots(account, secret, tilo_net::system_roots())
    }

    fn email_with_roots(
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

        let config = tilo_net::client_config_with_roots(roots)
            .map_err(|_| "could not initialize secure protocol versions".to_string())?;
        let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
            .map_err(|_| "invalid IMAP server name".to_string())?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "could not start the IMAP probe".to_string())?;
        runtime.block_on(async {
            let tcp = Self::connect(host, port).await?;
            let tls = timeout(
                Self::NETWORK_TIMEOUT,
                tokio_rustls::TlsConnector::from(Arc::new(config)).connect(server_name, tcp),
            )
            .await
            .map_err(|_| "IMAP server did not answer before the timeout".to_string())?
            .map_err(|_| "could not establish the secure IMAP connection".to_string())?;
            let mut client = async_imap::Client::new(tls);
            let greeting = timeout(Self::NETWORK_TIMEOUT, client.read_response())
                .await
                .map_err(|_| "IMAP server response timed out".to_string())?
                .ok_or_else(|| "IMAP server closed the connection unexpectedly".to_string())?
                .map_err(|_| "server did not provide a valid IMAP greeting".to_string())?;
            if !matches!(
                greeting.parsed(),
                async_imap::imap_proto::Response::Data {
                    status: async_imap::imap_proto::Status::Ok,
                    ..
                }
            ) {
                return Err("server did not provide a valid IMAP greeting".into());
            }
            let mut session = timeout(Self::NETWORK_TIMEOUT, client.login(user, password))
                .await
                .map_err(|_| "IMAP server did not answer before the timeout".to_string())?
                .map_err(|(error, _)| match error {
                    async_imap::error::Error::No(_) | async_imap::error::Error::Bad(_) => {
                        "IMAP server rejected the saved credential".to_string()
                    }
                    async_imap::error::Error::Validate(_) => {
                        "login contains unsupported control characters".to_string()
                    }
                    _ => "IMAP server closed the connection unexpectedly".to_string(),
                })?;
            let _ = timeout(Self::NETWORK_TIMEOUT, session.logout()).await;
            Ok("IMAP sign-in succeeded".into())
        })
    }

    fn dav(account: &accounts::Account, secret: &accounts::Secret) -> Result<String, String> {
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
        if parsed.scheme() != "https" && !local_http_allowed(&parsed) {
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
        })
        .map_err(|_| "could not initialize the secure HTTP client".to_string())?
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
        let url = account
            .config
            .get("server_url")
            .and_then(serde_json::Value::as_str)
            .ok_or("account has no server URL")?;
        let url = tilo_dav::Url::parse(url).map_err(|_| "invalid server URL")?;
        if url.scheme() != "https" && !local_http_allowed(&url) {
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
            ..tilo_dav::Options::default()
        })
        .map_err(|_| "could not initialize the secure HTTP client")?
        .with_credentials(Credentials::new(username, secret.password.clone()));
        tilo_dav::discovery::calendars(&client, &url)
    }

    fn test(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<String, String> {
        match account.type_id.as_str() {
            "email" => Self::email(account, secret),
            "caldav" => {
                let configured = account
                    .config
                    .get("calendars")
                    .and_then(serde_json::Value::as_array)
                    .filter(|calendars| !calendars.is_empty())
                    .ok_or("Choose calendars in Edit account before checking this connection")?;
                let discovered = self.discover_calendars(account, secret)?;
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
            "webdav" | "nextcloud" => Self::dav(account, secret),
            _ => Err("unsupported account type".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use test_http::{Reply, Responder};

    /// A one-connection TLS IMAP server. `login_reply` answers the LOGIN
    /// command with the command's own tag substituted for `TAG`.
    fn imap_server(
        greeting: &'static str,
        login_reply: &'static str,
    ) -> (u16, rustls::RootCertStore, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let port = listener.local_addr().expect("listener address").port();
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
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert_der).expect("trust test certificate");
        (port, roots, worker)
    }

    fn email_account(port: u16) -> accounts::Account {
        let mut account = accounts::Account::new("mail", "email");
        account.config = serde_json::json!({
            "email": "ada@example.test",
            "username": "ada@example.test",
            "imap_host": "localhost",
            "imap_port": port
        });
        account
    }

    /// The IMAP probe signs in over verified TLS, reports rejected credentials,
    /// refuses bad greetings and untrusted certificates, and never sends a
    /// login carrying control characters.
    #[test]
    fn email_probe_authenticates_and_refuses_untrusted_servers() {
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
                Ok("IMAP sign-in succeeded".into()),
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
                Err("could not establish the secure IMAP connection".into()),
                Some(&[]),
            ),
        ];
        for (case, greeting, reply, password, trusted, want, login_must_contain) in served {
            let (port, roots, worker) = imap_server(
                // The server fixture takes `'static` strings; the table is static.
                greeting, reply,
            );
            let roots = if trusted {
                roots
            } else {
                rustls::RootCertStore::empty()
            };
            assert_eq!(
                LiveAccountTester::email_with_roots(
                    &email_account(port),
                    &accounts::Secret::password(password),
                    roots,
                ),
                want,
                "case {case}"
            );
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

        // Control characters are refused before any connection is attempted.
        assert_eq!(
            LiveAccountTester::email_with_roots(
                &email_account(9),
                &accounts::Secret::password("pass\r\nA1 DELETE INBOX"),
                rustls::RootCertStore::empty(),
            ),
            Err("login contains unsupported control characters".into())
        );
    }

    #[test]
    fn a_calendar_account_without_selected_collections_is_not_connected() {
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

    fn dav_account(url: &str) -> accounts::Account {
        let mut account = accounts::Account::new("dav", "webdav");
        account.config = serde_json::json!({"base_url": url, "username": "ada"});
        account
    }

    /// The DAV probe signs in with PROPFIND, reports rejection, and never
    /// replays authorization to a redirect target.
    #[test]
    fn dav_probe_authenticates_and_never_replays_credentials() {
        let server = Responder::start(vec![Reply::status(207).body(
            r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/dav/</d:href></d:response></d:multistatus>"#,
        )]);
        let account = dav_account(&format!("http://localhost:{}/dav/", server.port()));
        assert_eq!(
            LiveAccountTester::dav(&account, &accounts::Secret::password("secret")),
            Ok("Server accepted the signed-in DAV request".into())
        );
        let request = server.next_request();
        assert_eq!(request.method, "PROPFIND");
        assert_eq!(request.header("depth"), Some("0"));
        assert!(request.body_text().contains("<D:resourcetype/>"));

        let server = Responder::start(vec![Reply::status(401)]);
        let account = dav_account(&format!("http://localhost:{}/dav/", server.port()));
        assert_eq!(
            LiveAccountTester::dav(&account, &accounts::Secret::password("wrong")),
            Err("server rejected the saved credential".into())
        );

        // A redirect is reported, not followed with the credential.
        let target = Responder::silent();
        let target_url = format!("http://localhost:{}/stolen", target.port());
        let redirect = Responder::start(vec![Reply::status(302).header("Location", &target_url)]);
        let account = dav_account(&format!("http://localhost:{}/dav", redirect.port()));
        let result = LiveAccountTester::dav(&account, &accounts::Secret::password("secret"));
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

/// Whether plain HTTP to `url` is allowed: only in a development build, and
/// only to this machine, named `localhost` or by a loopback address. A
/// loopback address reaches exactly the listener bound there, where the
/// name may resolve to `[::1]` first.
pub(crate) fn local_http_allowed(url: &tilo_dav::Url) -> bool {
    let local = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    cfg!(debug_assertions) && url.scheme() == "http" && local
}
