//! A stand-in mail server with real sockets: an IMAP and an SMTP server that take one made-up
//! login and refuse every other, for a rehearsal of a mail sign-in where no real mail server,
//! no real address and nobody's password may be used.
//!
//! It speaks what this crate's sign-in proof speaks: a port is TLS from the first byte, or
//! greets in the clear and upgrades with `STARTTLS`, and nothing signs in before the
//! connection is secure. Signed in, it shows an empty inbox. It keeps no mail and sends none:
//! a message handed to it is refused, so nothing a person writes is swallowed.
//!
//! The example `mail_stand_in` is this module as a program (the local stack starts it as
//! `mail-stand-in`); the crate's tests sign in to it
//! with the proof itself.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;

use base64::Engine as _;
use rustls::pki_types::CertificateDer;

/// What an IMAP server of this kind says it can do once the connection is secure.
const IMAP_OFFERS: &str = "IMAP4rev1 AUTH=PLAIN AUTH=XOAUTH2";

/// What a stand-in serves: the names its certificate is for, the one login it takes, where
/// its two servers listen, and which of them is TLS from the first byte.
pub struct Serving {
    pub names: Vec<String>,
    pub login: String,
    pub password: String,
    /// A bearer token the stand-in takes in place of the password, by
    /// `XOAUTH2`: the sign-in a provider's own page leaves an account with.
    pub oauth_token: Option<String>,
    pub imap: String,
    pub smtp: String,
    pub imap_wrapped: bool,
    pub smtp_wrapped: bool,
}

/// A stand-in that listens: where, and the certificate both servers present.
pub struct Started {
    pub imap: SocketAddr,
    pub smtp: SocketAddr,
    pub certificate: CertificateDer<'static>,
}

/// The one login, and the identity both servers present.
struct Taking {
    name: String,
    login: String,
    password: String,
    oauth_token: Option<String>,
    tls: Arc<rustls::ServerConfig>,
}

impl Taking {
    fn takes(&self, login: &str, password: &str) -> bool {
        login == self.login && password == self.password
    }

    /// Whether `user` with `token` is the one login signed in with
    /// `XOAUTH2`.
    fn takes_token(&self, user: &str, token: &str) -> bool {
        user.trim() == self.login && self.oauth_token.as_deref() == Some(token.trim())
    }
}

/// Start both servers. They answer for as long as the program lives.
///
/// # Errors
/// No certificate could be made for the names, or an address could not be listened on.
pub fn started(serving: Serving) -> Result<Started, String> {
    let certified = rcgen::generate_simple_self_signed(serving.names.clone())
        .map_err(|error| format!("no certificate for {:?}: {error}", serving.names))?;
    let certificate = certified.cert.der().clone();
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der());
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|error| error.to_string())?
    .with_no_client_auth()
    .with_single_cert(
        vec![certificate.clone()],
        rustls::pki_types::PrivateKeyDer::Pkcs8(key),
    )
    .map_err(|error| error.to_string())?;
    let listening = |address: &str| {
        TcpListener::bind(address).map_err(|error| format!("cannot listen on {address}: {error}"))
    };
    let (imap, smtp) = (listening(&serving.imap)?, listening(&serving.smtp)?);
    let at = |listener: &TcpListener| listener.local_addr().map_err(|error| error.to_string());
    let started = Started {
        imap: at(&imap)?,
        smtp: at(&smtp)?,
        certificate,
    };
    let taking = Arc::new(Taking {
        name: serving
            .names
            .first()
            .cloned()
            .unwrap_or_else(|| "localhost".to_owned()),
        login: serving.login,
        password: serving.password,
        oauth_token: serving.oauth_token,
        tls: Arc::new(tls),
    });
    serve(
        imap,
        Arc::clone(&taking),
        serving.imap_wrapped,
        imap_connection,
    );
    serve(smtp, taking, serving.smtp_wrapped, smtp_connection);
    Ok(started)
}

