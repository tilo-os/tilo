//! Servers for tests of anything that talks to mail servers through this crate's engine: the
//! IMAP server laid out like the servers people use (`fake_imap`), a STARTTLS SMTP server that
//! keeps what it is handed, and the throwaway TLS both present.
//!
//! Compiled for this crate's tests and, with the `fake-servers` feature, for other crates' tests
//! (the tilolang Mail origin's). Never in a release.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

pub use crate::fake_imap::{FakeImap, Layout, Stored};

/// The mock servers present a fresh self-signed certificate.
pub fn accepting_client_config() -> Arc<rustls::ClientConfig> {
    Arc::new(tilo_net::pinned_client_config(|_| true).unwrap())
}

/// A fresh self-signed cert + rustls server config for the mock servers.
pub fn server_tls_config() -> Arc<rustls::ServerConfig> {
    let cert =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
            .unwrap();
    let cert_der = cert.cert.der().clone();
    let key_der = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
    let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![cert_der],
        rustls::pki_types::PrivateKeyDer::Pkcs8(key_der),
    )
    .unwrap();
    Arc::new(cfg)
}

/// A scripted STARTTLS SMTP server — the path the engine drives for the
/// non-465 submission port (`Tls::Required`): a plaintext greeting +
/// EHLO advertising STARTTLS, the in-band TLS upgrade, then EHLO +
/// AUTH LOGIN + MAIL/RCPT/DATA over TLS, capturing the DATA payload.
/// The plaintext half of the test SMTP server: greeting, EHLO, and the
/// STARTTLS upgrade. Returns the raw socket once the client has been
/// told to start TLS, or `None` if the session ended first (QUIT, EOF,
/// or a write error).
async fn smtp_plaintext_until_starttls(tcp: TcpStream) -> Option<TcpStream> {
    let mut plain = BufReader::new(tcp);
    if plain
        .write_all(b"220 localhost ESMTP test\r\n")
        .await
        .is_err()
    {
        return None;
    }
    let _ = plain.flush().await;
    let mut line = String::new();
    loop {
        line.clear();
        match plain.read_line(&mut line).await {
            Ok(0) | Err(_) => return None,
            Ok(_) => {}
        }
        let up = line.trim_end().to_ascii_uppercase();
        if up.starts_with("EHLO") || up.starts_with("HELO") {
            let _ = plain
                .write_all(b"250-localhost\r\n250-STARTTLS\r\n250 8BITMIME\r\n")
                .await;
            let _ = plain.flush().await;
        } else if up.starts_with("STARTTLS") {
            let _ = plain.write_all(b"220 2.0.0 Ready to start TLS\r\n").await;
            let _ = plain.flush().await;
            break;
        } else if up.starts_with("QUIT") {
            let _ = plain.write_all(b"221 2.0.0 Bye\r\n").await;
            let _ = plain.flush().await;
            return None;
        } else {
            let _ = plain.write_all(b"250 2.0.0 Ok\r\n").await;
            let _ = plain.flush().await;
        }
    }

    // The client waits for the "220 Ready" before its ClientHello, so no
    // application bytes are buffered past STARTTLS: the raw socket can be
    // handed to the TLS acceptor cleanly.
    Some(plain.into_inner())
}

pub async fn serve_smtp(
    tls: Arc<rustls::ServerConfig>,
    tcp: TcpStream,
    captured: Arc<Mutex<Vec<u8>>>,
) {
    let Some(tcp) = smtp_plaintext_until_starttls(tcp).await else {
        return;
    };
    let acceptor = tokio_rustls::TlsAcceptor::from(tls);
    let Ok(stream) = acceptor.accept(tcp).await else {
        return;
    };
    let (rd, mut wr) = tokio::io::split(stream);
    let mut reader = BufReader::new(rd);

    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let up = line.trim_end().to_ascii_uppercase();
        if up.starts_with("EHLO") || up.starts_with("HELO") {
            let _ = wr
                .write_all(b"250-localhost\r\n250-AUTH LOGIN\r\n250 8BITMIME\r\n")
                .await;
        } else if up.starts_with("AUTH LOGIN") {
            // base64("Username:") then base64("Password:").
            let _ = wr.write_all(b"334 VXNlcm5hbWU6\r\n").await;
            let _ = wr.flush().await;
            line.clear();
            let _ = reader.read_line(&mut line).await;
            let _ = wr.write_all(b"334 UGFzc3dvcmQ6\r\n").await;
            let _ = wr.flush().await;
            line.clear();
            let _ = reader.read_line(&mut line).await;
            let _ = wr
                .write_all(b"235 2.7.0 Authentication successful\r\n")
                .await;
        } else if up.starts_with("MAIL FROM") {
            let _ = wr.write_all(b"250 2.1.0 Ok\r\n").await;
        } else if up.starts_with("RCPT TO") {
            let _ = wr.write_all(b"250 2.1.5 Ok\r\n").await;
        } else if up.starts_with("DATA") {
            let _ = wr
                .write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n")
                .await;
            let _ = wr.flush().await;
            let mut data = Vec::new();
            loop {
                let mut dline = Vec::new();
                match reader.read_until(b'\n', &mut dline).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if dline == b".\r\n" {
                    break;
                }
                data.extend_from_slice(&dline);
            }
            *captured.lock().unwrap() = data;
            let _ = wr.write_all(b"250 2.0.0 Ok: queued\r\n").await;
        } else if up.starts_with("QUIT") {
            let _ = wr.write_all(b"221 2.0.0 Bye\r\n").await;
            let _ = wr.flush().await;
            break;
        } else {
            let _ = wr.write_all(b"250 2.0.0 Ok\r\n").await;
        }
        let _ = wr.flush().await;
    }
}

/// A STARTTLS SMTP server on a loopback port that accepts every message and keeps what it was
/// handed, or refuses every recipient while [`FakeSmtp::refuse`] says why.
pub struct FakeSmtp {
    pub port: u16,
    sent: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl FakeSmtp {
    /// Start serving on the current tokio runtime, which must outlive the server.
    pub async fn start(tls: Arc<rustls::ServerConfig>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port");
        let port = listener.local_addr().expect("a bound port").port();
        let sent: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
        let kept = Arc::clone(&sent);
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let tls = Arc::clone(&tls);
                let kept = Arc::clone(&kept);
                tokio::spawn(async move {
                    let one: Arc<Mutex<Vec<u8>>> = Arc::default();
                    serve_smtp(tls, tcp, Arc::clone(&one)).await;
                    let data = std::mem::take(&mut *one.lock().expect("not poisoned"));
                    if !data.is_empty() {
                        kept.lock().expect("not poisoned").push(data);
                    }
                });
            }
        });
        Self { port, sent }
    }

    /// The messages it accepted, as their DATA.
    #[must_use]
    pub fn sent(&self) -> Vec<Vec<u8>> {
        self.sent.lock().expect("not poisoned").clone()
    }
}
