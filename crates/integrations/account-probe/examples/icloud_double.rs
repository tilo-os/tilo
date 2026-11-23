//! An iCloud-shaped stand-in for a rehearsal of connecting an iCloud account: the stand-in
//! mail server beside this (an IMAP and an SMTP server with real sockets) and, under the same
//! login and app password, a small `CalDAV` server that discovery finds a calendar in.
//!
//! ```text
//! icloud_double --name NAME [--name NAME ..] --login LOGIN --password-file FILE \
//!     --write-certificate FILE [--imap ADDRESS] [--smtp ADDRESS] [--caldav ADDRESS] \
//!     [--imap-tls] [--smtp-tls] [--autoconfig ADDRESS]… [--autoconfig-document FILE]
//! ```
//!
//! The mail servers are the tree's stand-in (`mail_stand_in`'s own: one certificate for every
//! name given, one login, the same password). The calendar server is TLS on `--caldav`
//! (default `127.0.0.1:443`), and answers discovery at `https://caldav.icloud.com` when the
//! walk maps that name here: the principal and the calendar home, and the depth-one listing
//! whose one calendar is "Personal". Both take the same login and app password.
//!
//! `--autoconfig ADDRESS` serves `--autoconfig-document FILE` on https there, for whatever
//! path is asked: what a provider publishes at `<domain>/.well-known/autoconfig/mail/config-v1.1.xml`,
//! so a mail program that looks its servers up finds this stand-in.
//!
//! It prints what it serves as `mail_stand_in` does, then one JSON document:
//! `{"address", "imap", "smtp", "caldav", "caldav_listener", "autoconfig"}`. `caldav` is
//! where the calendar lives as Tilo reaches iCloud's own, `https://caldav.icloud.com`.

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

const USAGE: &str = "usage: icloud_double --name NAME [--name NAME ..] --login LOGIN \
     --password-file FILE --write-certificate FILE [--imap ADDRESS] [--smtp ADDRESS] \
     [--caldav ADDRESS] [--imap-tls] [--smtp-tls] [--autoconfig ADDRESS]… \
     [--autoconfig-document FILE]";

/// The address a walk's calendar is reached at, as Tilo reaches iCloud's own.
const ICLOUD_CALENDARS: &str = "https://caldav.icloud.com";