/// `certificate` as the text a trust bundle holds.
#[must_use]
pub fn pem(certificate: &CertificateDer<'_>) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(certificate.as_ref());
    let mut text = String::from("-----BEGIN CERTIFICATE-----\n");
    for piece in encoded.as_bytes().chunks(64) {
        text.push_str(&String::from_utf8_lossy(piece));
        text.push('\n');
    }
    text.push_str("-----END CERTIFICATE-----\n");
    text
}

/// Answer every connection to `listener` on a thread of its own.
fn serve(
    listener: TcpListener,
    taking: Arc<Taking>,
    wrapped: bool,
    connection: fn(TcpStream, &Taking, bool),
) {
    std::thread::spawn(move || {
        // A connection that could not be taken is passed over; the next one is waited for.
        for tcp in listener.incoming().flatten() {
            let taking = Arc::clone(&taking);
            std::thread::spawn(move || connection(tcp, &taking, wrapped));
        }
    });
}

/// One side of a conversation in lines.
struct Lines<S: Read + Write>(BufReader<S>);

impl<S: Read + Write> Lines<S> {
    /// The next line without its end, or nothing once the other side left.
    fn line(&mut self) -> Option<String> {
        let mut line = String::new();
        match self.0.read_line(&mut line) {
            Ok(read) if read > 0 => Some(line.trim_end_matches(['\r', '\n']).to_owned()),
            _ => None,
        }
    }

    /// Send `text` and a line's end. Whether it was sent.
    fn say(&mut self, text: &str) -> bool {
        let to = self.0.get_mut();
        to.write_all(text.as_bytes()).is_ok() && to.write_all(b"\r\n").is_ok() && to.flush().is_ok()
    }
}

/// A connection this stand-in secured with its certificate.
type Secure = rustls::StreamOwned<rustls::ServerConnection, TcpStream>;

/// `tcp` as a connection secured with this stand-in's certificate.
fn secured(tcp: TcpStream, taking: &Taking) -> Option<Lines<Secure>> {
    let connection = rustls::ServerConnection::new(Arc::clone(&taking.tls)).ok()?;
    Some(Lines(BufReader::new(rustls::StreamOwned::new(
        connection, tcp,
    ))))
}

/// A command line's tag, its command in capitals, and what follows.
fn parts(line: &str) -> (String, String, String) {
    let mut words = line.splitn(3, ' ');
    let tag = words.next().unwrap_or_default().to_owned();
    let command = words.next().unwrap_or_default().to_ascii_uppercase();
    let rest = words.next().unwrap_or_default().to_owned();
    (tag, command, rest)
}

/// The words of a command's arguments: quoted strings without their quotes, and bare words.
fn arguments(rest: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut letters = rest.chars().peekable();
    while let Some(first) = letters.next() {
        if first != ' ' {
            let mut word = String::new();
            if first == '"' {
                while let Some(letter) = letters.next() {
                    match letter {
                        '"' => break,
                        '\\' => word.extend(letters.next()),
                        other => word.push(other),
                    }
                }
            } else {
                word.push(first);
                while let Some(letter) = letters.next_if(|letter| *letter != ' ') {
                    word.push(letter);
                }
            }
            words.push(word);
        }
    }
    words
}

/// The login and the password a `PLAIN` sign-in carries.
fn plain_login(token: &str) -> Option<(String, String)> {
    let said = base64::engine::general_purpose::STANDARD
        .decode(token.trim())
        .ok()?;
    let mut fields = said
        .split(|byte| *byte == 0)
        .map(|field| String::from_utf8_lossy(field).into_owned());
    let (_, login, password) = (fields.next()?, fields.next()?, fields.next()?);
    Some((login, password))
}

