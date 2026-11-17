//! The sign-in to an account's SMTP server, proved when the account is
//! added: a person who can read their mail and not send any finds out here,
//! not at their first message.
//!
//! Port 465 is TLS from the first byte. Every other port starts in the clear
//! and has to upgrade with `STARTTLS` before anything signs in, as Mail's
//! own sending does. The probe greets the server, signs in with `AUTH PLAIN`
//! or `AUTH LOGIN`, and leaves: it sends no message.

use std::sync::Arc;

use base64::Engine as _;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::time::timeout;
use web_time::Duration;

/// The port SMTP submission is TLS from the first byte on.
const IMPLICIT_TLS_PORT: u16 = 465;
/// The longest line a reply may hold. The protocol's own bound is 512
/// bytes; a server that sends more than this is not one to sign in to.
const MAX_LINE: u64 = 4096;
/// The most lines one reply may run to.
const MAX_REPLY_LINES: usize = 64;

/// One reply of the server: its code, and what each of its lines says.
struct Reply {
    code: u16,
    lines: Vec<String>,
}

/// Sign in to the SMTP server at `host:port` as `user`, and leave. Each step
/// of it may take `step`.
///
/// # Errors
/// The server could not be reached, did not answer as an SMTP server, would
/// not upgrade a connection that has to, presented a certificate `tls` does
/// not trust, offers no password sign-in, or rejected the credential.
pub(crate) async fn sign_in(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    tls: Arc<rustls::ClientConfig>,
    step: Duration,
) -> Result<(), String> {
    let server_name = rustls::pki_types::ServerName::try_from(host.to_owned())
        .map_err(|_| "invalid SMTP server name".to_string())?;
    let connector = tokio_rustls::TlsConnector::from(tls);
    let tcp = crate::imap::reach(host, port, step).await?;
    let secured = |tcp| async {
        timeout(step, connector.connect(server_name.clone(), tcp))
            .await
            .map_err(|_| unanswered())?
            .map_err(|error| crate::imap::insecure("SMTP", &error))
    };
    let mut server = if port == IMPLICIT_TLS_PORT {
        let mut server = BufReader::new(secured(tcp).await?);
        greeted(&mut server, step).await?;
        server
    } else {
        let mut clear = BufReader::new(tcp);
        greeted(&mut clear, step).await?;
        let offered = hello(&mut clear, step).await?;
        let upgrades = offered
            .iter()
            .any(|line| line.eq_ignore_ascii_case("STARTTLS"));
        if !upgrades || ask(&mut clear, "STARTTLS", step).await?.code != 220 {
            return Err("SMTP server does not offer STARTTLS on this port".into());
        }
        // What the server, or anyone between, sent in the clear after its
        // answer is dropped with this reader's buffer.
        BufReader::new(secured(clear.into_inner()).await?)
    };
    // What a server offers is asked again once the connection is secure:
    // nothing it said in the clear is believed.
    let offered = hello(&mut server, step).await?;
    let offers = |mechanism: &str| {
        offered.iter().any(|line| {
            let mut words = line.split([' ', '=']);
            words
                .next()
                .is_some_and(|word| word.eq_ignore_ascii_case("AUTH"))
                && words.any(|word| word.eq_ignore_ascii_case(mechanism))
        })
    };
    let encoded = |text: &str| base64::engine::general_purpose::STANDARD.encode(text);
    let answer = if offers("PLAIN") {
        let token = encoded(&format!("\0{user}\0{password}"));
        ask(&mut server, &format!("AUTH PLAIN {token}"), step).await?
    } else if offers("LOGIN") {
        let mut answer = ask(&mut server, "AUTH LOGIN", step).await?;
        for part in [user, password] {
            if answer.code != 334 {
                break;
            }
            answer = ask(&mut server, &encoded(part), step).await?;
        }
        answer
    } else {
        let _ = ask(&mut server, "QUIT", step).await;
        return Err("SMTP server offers no password sign-in".into());
    };
    let _ = ask(&mut server, "QUIT", step).await;
    if answer.code == 235 {
        Ok(())
    } else {
        Err("SMTP server rejected the saved credential".into())
    }
}

fn unanswered() -> String {
    "SMTP server did not answer before the timeout".into()
}

/// Read the server's greeting, which has to say it is ready.
async fn greeted<S>(server: &mut BufReader<S>, step: Duration) -> Result<(), String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if reply(server, step).await?.code == 220 {
        Ok(())
    } else {
        Err("server did not provide a valid SMTP greeting".into())
    }
}

/// Say `EHLO`, answering what the server offers, a line each.
async fn hello<S>(server: &mut BufReader<S>, step: Duration) -> Result<Vec<String>, String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // A probe has no name of its own to give: the address literal is what a
    // client without one says.
    let answer = ask(server, "EHLO [127.0.0.1]", step).await?;
    if answer.code == 250 {
        Ok(answer.lines)
    } else {
        Err("server did not answer as an SMTP server".into())
    }
}

/// Send one command line and read its reply.
async fn ask<S>(server: &mut BufReader<S>, command: &str, step: Duration) -> Result<Reply, String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let sent = async {
        server.write_all(command.as_bytes()).await?;
        server.write_all(b"\r\n").await?;
        server.flush().await
    };
    timeout(step, sent)
        .await
        .map_err(|_| unanswered())?
        .map_err(|_| "SMTP server closed the connection unexpectedly".to_string())?;
    reply(server, step).await
}

/// Read one reply, every line of it.
async fn reply<S>(server: &mut BufReader<S>, step: Duration) -> Result<Reply, String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let malformed = || "server did not answer as an SMTP server".to_string();
    let mut lines = Vec::new();
    loop {
        let mut raw = Vec::new();
        let read = timeout(
            step,
            (&mut *server).take(MAX_LINE).read_until(b'\n', &mut raw),
        )
        .await
        .map_err(|_| unanswered())?
        .map_err(|_| "SMTP server closed the connection unexpectedly".to_string())?;
        if read == 0 {
            return Err("SMTP server closed the connection unexpectedly".into());
        }
        if !raw.ends_with(b"\n") || lines.len() >= MAX_REPLY_LINES {
            return Err(malformed());
        }
        let line = String::from_utf8_lossy(&raw);
        let line = line.trim_end();
        let code = line
            .get(..3)
            .and_then(|code| code.parse::<u16>().ok())
            .ok_or_else(malformed)?;
        let rest = line.get(3..).unwrap_or_default();
        lines.push(rest.trim_start_matches(['-', ' ']).to_owned());
        if !rest.starts_with('-') {
            return Ok(Reply { code, lines });
        }
    }
}
