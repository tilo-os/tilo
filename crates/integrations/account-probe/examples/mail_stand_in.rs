//! A stand-in mail server for a rehearsal of connecting a mail account: an IMAP and an SMTP
//! server with real sockets that take one made-up login, show a few made-up messages and keep
//! the mail they are handed, and, where it is asked for, the settings document a provider
//! publishes so a mail program finds its servers from an address alone.
//!
//! ```text
//! mail_stand_in --name imap.NAME --name smtp.NAME --login LOGIN --password-file FILE \
//!     --write-certificate FILE [--imap ADDRESS] [--smtp ADDRESS] [--imap-tls] [--smtp-tls] \
//!     [--autoconfig ADDRESS]… [--autoconfig-document FILE]
//! ```
//!
//! As a mail server does, port 993 and port 465 are TLS from the first byte and every other
//! port greets in the clear and upgrades with `STARTTLS`; `--imap-tls` and `--smtp-tls` say
//! the first of it for another port, where the client is told to expect it. The program makes
//! one certificate for the names it is given and writes it to the file named: whoever signs in
//! has to trust it, so it is added to the trust bundle of the stack that is rehearsed and of
//! nothing else. The password is read from a file, so it stands on no command line. No real
//! mail server, no real address and nobody's password belong here.
//!
//! With `--autoconfig ADDRESS` it also answers https there with the document
//! `--autoconfig-document` holds, for any path: what a provider publishes at
//! `autoconfig.<domain>/mail/config-v1.1.xml` and `<domain>/.well-known/autoconfig/mail/config-v1.1.xml`,
//! so a mail program that looks its servers up finds this stand-in. An address it cannot
//! listen on is said, and the mail servers go on without it.

#[path = "../src/stand_in.rs"]
#[allow(
    dead_code,
    reason = "the program serves; what a test reads of the mailbox is not read here"
)]
mod stand_in;

use std::convert::Infallible;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::ExitCode;
use std::sync::Arc;

const USAGE: &str = "usage: mail_stand_in --name NAME [--name NAME ..] --login LOGIN \
     --password-file FILE --write-certificate FILE [--imap ADDRESS] [--smtp ADDRESS] \
     [--imap-tls] [--smtp-tls] [--autoconfig ADDRESS]… [--autoconfig-document FILE]";

fn main() -> ExitCode {
    match stood_in() {
        Ok(never) => match never {},
        Err(why) => {
            eprintln!("mail stand-in: {why}\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

/// Start both servers as the arguments say, and answer until the program is ended.
fn stood_in() -> Result<Infallible, String> {
    let (mut imap, mut smtp) = ("127.0.0.1:993".to_owned(), "127.0.0.1:465".to_owned());
    let mut names = Vec::new();
    let (mut imap_tls, mut smtp_tls) = (false, false);
    let (mut login, mut password_file, mut certificate_file) = (None, None, None);
    let (mut autoconfig_addresses, mut autoconfig_document) = (Vec::new(), None);
    let mut said = std::env::args().skip(1);
    while let Some(flag) = said.next() {
        match flag.as_str() {
            "--imap-tls" => {
                imap_tls = true;
                continue;
            }
            "--smtp-tls" => {
                smtp_tls = true;
                continue;
            }
            _ => {}
        }
        let value = said.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--imap" => imap = value,
            "--smtp" => smtp = value,
            "--name" => names.push(value),
            "--login" => login = Some(value),
            "--password-file" => password_file = Some(value),
            "--write-certificate" => certificate_file = Some(value),
            "--autoconfig" => autoconfig_addresses.push(value),
            "--autoconfig-document" => autoconfig_document = Some(value),
            _ => return Err(format!("unknown argument {flag}")),
        }
    }
    let needed =
        |value: Option<String>, flag: &str| value.ok_or_else(|| format!("{flag} is needed"));
    let login = needed(login, "--login")?;
    let password_file = needed(password_file, "--password-file")?;
    let certificate_file = needed(certificate_file, "--write-certificate")?;
    if names.is_empty() {
        return Err("--name is needed: a name the certificate is for".to_owned());
    }
    let password = std::fs::read_to_string(&password_file)
        .map_err(|error| format!("cannot read {password_file}: {error}"))?
        .trim()
        .to_owned();
    if password.is_empty() {
        return Err(format!("{password_file} holds no password"));
    }
    let on_port = |address: &str, port: &str| {
        address
            .rsplit_once(':')
            .is_some_and(|(_, said)| said == port)
    };
    let up = stand_in::started(stand_in::Serving {
        imap_wrapped: imap_tls || on_port(&imap, "993"),
        smtp_wrapped: smtp_tls || on_port(&smtp, "465"),
        names,
        login,
        password,
        oauth_token: None,
        imap,
        smtp,
        layout: stand_in::Layout::Plain,
        seeded: true,
    })?;
    std::fs::write(&certificate_file, stand_in::pem(&up.certificate))
        .map_err(|error| format!("cannot write {certificate_file}: {error}"))?;
    println!(
        "mail stand-in: IMAP at {}, SMTP at {}; its certificate is in {certificate_file}. It \
         takes one login, holds a few made-up messages and keeps what it is handed.",
        up.imap, up.smtp
    );
    if !autoconfig_addresses.is_empty() {
        let document = autoconfig_document
            .ok_or_else(|| "--autoconfig needs --autoconfig-document FILE".to_owned())
            .and_then(|file| {
                std::fs::read_to_string(&file)
                    .map_err(|error| format!("cannot read {file}: {error}"))
            })?;
        for address in autoconfig_addresses {
            match published_on(&address, Arc::clone(&up.tls), document.clone()) {
                Ok(at) => println!("mail stand-in: the autoconfig document is at https://{at}/"),
                Err(why) => eprintln!("mail stand-in: no autoconfig document: {why}"),
            }
        }
    }
    loop {
        std::thread::park();
    }
}

/// Answer every https request on `address` with `document`, for whatever path it asks: what a
/// provider publishes for mail programs.
fn published_on(
    address: &str,
    tls: Arc<rustls::ServerConfig>,
    document: String,
) -> Result<SocketAddr, String> {
    let listener = TcpListener::bind(address)
        .map_err(|error| format!("cannot listen on {address}: {error}"))?;
    let at = listener
        .local_addr()
        .map_err(|error| format!("cannot read where {address} listens: {error}"))?;
    std::thread::spawn(move || {
        for tcp in listener.incoming().flatten() {
            let tls = Arc::clone(&tls);
            let document = document.clone();
            std::thread::spawn(move || answered(tcp, tls, &document));
        }
    });
    Ok(at)
}

/// One https connection: read the request's head, answer it with the document.
fn answered(tcp: TcpStream, tls: Arc<rustls::ServerConfig>, document: &str) {
    let Ok(connection) = rustls::ServerConnection::new(tls) else {
        return;
    };
    let mut stream = rustls::StreamOwned::new(connection, tcp);
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    while request.len() < 8 * 1024 {
        match stream.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(read) => request.extend_from_slice(buffer.get(..read).unwrap_or_default()),
        }
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let body = document.as_bytes();
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/xml; charset=utf-8\r\n\
Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}