/// The user and bearer token an `XOAUTH2` sign-in carries, decoded:
/// `user=<name>\x01auth=Bearer <token>\x01\x01`.
fn oauth2_login(token: &str) -> Option<(String, String)> {
    let said = base64::engine::general_purpose::STANDARD
        .decode(token.trim())
        .ok()?;
    let said = String::from_utf8(said).ok()?;
    let mut fields = said.split('\u{1}');
    let user = fields.next()?.strip_prefix("user=")?.to_owned();
    let bearer = fields.next()?.strip_prefix("auth=Bearer ")?.to_owned();
    Some((user, bearer))
}

/// What the client answers to `prompt`, decoded.
fn asked<S: Read + Write>(lines: &mut Lines<S>, prompt: &str) -> Option<String> {
    lines.say(prompt).then_some(())?;
    let said = base64::engine::general_purpose::STANDARD
        .decode(lines.line()?.trim())
        .ok()?;
    String::from_utf8(said).ok()
}

fn imap_connection(tcp: TcpStream, taking: &Taking, wrapped: bool) {
    let tcp = if wrapped {
        tcp
    } else if let Some(upgraded) = imap_in_the_clear(tcp) {
        upgraded
    } else {
        return;
    };
    let Some(mut lines) = secured(tcp, taking) else {
        return;
    };
    // A connection that upgraded was greeted in the clear and is not greeted again.
    if wrapped
        && !lines.say(&format!(
            "* OK [CAPABILITY {IMAP_OFFERS}] mail stand-in ready"
        ))
    {
        return;
    }
    imap_secure(&mut lines, taking);
}

/// The cleartext start of an IMAP connection: the greeting and, when asked, the upgrade. The
/// connection once the client may begin its handshake; nothing signs in before it.
fn imap_in_the_clear(tcp: TcpStream) -> Option<TcpStream> {
    let mut lines = Lines(BufReader::new(tcp));
    lines
        .say("* OK [CAPABILITY IMAP4rev1 STARTTLS LOGINDISABLED] mail stand-in ready")
        .then_some(())?;
    loop {
        let line = lines.line()?;
        let (tag, command, _) = parts(&line);
        let said = match command.as_str() {
            "CAPABILITY" => {
                lines.say("* CAPABILITY IMAP4rev1 STARTTLS LOGINDISABLED")
                    && lines.say(&format!("{tag} OK CAPABILITY completed"))
            }
            "STARTTLS" => {
                // The client waits for this answer before its handshake, so nothing of the
                // handshake is left in this reader.
                let begun = lines.say(&format!("{tag} OK begin TLS negotiation now"));
                return begun.then(|| lines.0.into_inner());
            }
            "LOGOUT" => {
                lines.say("* BYE mail stand-in leaving");
                lines.say(&format!("{tag} OK LOGOUT completed"));
                return None;
            }
            "NOOP" => lines.say(&format!("{tag} OK NOOP completed")),
            _ => lines.say(&format!(
                "{tag} NO [PRIVACYREQUIRED] secure the connection first"
            )),
        };
        if !said {
            return None;
        }
    }
}

