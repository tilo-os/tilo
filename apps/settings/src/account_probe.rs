//! Bounded, injectable account connection probes used by resource actions.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

pub trait AccountTester: Send + Sync {
    fn test(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<String, String>;
}

#[derive(Default)]
pub struct LiveAccountTester;

impl LiveAccountTester {
    const NETWORK_TIMEOUT: Duration = Duration::from_secs(8);

    fn connect(host: &str, port: u16) -> Result<TcpStream, String> {
        // The platform resolver is blocking, so isolate it behind a deadline
        // as well; connect/read/write timeouts alone do not bound DNS stalls.
        let host_owned = host.to_string();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let resolved = (host_owned.as_str(), port)
                .to_socket_addrs()
                .map(|iter| iter.collect::<Vec<_>>())
                .map_err(|_| "server name could not be resolved".to_string());
            let _ = sender.send(resolved);
        });
        let addresses = receiver
            .recv_timeout(Self::NETWORK_TIMEOUT)
            .map_err(|_| "server name resolution timed out".to_string())??;
        if addresses.is_empty() {
            return Err("server name returned no address".into());
        }
        let deadline = std::time::Instant::now() + Self::NETWORK_TIMEOUT;
        let mut last_error = None;
        for address in addresses {
            let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
                break;
            };
            match TcpStream::connect_timeout(&address, remaining) {
                Ok(stream) => {
                    stream
                        .set_read_timeout(Some(Self::NETWORK_TIMEOUT))
                        .map_err(|_| "could not configure the connection timeout".to_string())?;
                    stream
                        .set_write_timeout(Some(Self::NETWORK_TIMEOUT))
                        .map_err(|_| "could not configure the connection timeout".to_string())?;
                    return Ok(stream);
                }
                Err(error) => last_error = Some(error),
            }
        }
        Err(format!(
            "could not reach {host}:{port}: {}",
            last_error.map_or_else(|| "connection timed out".into(), |error| error.to_string())
        ))
    }

    fn read_imap_response(
        stream: &mut impl Read,
        tagged: Option<&str>,
        deadline: std::time::Instant,
    ) -> Result<String, String> {
        const MAX_RESPONSE: usize = 16 * 1024;
        let mut response = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            if std::time::Instant::now() >= deadline {
                return Err("IMAP server response timed out".into());
            }
            let read = stream
                .read(&mut chunk)
                .map_err(|_| "IMAP server did not answer before the timeout".to_string())?;
            if read == 0 {
                return Err("IMAP server closed the connection unexpectedly".into());
            }
            response.extend_from_slice(&chunk[..read]);
            if response.len() > MAX_RESPONSE {
                return Err("IMAP server response exceeded the safety limit".into());
            }
            let text = String::from_utf8_lossy(&response);
            let complete = match tagged {
                Some(tag) => text.split_inclusive('\n').any(|line| {
                    line.ends_with('\n')
                        && line.starts_with(tag)
                        && line.as_bytes().get(tag.len()) == Some(&b' ')
                }),
                None => response.windows(2).any(|window| window == b"\r\n"),
            };
            if complete {
                return Ok(text.into_owned());
            }
        }
    }

    fn imap_quote(value: &str) -> Result<String, String> {
        if value.chars().any(char::is_control) {
            return Err("login contains unsupported control characters".into());
        }
        Ok(value.replace('\\', "\\\\").replace('"', "\\\""))
    }

    /// Complete TLS, read the greeting, authenticate, and log out. A TCP
    /// accept alone is deliberately not success: it says nothing about the
    /// saved credential or whether this is an IMAP endpoint.
    fn email(account: &accounts::Account, secret: &accounts::Secret) -> Result<String, String> {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        Self::email_with_roots(account, secret, roots)
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

        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| "could not initialize secure protocol versions".to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
            .map_err(|_| "invalid IMAP server name".to_string())?;
        let connection = rustls::ClientConnection::new(Arc::new(config), server_name)
            .map_err(|_| "could not initialize the secure IMAP connection".to_string())?;
        let tcp = Self::connect(host, port)?;
        let mut stream = rustls::StreamOwned::new(connection, tcp);

        let greeting = Self::read_imap_response(
            &mut stream,
            None,
            std::time::Instant::now() + Self::NETWORK_TIMEOUT,
        )?;
        if !greeting.to_ascii_uppercase().starts_with("* OK") {
            return Err("server did not provide a valid IMAP greeting".into());
        }
        let user = Self::imap_quote(username.trim())?;
        let password = Self::imap_quote(secret.password.trim())?;
        write!(stream, "qos1 LOGIN \"{user}\" \"{password}\"\r\n")
            .map_err(|_| "could not send the IMAP login".to_string())?;
        stream
            .flush()
            .map_err(|_| "could not send the IMAP login".to_string())?;
        let login = Self::read_imap_response(
            &mut stream,
            Some("qos1"),
            std::time::Instant::now() + Self::NETWORK_TIMEOUT,
        )?;
        let accepted = login.lines().any(|line| {
            line.get(..7)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("qos1 OK"))
        });
        if !accepted {
            return Err("IMAP server rejected the saved credential".into());
        }
        let _ = stream.write_all(b"qos2 LOGOUT\r\n");
        Ok("IMAP sign-in succeeded".into())
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
        let parsed = reqwest::Url::parse(url).map_err(|_| "account has an invalid server URL")?;
        let localhost_http = cfg!(debug_assertions)
            && parsed.scheme() == "http"
            && parsed.host_str() == Some("localhost");
        if parsed.scheme() != "https" && !localhost_http {
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
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Self::NETWORK_TIMEOUT)
            .timeout(Self::NETWORK_TIMEOUT)
            // Never replay Basic credentials to a redirect target. Discovery
            // belongs in setup; a stored endpoint must answer at its exact
            // origin.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "could not initialize the secure HTTP client".to_string())?;
        let method = reqwest::Method::from_bytes(b"PROPFIND")
            .map_err(|_| "could not construct DAV probe".to_string())?;
        let response = client
            .request(method, parsed)
            .basic_auth(username, Some(&secret.password))
            .header("Depth", "0")
            .body("<?xml version=\"1.0\"?><propfind xmlns=\"DAV:\"><prop><resourcetype/></prop></propfind>")
            .send()
            .map_err(|error| format!("connection failed: {error}"))?;
        if response.status().is_success() || response.status().as_u16() == 207 {
            Ok("Server accepted the signed-in DAV request".into())
        } else if response.status() == reqwest::StatusCode::UNAUTHORIZED {
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
    fn test(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<String, String> {
        match account.type_id.as_str() {
            "email" => Self::email(account, secret),
            "caldav" | "webdav" | "nextcloud" => Self::dav(account, secret),
            _ => Err("unsupported account type".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::sync::mpsc;

    #[test]
    fn imap_response_requires_the_expected_tag() {
        let mut response = &b"* CAPABILITY IMAP4rev1\r\nqos1 OK logged in\r\n"[..];
        let parsed = LiveAccountTester::read_imap_response(
            &mut response,
            Some("qos1"),
            std::time::Instant::now() + Duration::from_secs(1),
        )
        .expect("tagged completion");
        assert!(parsed.contains("qos1 OK"));
    }

    #[test]
    fn email_probe_completes_tls_and_authenticates() {
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
            stream
                .write_all(b"* OK [CAPABILITY IMAP4rev1] ready\r\n")
                .expect("greeting");
            stream.flush().expect("greeting flush");
            let mut login = String::new();
            {
                let mut reader = BufReader::new(&mut stream);
                reader.read_line(&mut login).expect("login command");
            }
            assert!(login.starts_with("qos1 LOGIN "));
            assert!(login.contains("\"ada@example.test\""));
            assert!(login.contains("\"app-password\""));
            stream
                .write_all(b"qos1 OK LOGIN completed\r\n")
                .expect("login response");
            stream.flush().expect("login flush");
        });

        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert_der).expect("trust test certificate");
        let mut account = accounts::Account::new("mail", "email");
        account.config = serde_json::json!({
            "email": "ada@example.test",
            "username": "ada@example.test",
            "imap_host": "localhost",
            "imap_port": port
        });
        assert_eq!(
            LiveAccountTester::email_with_roots(
                &account,
                &accounts::Secret::password("app-password"),
                roots,
            ),
            Ok("IMAP sign-in succeeded".into())
        );
        worker.join().expect("server worker");
    }

    #[test]
    fn dav_redirect_never_replays_authorization() {
        let target = TcpListener::bind("127.0.0.1:0").expect("target listener");
        let target_url = format!(
            "http://localhost:{}/stolen",
            target.local_addr().expect("target addr").port()
        );
        let redirect = TcpListener::bind("127.0.0.1:0").expect("redirect listener");
        let redirect_url = format!(
            "http://localhost:{}/dav",
            redirect.local_addr().expect("redirect addr").port()
        );
        let (sent, received) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut socket, _) = redirect.accept().expect("redirect request");
            let mut reader = BufReader::new(socket.try_clone().expect("clone"));
            let mut request = String::new();
            while reader.read_line(&mut request).is_ok_and(|count| count > 0)
                && !request.ends_with("\r\n\r\n")
            {}
            write!(
                socket,
                "HTTP/1.1 302 Found\r\nLocation: {target_url}\r\nContent-Length: 0\r\n\r\n"
            )
            .expect("redirect response");
            sent.send(request).expect("request capture");
        });

        let mut account = accounts::Account::new("dav", "webdav");
        account.config = serde_json::json!({"base_url": redirect_url, "username": "ada"});
        let result = LiveAccountTester::dav(&account, &accounts::Secret::password("secret"));
        assert!(
            result.is_err(),
            "a redirect is not accepted as authenticated DAV"
        );
        let first_request = received
            .recv_timeout(Duration::from_secs(2))
            .expect("request");
        assert!(
            first_request
                .to_ascii_lowercase()
                .contains("authorization: basic")
        );
        target.set_nonblocking(true).expect("nonblocking target");
        assert!(
            target.accept().is_err(),
            "redirect target must receive no request at all"
        );
    }
}