fn main() -> ExitCode {
    match stood_in() {
        Ok(never) => match never {},
        Err(why) => {
            eprintln!("icloud double: {why}\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

/// Start the mail and calendar servers as the arguments say, and answer until the program is
/// ended.
#[allow(
    clippy::too_many_lines,
    reason = "one program: its arguments read in order, then what each server is started with"
)]
fn stood_in() -> Result<Infallible, String> {
    let (mut imap, mut smtp) = ("127.0.0.1:993".to_owned(), "127.0.0.1:465".to_owned());
    let mut caldav = "127.0.0.1:443".to_owned();
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
            "--caldav" => caldav = value,
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
        login: login.clone(),
        password: password.clone(),
        oauth_token: None,
        imap,
        smtp,
        // A provider's own folders, as iCloud lays its mail out.
        layout: stand_in::Layout::Plain,
        seeded: true,
    })?;
    std::fs::write(&certificate_file, stand_in::pem(&up.certificate))
        .map_err(|error| format!("cannot write {certificate_file}: {error}"))?;
    let caldav_address: SocketAddr = caldav
        .parse()
        .map_err(|_| format!("{caldav} is not an address to listen on"))?;
    let at = calendar::serve(caldav_address, Arc::clone(&up.tls), login.clone(), password)
        .map_err(|error| format!("CalDAV cannot listen on {caldav_address}: {error}"))?;
    let mut autoconfig = Vec::new();
    if !autoconfig_addresses.is_empty() {
        let document = autoconfig_document
            .ok_or_else(|| "--autoconfig needs --autoconfig-document FILE".to_owned())
            .and_then(|file| {
                std::fs::read_to_string(&file)
                    .map_err(|error| format!("cannot read {file}: {error}"))
            })?;
        for address in autoconfig_addresses {
            match published_on(&address, Arc::clone(&up.tls), document.clone()) {
                Ok(at) => {
                    println!("icloud double: the autoconfig document is at https://{at}/");
                    autoconfig.push(format!("https://{at}/"));
                }
                Err(why) => eprintln!("icloud double: no autoconfig document: {why}"),
            }
        }
    }
    println!(
        "icloud double: IMAP at {}, SMTP at {}, CalDAV at {ICLOUD_CALENDARS} ({at})",
        up.imap, up.smtp
    );
    println!(
        "{}",
        serde_json::json!({
            "address": login,
            "imap": up.imap.to_string(),
            "smtp": up.smtp.to_string(),
            "caldav": ICLOUD_CALENDARS,
            "caldav_listener": at.to_string(),
            "autoconfig": autoconfig,
        })
    );
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

/// The calendar server: TLS on its own listener, signed in with the account's login and app
/// password, saying where the principal, its home and the calendar in it are.
mod calendar {
    use std::io::{Read, Write as _};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::Arc;

    use base64::Engine as _;
    use rustls::ServerConnection;

    /// Answer on `address` until the program ends, one connection at a time.
    pub(super) fn serve(
        address: SocketAddr,
        tls: Arc<rustls::ServerConfig>,
        login: String,
        password: String,
    ) -> std::io::Result<SocketAddr> {
        let listener = TcpListener::bind(address)?;
        let at = listener.local_addr()?;
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let tls = Arc::clone(&tls);
                let (login, password) = (login.clone(), password.clone());
                std::thread::spawn(move || {
                    let _ = answer(stream, &tls, &login, &password);
                });
            }
        });
        Ok(at)
    }

    /// One request, answered and closed: HTTP/1.1 over the stand-in's certificate.
    fn answer(
        stream: TcpStream,
        tls: &Arc<rustls::ServerConfig>,
        login: &str,
        password: &str,
    ) -> std::io::Result<()> {
        let session = ServerConnection::new(Arc::clone(tls))
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let mut secure = rustls::StreamOwned::new(session, stream);
        let Some(request) = read_request(&mut secure)? else {
            return Ok(());
        };
        let (status, reply) = reply(&request, login, password);
        let allow = if status.starts_with("401") {
            "WWW-Authenticate: Basic realm=\"iCloud\"\r\n"
        } else {
            ""
        };
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/xml; charset=utf-8\r\n\
             Content-Length: {}\r\nConnection: close\r\n{allow}\r\n",
            reply.len()
        );
        secure.write_all(head.as_bytes())?;
        secure.write_all(reply.as_bytes())?;
        secure.flush()
    }

    /// The request's method, path and Authorization header, once its headers arrived.
    fn read_request(secure: &mut impl Read) -> std::io::Result<Option<(String, String, String)>> {
        let mut bytes = Vec::new();
        let mut byte = [0u8; 1];
        while !bytes.ends_with(b"\r\n\r\n") {
            if secure.read(&mut byte)? == 0 {
                return Ok(None);
            }
            bytes.push(byte[0]);
            if bytes.len() > 64 * 1024 {
                return Ok(None);
            }
        }
        let text = String::from_utf8_lossy(&bytes);
        let mut lines = text.lines();
        let Some(first) = lines.next() else {
            return Ok(None);
        };
        let mut parts = first.split_whitespace();
        let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
            return Ok(None);
        };
        let authorization = lines
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| value.trim().to_owned())
            .unwrap_or_default();
        Ok(Some((method.to_owned(), path.to_owned(), authorization)))
    }

    /// The status and body for `request`.
    fn reply(request: &(String, String, String), login: &str, password: &str) -> (String, String) {
        let (method, path, authorization) = request;
        if !signed_in(authorization, login, password) {
            return ("401 Unauthorized".to_owned(), String::new());
        }
        let path = path.split(['?', '#']).next().unwrap_or(path);
        match method.as_str() {
            "OPTIONS" => ("200 OK".to_owned(), String::new()),
            "PROPFIND" => ("207 Multi-Status".to_owned(), listing(path)),
            "REPORT" => ("207 Multi-Status".to_owned(), multistatus(&[])),
            _ => ("404 Not Found".to_owned(), String::new()),
        }
    }

    /// Whether `authorization` is `login`'s own sign-in with the app password.
    fn signed_in(authorization: &str, login: &str, password: &str) -> bool {
        let Some(encoded) = authorization.strip_prefix("Basic ") else {
            return false;
        };
        let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded.trim()) else {
            return false;
        };
        let Ok(pair) = String::from_utf8(decoded) else {
            return false;
        };
        pair == format!("{login}:{password}")
    }

    /// The properties discovery asks of each address it visits.
    const PRINCIPAL: &str = "<d:current-user-principal><d:href>/principals/me/</d:href>\
                             </d:current-user-principal>\
                             <c:calendar-home-set><d:href>/calendars/me/</d:href>\
                             </c:calendar-home-set>";

    /// What every path discovery may ask of says: the principal and the home, and, at the
    /// home, the one calendar in it.
    fn listing(path: &str) -> String {
        let collection = "<d:resourcetype><d:collection/></d:resourcetype>";
        let privileges = "<d:current-user-privilege-set><d:privilege><d:read/></d:privilege>\
                          <d:privilege><d:write/></d:privilege></d:current-user-privilege-set>";
        let home = format!(
            "<d:response><d:href>/calendars/me/</d:href><d:propstat><d:prop>{collection}\
             <d:displayname>Home</d:displayname>{privileges}</d:prop>\
             <d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
        );
        let calendar = format!(
            "<d:response><d:href>/calendars/me/personal/</d:href><d:propstat><d:prop>\
             <d:resourcetype><d:collection/><c:calendar/></d:resourcetype>\
             <d:displayname>Personal</d:displayname>{privileges}</d:prop>\
             <d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
        );
        match path {
            "/calendars/me/" | "/calendars/me" => multistatus(&[&home, &calendar]),
            "/calendars/me/personal/" | "/calendars/me/personal" => multistatus(&[&calendar]),
            "/" | "" => multistatus(&[&format!(
                "<d:response><d:href>/</d:href><d:propstat><d:prop>{PRINCIPAL}{collection}\
                 </d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"
            )]),
            other => multistatus(&[&format!(
                "<d:response><d:href>{other}</d:href><d:propstat><d:prop>{PRINCIPAL}\
                 {collection}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>\
                 </d:response>"
            )]),
        }
    }

    /// `responses` as a multistatus document, in the namespaces discovery reads.
    fn multistatus(responses: &[&str]) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
             <d:multistatus xmlns:d=\"DAV:\" xmlns:c=\"urn:ietf:params:xml:ns:caldav\">{}\
             </d:multistatus>",
            responses.concat()
        )
    }
}