/// The secured part of an IMAP connection: the sign-in, then an empty mailbox.
fn imap_secure<S: Read + Write>(lines: &mut Lines<S>, taking: &Taking) {
    let mut signed_in = false;
    let signed = |tag: &str, taken: bool| {
        if taken {
            format!("{tag} OK signed in")
        } else {
            format!("{tag} NO [AUTHENTICATIONFAILED] the mail stand-in does not take this login")
        }
    };
    while let Some(line) = lines.line() {
        let (tag, command, rest) = parts(&line);
        // A command by message numbers of the other kind is answered as the plain one.
        let (command, rest) = if command == "UID" {
            let (_, inner, rest) = parts(&format!("- {rest}"));
            (inner, rest)
        } else {
            (command, rest)
        };
        let said = match command.as_str() {
            "LOGIN" => {
                signed_in = match arguments(&rest).as_slice() {
                    [login, password] => taking.takes(login, password),
                    _ => false,
                };
                lines.say(&signed(&tag, signed_in))
            }
            "AUTHENTICATE" => {
                let mut words = rest.split(' ');
                let mechanism = words.next().unwrap_or_default().to_ascii_uppercase();
                let plain = mechanism == "PLAIN";
                let xoauth2 = mechanism == "XOAUTH2";
                let token = match words.next() {
                    Some(token) => Some(token.to_owned()),
                    None if (plain || xoauth2) && lines.say("+ ") => lines.line(),
                    None => None,
                };
                signed_in = (plain
                    && token
                        .as_deref()
                        .and_then(plain_login)
                        .is_some_and(|(login, password)| taking.takes(&login, &password)))
                    || (xoauth2
                        && token
                            .as_deref()
                            .and_then(oauth2_login)
                            .is_some_and(|(user, token)| taking.takes_token(&user, &token)));
                lines.say(&signed(&tag, signed_in))
            }
            "IDLE" if signed_in => {
                lines.say("+ idling");
                while lines
                    .line()
                    .is_some_and(|line| !line.eq_ignore_ascii_case("DONE"))
                {}
                lines.say(&format!("{tag} OK IDLE ended"))
            }
            "LOGOUT" => {
                lines.say("* BYE mail stand-in leaving");
                lines.say(&format!("{tag} OK LOGOUT completed"));
                return;
            }
            _ => match of_an_empty_mailbox(&command, &rest, signed_in) {
                Some(untagged) => {
                    untagged.iter().all(|line| lines.say(line))
                        && lines.say(&format!("{tag} OK {command} completed"))
                }
                None if signed_in => lines.say(&format!(
                    "{tag} BAD the mail stand-in does not know {command}"
                )),
                None => lines.say(&format!("{tag} NO sign in first")),
            },
        };
        if !said {
            return;
        }
    }
}

/// What an empty mailbox answers to a command that needs no conversation: its untagged lines.
/// Nothing for a command this stand-in does not know, or one that needs a sign-in first.
fn of_an_empty_mailbox(command: &str, rest: &str, signed_in: bool) -> Option<Vec<String>> {
    let lines = |lines: &[&str]| Some(lines.iter().map(|line| (*line).to_owned()).collect());
    if !signed_in && !matches!(command, "CAPABILITY" | "NOOP" | "ID") {
        return None;
    }
    match command {
        "CAPABILITY" => Some(vec![format!("* CAPABILITY {IMAP_OFFERS}")]),
        "ID" => lines(&["* ID NIL"]),
        "NOOP" | "CHECK" | "CLOSE" | "ENABLE" | "SUBSCRIBE" | "LSUB" | "FETCH" | "STORE"
        | "EXPUNGE" => Some(Vec::new()),
        "NAMESPACE" => lines(&["* NAMESPACE ((\"\" \"/\")) NIL NIL"]),
        "LIST" if rest.trim_end().ends_with("\"\"") => lines(&["* LIST (\\Noselect) \"/\" \"\""]),
        "LIST" => lines(&["* LIST (\\HasNoChildren) \"/\" \"INBOX\""]),
        "SELECT" | "EXAMINE" => lines(&[
            "* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)",
            "* 0 EXISTS",
            "* 0 RECENT",
            "* OK [UIDVALIDITY 1] the mail stand-in's one mailbox",
            "* OK [UIDNEXT 1] it is empty",
        ]),
        "STATUS" => {
            lines(&["* STATUS \"INBOX\" (MESSAGES 0 RECENT 0 UIDNEXT 1 UIDVALIDITY 1 UNSEEN 0)"])
        }
        "SEARCH" => lines(&["* SEARCH"]),
        _ => None,
    }
}

fn smtp_connection(tcp: TcpStream, taking: &Taking, wrapped: bool) {
    let greeting = format!("220 {} ESMTP mail stand-in", taking.name);
    let tcp = if wrapped {
        tcp
    } else if let Some(upgraded) = smtp_in_the_clear(tcp, &greeting, &taking.name) {
        upgraded
    } else {
        return;
    };
    let Some(mut lines) = secured(tcp, taking) else {
        return;
    };
    // A connection that upgraded was greeted in the clear and is not greeted again.
    if wrapped && !lines.say(&greeting) {
        return;
    }
    smtp_secure(&mut lines, taking);
}

/// The cleartext start of an SMTP connection: the greeting, what is offered, and the upgrade.
/// The connection once the client may begin its handshake; nothing signs in before it.
fn smtp_in_the_clear(tcp: TcpStream, greeting: &str, name: &str) -> Option<TcpStream> {
    let mut lines = Lines(BufReader::new(tcp));
    lines.say(greeting).then_some(())?;
    loop {
        let line = lines.line()?;
        let command = line
            .split(' ')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let said = match command.as_str() {
            "EHLO" | "HELO" => lines.say(&format!("250-{name}")) && lines.say("250 STARTTLS"),
            "STARTTLS" => {
                let begun = lines.say("220 ready to start TLS");
                return begun.then(|| lines.0.into_inner());
            }
            "QUIT" => {
                lines.say("221 bye");
                return None;
            }
            "NOOP" | "RSET" => lines.say("250 ok"),
            _ => lines.say("530 5.7.0 secure the connection first"),
        };
        if !said {
            return None;
        }
    }
}

/// The secured part of an SMTP connection: what is offered, the sign-in, and no mail taken.
fn smtp_secure<S: Read + Write>(lines: &mut Lines<S>, taking: &Taking) {
    while let Some(line) = lines.line() {
        let mut words = line.split(' ');
        let command = words.next().unwrap_or_default().to_ascii_uppercase();
        let said = match command.as_str() {
            "EHLO" | "HELO" => {
                lines.say(&format!("250-{}", taking.name))
                    && lines.say("250 AUTH PLAIN LOGIN XOAUTH2")
            }
            "AUTH" => {
                let mechanism = words.next().unwrap_or_default().to_ascii_uppercase();
                let said = match mechanism.as_str() {
                    "PLAIN" => match words.next() {
                        Some(token) => plain_login(token),
                        None => asked_plain(lines),
                    },
                    "LOGIN" => {
                        let login = asked(lines, "334 VXNlcm5hbWU6");
                        let password = login
                            .as_ref()
                            .and_then(|_| asked(lines, "334 UGFzc3dvcmQ6"));
                        login.zip(password)
                    }
                    "XOAUTH2" => match words.next() {
                        Some(token) => oauth2_login(token),
                        None => lines
                            .say("334 ")
                            .then_some(())
                            .and_then(|()| lines.line())
                            .as_deref()
                            .and_then(oauth2_login),
                    },
                    _ => None,
                };
                let accepted = said
                    .as_ref()
                    .is_some_and(|(login, secret)| taking.takes(login, secret));
                let accepted_token = mechanism == "XOAUTH2"
                    && said
                        .as_ref()
                        .is_some_and(|(user, token)| taking.takes_token(user, token));
                if accepted || accepted_token {
                    lines.say("235 2.7.0 signed in")
                } else {
                    lines.say("535 5.7.8 the mail stand-in does not take this login")
                }
            }
            "MAIL" | "RCPT" | "DATA" => lines.say("554 5.7.1 the mail stand-in sends no mail"),
            "NOOP" | "RSET" => lines.say("250 ok"),
            "QUIT" => {
                lines.say("221 bye");
                return;
            }
            _ => lines.say("502 5.5.1 the mail stand-in does not know this command"),
        };
        if !said {
            return;
        }
    }
}

/// The login and the password of a `PLAIN` sign-in the client sends on a line of its own.
fn asked_plain<S: Read + Write>(lines: &mut Lines<S>) -> Option<(String, String)> {
    lines.say("334 ").then_some(())?;
    plain_login(&lines.line()?)
}
