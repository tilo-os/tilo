//! A stand-in mail server with real sockets: an IMAP and an SMTP server that take one made-up
//! login and refuse every other, for a rehearsal of a mail account where no real mail server,
//! no real address and nobody's password may be used.
//!
//! Its inbox holds a few made-up messages, so whoever rehearses reads, searches and answers
//! mail as a person does, and what its SMTP server is handed it delivers to that inbox, so a
//! message sent to it arrives. It speaks what this crate's sign-in proof speaks: a port is TLS
//! from the first byte, or greets in the clear and upgrades with `STARTTLS`, and nothing signs
//! in before the connection is secure. It sends nothing anywhere: every message the SMTP
//! server takes stays on the box it is started on.
//!
//! The example `mail_stand_in` is this module as a program (the local stack starts it as
//! `mail-stand-in`); the crate's tests sign in to it with the proof itself.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use rustls::pki_types::CertificateDer;

/// What an IMAP server of this kind says it can do once the connection is secure.
const IMAP_OFFERS: &str = "IMAP4rev1 AUTH=PLAIN AUTH=XOAUTH2 IDLE";

/// The made-up mail the inbox holds when a stand-in starts: whose it is from, its subject, its
/// words, and the hour it arrived on the 18th of November 2026, oldest first.
const INBOX: [(&str, &str, &str); 3] = [
    (
        "Quarterly plan",
        "The draft is in the shared folder.",
        "14-Nov-2026 09:05:00 +0000",
    ),
    (
        "Lunch on Friday?",
        "Does one o'clock work for you?",
        "17-Nov-2026 12:10:00 +0000",
    ),
    (
        "Welcome to the lab",
        "Your badge is at the reception from Monday.",
        "18-Nov-2026 08:30:00 +0000",
    ),
];

/// Who the inbox's made-up mail is from.
const SENDER: &str = "Grace Hopper <grace@example.test>";

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

/// A stand-in that listens: where, the certificate both servers present, and the TLS they
/// speak it with (the autoconfig document, where one is served, presents the same).
pub struct Started {
    pub imap: SocketAddr,
    pub smtp: SocketAddr,
    pub certificate: CertificateDer<'static>,
    /// The TLS both servers speak, which the program that serves a provider's settings document
    /// presents too: the tests sign in and read mail, and the example serves https.
    #[allow(
        dead_code,
        reason = "read by the example that serves the settings document"
    )]
    pub tls: Arc<rustls::ServerConfig>,
}

/// The one login, the identity both servers present, and the mail they hold.
struct Taking {
    name: String,
    login: String,
    password: String,
    oauth_token: Option<String>,
    tls: Arc<rustls::ServerConfig>,
    mail: Arc<Mutex<Mail>>,
}

impl Taking {
    fn takes(&self, login: &str, password: &str) -> bool {
        login == self.login && password == self.password
    }

    /// Whether `user` with `token` is the one login signed in with `XOAUTH2`.
    fn takes_token(&self, user: &str, token: &str) -> bool {
        user.trim() == self.login && self.oauth_token.as_deref() == Some(token.trim())
    }
}

/// One message as the stand-in holds it.
struct Stored {
    uid: u32,
    flags: Vec<String>,
    /// When it arrived, as `INTERNALDATE` writes a date.
    received: String,
    raw: Vec<u8>,
}

/// One folder of the stand-in's mailbox.
struct Folder {
    name: String,
    validity: u32,
    next_uid: u32,
    messages: Vec<Stored>,
}

/// The stand-in's whole mailbox: an inbox with a few made-up messages, and whatever folders a
/// mail program made.
struct Mail {
    folders: Vec<Folder>,
}

impl Mail {
    /// The inbox a stand-in starts with, holding the made-up mail above.
    fn seeded(login: &str) -> Self {
        let messages = INBOX
            .iter()
            .enumerate()
            .map(|(number, (subject, body, received))| Stored {
                uid: u32::try_from(number).unwrap_or(0).saturating_add(1),
                flags: Vec::new(),
                received: (*received).to_owned(),
                raw: message(number, subject, body, received, login).into_bytes(),
            })
            .collect();
        Self {
            folders: vec![Folder {
                name: "INBOX".to_owned(),
                validity: 1,
                next_uid: u32::try_from(INBOX.len()).unwrap_or(0).saturating_add(1),
                messages,
            }],
        }
    }

    fn folder(&self, name: &str) -> Option<&Folder> {
        self.folders
            .iter()
            .find(|folder| folder.name.eq_ignore_ascii_case(name))
    }

    fn folder_mut(&mut self, name: &str) -> Option<&mut Folder> {
        self.folders
            .iter_mut()
            .find(|folder| folder.name.eq_ignore_ascii_case(name))
    }

    /// Keep `raw` in `folder`, as `APPEND` and the SMTP server do, and answer its UID.
    fn keep(
        &mut self,
        folder: &str,
        flags: Vec<String>,
        raw: Vec<u8>,
        received: String,
    ) -> Option<u32> {
        let folder = self.folder_mut(folder)?;
        let uid = folder.next_uid;
        folder.next_uid = folder.next_uid.saturating_add(1);
        folder.messages.push(Stored {
            uid,
            flags,
            received,
            raw,
        });
        Some(uid)
    }
}

/// One message the stand-in writes: made-up headers, and what it says.
fn message(number: usize, subject: &str, body: &str, received: &str, to: &str) -> String {
    format!(
        "Message-ID: <stand-in-{number}@example.test>\r\nFrom: {SENDER}\r\nTo: {to}\r\n\
Subject: {subject}\r\nDate: {received}\r\nMIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
    )
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
    let (imap_at, smtp_at) = (at(&imap)?, at(&smtp)?);
    let mail = Arc::new(Mutex::new(Mail::seeded(&serving.login)));
    let tls = Arc::new(tls);
    let taking = Arc::new(Taking {
        name: serving
            .names
            .first()
            .cloned()
            .unwrap_or_else(|| "localhost".to_owned()),
        login: serving.login,
        password: serving.password,
        oauth_token: serving.oauth_token,
        tls: Arc::clone(&tls),
        mail,
    });
    serve(
        imap,
        Arc::clone(&taking),
        serving.imap_wrapped,
        imap_connection,
    );
    serve(smtp, taking, serving.smtp_wrapped, smtp_connection);
    Ok(Started {
        imap: imap_at,
        smtp: smtp_at,
        certificate,
        tls,
    })
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
        self.send(&format!("{text}\r\n"))
    }

    /// Send `text` as it stands. Whether it was sent.
    fn send(&mut self, text: &str) -> bool {
        let to = self.0.get_mut();
        to.write_all(text.as_bytes()).is_ok() && to.flush().is_ok()
    }

    /// Read `size` bytes as they stand: a command's literal. Whether they came.
    fn literal(&mut self, size: usize) -> Option<Vec<u8>> {
        let mut raw = vec![0_u8; size];
        self.0.read_exact(&mut raw).ok()?;
        // The end of the command's line follows the literal.
        self.line()?;
        Some(raw)
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

/// `rest` without the quotes it may be written in.
fn unquoted(rest: &str) -> String {
    let rest = rest.trim();
    rest.strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(rest)
        .replace("\\\"", "\"")
        .replace("\\\\", "\\")
}

/// The size a command's literal is declared with (`{123}` at its end).
fn literal_size(line: &str) -> Option<usize> {
    line.rsplit_once('{')?
        .1
        .strip_suffix('}')?
        .trim_end_matches('+')
        .parse()
        .ok()
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

/// What one connection has done so far: whether it signed in, and the folder it selected.
struct Signed {
    taken: bool,
    selected: Option<String>,
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

/// The secured part of an IMAP connection: the sign-in, then its mailbox.
fn imap_secure<S: Read + Write>(lines: &mut Lines<S>, taking: &Taking) {
    let mut signed = Signed {
        taken: false,
        selected: None,
    };
    while let Some(line) = lines.line() {
        let (tag, command, rest) = parts(&line);
        // A command by message numbers of the other kind is answered as the plain one, which
        // is told which numbers its set names.
        let (uid, command, rest) = if command == "UID" {
            let (_, inner, rest) = parts(&format!("- {rest}"));
            (true, inner, rest)
        } else {
            (false, command, rest)
        };
        let said = match command.as_str() {
            "LOGIN" => {
                signed.taken = match arguments(&rest).as_slice() {
                    [login, password] => taking.takes(login, password),
                    _ => false,
                };
                lines.say(&signed_in(&tag, signed.taken))
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
                signed.taken = (plain
                    && token
                        .as_deref()
                        .and_then(plain_login)
                        .is_some_and(|(login, password)| taking.takes(&login, &password)))
                    || (xoauth2
                        && token
                            .as_deref()
                            .and_then(oauth2_login)
                            .is_some_and(|(user, token)| taking.takes_token(&user, &token)));
                lines.say(&signed_in(&tag, signed.taken))
            }
            "IDLE" if signed.taken => {
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
            "APPEND" => match (literal_size(&line), signed.taken) {
                (None, _) => lines.say(&format!("{tag} BAD APPEND needs a literal")),
                (Some(_), false) => lines.say(&format!("{tag} NO sign in first")),
                (Some(_), true) if !lines.say("+ go on") => false,
                (Some(size), true) => match lines.literal(size) {
                    Some(raw) => {
                        let folder = arguments(&rest).first().cloned().unwrap_or_default();
                        let flags = append_flags(&rest);
                        let mut mail = taking
                            .mail
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let validity = mail.folder(&folder).map_or(0, |folder| folder.validity);
                        match mail.keep(&folder, flags, raw, received_now()) {
                            Some(uid) => lines.say(&format!(
                                "{tag} OK [APPENDUID {validity} {uid}] APPEND completed"
                            )),
                            None => lines.say(&format!("{tag} NO [TRYCREATE] no such mailbox")),
                        }
                    }
                    None => false,
                },
            },
            _ => match answer(&tag, &command, &rest, uid, &mut signed, taking) {
                Some(reply) => lines.send(&reply),
                None if signed.taken => lines.say(&format!(
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

fn signed_in(tag: &str, taken: bool) -> String {
    if taken {
        format!("{tag} OK signed in")
    } else {
        format!("{tag} NO [AUTHENTICATIONFAILED] the mail stand-in does not take this login")
    }
}

/// The flags an `APPEND` gives the message it keeps, as `(\\Seen \\Draft)` writes them.
fn append_flags(rest: &str) -> Vec<String> {
    let Some((open, closed)) = rest
        .split_once('(')
        .and_then(|(_, tail)| tail.split_once(')'))
    else {
        return Vec::new();
    };
    let _ = closed;
    open.split_whitespace().map(str::to_owned).collect()
}

/// What one signed-in command answers: every untagged line, and the tagged one, as the
/// connection sends them; none for a command this stand-in does not know.
#[expect(
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "one IMAP command's dispatch, each answer built under the mailbox's one lock"
)]
fn answer(
    tag: &str,
    command: &str,
    rest: &str,
    uid: bool,
    signed: &mut Signed,
    taking: &Taking,
) -> Option<String> {
    match command {
        "CAPABILITY" => Some(format!(
            "* CAPABILITY {IMAP_OFFERS}\r\n{tag} OK CAPABILITY completed\r\n"
        )),
        "NOOP" | "CHECK" | "ENABLE" | "SUBSCRIBE" | "UNSUBSCRIBE" | "UNSELECT" => {
            Some(format!("{tag} OK {command} completed\r\n"))
        }
        "ID" => Some(format!("* ID NIL\r\n{tag} OK ID completed\r\n")),
        "CLOSE" => {
            signed.selected = None;
            Some(format!("{tag} OK CLOSE completed\r\n"))
        }
        "NAMESPACE" => Some(format!(
            "* NAMESPACE ((\"\" \"/\")) NIL NIL\r\n{tag} OK NAMESPACE completed\r\n"
        )),
        "LIST" | "LSUB" => {
            let pattern = arguments(rest).last().cloned().unwrap_or_default();
            if pattern.is_empty() {
                return Some(format!(
                    "* LIST (\\Noselect) \"/\" \"\"\r\n{tag} OK LIST completed\r\n"
                ));
            }
            let mail = taking
                .mail
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut reply = String::new();
            for folder in &mail.folders {
                let uses = match folder.name.as_str() {
                    "Sent" => " \\Sent",
                    "Drafts" => " \\Drafts",
                    "Trash" => " \\Trash",
                    "Junk" => " \\Junk",
                    _ => "",
                };
                let _ = write!(
                    reply,
                    "* LIST (\\HasNoChildren{uses}) \"/\" \"{}\"\r\n",
                    folder.name
                );
            }
            let _ = write!(reply, "{tag} OK LIST completed\r\n");
            Some(reply)
        }
        "SELECT" | "EXAMINE" => {
            let name = unquoted(rest.split_whitespace().next().unwrap_or_default());
            let mail = taking
                .mail
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let folder = mail.folder(&name)?;
            let (exists, validity, next) =
                (folder.messages.len(), folder.validity, folder.next_uid);
            signed.selected = Some(folder.name.clone());
            Some(format!(
                "* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)\r\n\
* {exists} EXISTS\r\n* 0 RECENT\r\n\
* OK [UIDVALIDITY {validity}] the mail stand-in's mailbox\r\n\
* OK [UIDNEXT {next}] the next message it takes\r\n\
* OK [PERMANENTFLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft \\*)] flags it keeps\r\n\
{tag} OK [READ-WRITE] {command} completed\r\n"
            ))
        }
        "STATUS" => {
            let name = arguments(rest).first().cloned().unwrap_or_default();
            let mail = taking
                .mail
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let folder = mail.folder(&name)?;
            let unseen = folder
                .messages
                .iter()
                .filter(|message| !has_flag(message, "\\Seen"))
                .count();
            let (exists, validity, next) =
                (folder.messages.len(), folder.validity, folder.next_uid);
            Some(format!(
                "* STATUS \"{}\" (MESSAGES {exists} RECENT 0 UIDNEXT {next} UIDVALIDITY {validity} UNSEEN {unseen})\r\n\
{tag} OK STATUS completed\r\n",
                folder.name
            ))
        }
        "FETCH" => {
            let selected = signed.selected.clone()?;
            let (set, items) = fetch_arguments(rest);
            let mail = taking
                .mail
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let folder = mail.folder(&selected)?;
            let mut reply = String::new();
            for (position, message) in folder.messages.iter().enumerate() {
                let position = u32::try_from(position).unwrap_or(0).saturating_add(1);
                let wanted = if uid {
                    uid_set(set, &folder.messages).contains(&message.uid)
                } else {
                    numbers(set, folder.messages.len()).contains(&position)
                };
                if wanted {
                    reply.push_str(&fetch_line(position, message, &items));
                }
            }
            let _ = write!(reply, "{tag} OK FETCH completed\r\n");
            Some(reply)
        }
        "SEARCH" => {
            let selected = signed.selected.clone()?;
            let keys = search_keys(rest);
            let mail = taking
                .mail
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let folder = mail.folder(&selected)?;
            let matching: Vec<String> = folder
                .messages
                .iter()
                .enumerate()
                .filter(|(_, message)| keys.iter().all(|key| key.matches(message)))
                .map(|(position, message)| {
                    if uid {
                        message.uid.to_string()
                    } else {
                        u32::try_from(position)
                            .unwrap_or(0)
                            .saturating_add(1)
                            .to_string()
                    }
                })
                .collect();
            Some(format!(
                "* SEARCH {}\r\n{tag} OK SEARCH completed\r\n",
                matching.join(" ")
            ))
        }
        "STORE" => {
            let selected = signed.selected.clone()?;
            let words: Vec<&str> = rest.split_whitespace().collect();
            let set = words.first().copied().unwrap_or_default();
            let modes = words
                .get(1)
                .copied()
                .unwrap_or_default()
                .to_ascii_uppercase();
            let adding = modes.starts_with('+');
            let silent = modes.contains("SILENT");
            let flags: Vec<String> = rest
                .split_once('(')
                .and_then(|(_, tail)| tail.split_once(')'))
                .map(|(flags, _)| flags.split_whitespace().map(str::to_owned).collect())
                .unwrap_or_default();
            let mut mail = taking
                .mail
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let folder = mail.folder_mut(&selected)?;
            let count = folder.messages.len();
            let wanted = if uid {
                uid_set(set, &folder.messages)
            } else {
                numbers(set, count)
            };
            let mut reply = String::new();
            for (position, message) in folder.messages.iter_mut().enumerate() {
                let position = u32::try_from(position).unwrap_or(0).saturating_add(1);
                if !wanted.contains(&message.uid) && !wanted.contains(&position) {
                    continue;
                }
                for flag in &flags {
                    if adding {
                        if !message.flags.contains(flag) {
                            message.flags.push(flag.clone());
                        }
                    } else {
                        message.flags.retain(|kept| kept != flag);
                    }
                }
                if !silent {
                    let _ = write!(
                        reply,
                        "* {position} FETCH (UID {} FLAGS ({}))\r\n",
                        message.uid,
                        message.flags.join(" ")
                    );
                }
            }
            let _ = write!(reply, "{tag} OK STORE completed\r\n");
            Some(reply)
        }
        "CREATE" => {
            let name = unquoted(rest);
            let mut mail = taking
                .mail
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if mail.folder(&name).is_some() {
                return Some(format!("{tag} NO mailbox already exists\r\n"));
            }
            let validity = u32::try_from(mail.folders.len())
                .unwrap_or(0)
                .saturating_add(100);
            mail.folders.push(Folder {
                name,
                validity,
                next_uid: 1,
                messages: Vec::new(),
            });
            Some(format!("{tag} OK CREATE completed\r\n"))
        }
        _ => None,
    }
}

fn has_flag(message: &Stored, flag: &str) -> bool {
    message
        .flags
        .iter()
        .any(|kept| kept.eq_ignore_ascii_case(flag))
}

/// The message set and the items of a `FETCH`: `1:3 (UID FLAGS …)`.
fn fetch_arguments(rest: &str) -> (&str, String) {
    let set = rest.split_whitespace().next().unwrap_or_default();
    let items = rest
        .split_once('(')
        .map_or_else(String::new, |(_, items)| items.to_ascii_uppercase());
    (set.trim_matches('('), items)
}

/// The positions a message set names, `*` being the folder's newest: `1`, `2:4`, `1,3`, `1:*`.
fn numbers(set: &str, count: usize) -> Vec<u32> {
    let newest = u32::try_from(count).unwrap_or(0);
    let mut wanted = Vec::new();
    for part in set.split(',') {
        let (first, last) = part.split_once(':').unwrap_or((part, part));
        let bound = |end: &str| {
            if end == "*" {
                newest
            } else {
                end.parse().unwrap_or(0)
            }
        };
        let (first, last) = (bound(first), bound(last));
        if first == 0 || last == 0 {
            continue;
        }
        wanted.extend(first.min(last)..=first.max(last));
    }
    wanted
}

/// The UIDs a message set names, `*` being the folder's newest.
fn uid_set(set: &str, messages: &[Stored]) -> Vec<u32> {
    let newest = messages
        .iter()
        .map(|message| message.uid)
        .max()
        .unwrap_or(0);
    let mut wanted = Vec::new();
    for part in set.split(',') {
        let (first, last) = part.split_once(':').unwrap_or((part, part));
        let bound = |end: &str| {
            if end == "*" {
                newest
            } else {
                end.parse().unwrap_or(0)
            }
        };
        let (first, last) = (bound(first), bound(last));
        wanted.extend(
            messages
                .iter()
                .map(|message| message.uid)
                .filter(|uid| *uid >= first.min(last) && *uid <= first.max(last)),
        );
    }
    wanted
}

/// One message as `FETCH` answers it: its position, its flags, when it arrived, and, when
/// asked for, its words as a literal.
fn fetch_line(position: u32, message: &Stored, items: &str) -> String {
    let mut said = format!(
        "* {position} FETCH (UID {} FLAGS ({})",
        message.uid,
        message.flags.join(" ")
    );
    if items.contains("INTERNALDATE") {
        let _ = write!(said, " INTERNALDATE \"{}\"", message.received);
    }
    if items.contains("BODY") || items.contains("RFC822") {
        let _ = write!(said, " BODY[] {{{}}}\r\n", message.raw.len());
        said.push_str(&String::from_utf8_lossy(&message.raw));
    }
    said.push_str(")\r\n");
    said
}

/// One key of a search, as far as this server answers them. A message has to match every key
/// of its search.
enum SearchKey {
    /// Words anywhere in the message.
    Text(String),
    /// Words in the header of this name alone.
    Header(String, String),
    Unseen,
    Seen,
    Flagged,
    /// A range of UIDs, both ends included.
    Uids(u32, u32),
    /// `ALL`, and what this server does not know.
    Any,
}

impl SearchKey {
    fn matches(&self, message: &Stored) -> bool {
        let raw = String::from_utf8_lossy(&message.raw).to_ascii_lowercase();
        match self {
            Self::Text(wanted) => raw.contains(wanted),
            Self::Header(name, wanted) => {
                let headers = raw.split("\r\n\r\n").next().unwrap_or_default();
                let prefix = format!("{name}:");
                headers.lines().any(|line| {
                    line.strip_prefix(&prefix)
                        .is_some_and(|value| value.contains(wanted))
                })
            }
            Self::Unseen => !has_flag(message, "\\Seen"),
            Self::Seen => has_flag(message, "\\Seen"),
            Self::Flagged => has_flag(message, "\\Flagged"),
            Self::Uids(first, last) => (*first..=*last).contains(&message.uid),
            Self::Any => true,
        }
    }
}

/// The keys of the search `query` asks: `TEXT`, `FROM`, `TO`, `SUBJECT`, `HEADER name`, each
/// with the words it looks for, `UNSEEN`, `SEEN`, `FLAGGED`, and `UID` with a range.
fn search_keys(query: &str) -> Vec<SearchKey> {
    let mut words = search_words(query).into_iter();
    let mut keys = Vec::new();
    while let Some(word) = words.next() {
        let mut wanted = || words.next().unwrap_or_default().to_ascii_lowercase();
        keys.push(match word.to_ascii_uppercase().as_str() {
            "TEXT" => SearchKey::Text(wanted()),
            "FROM" | "TO" | "SUBJECT" => SearchKey::Header(word.to_ascii_lowercase(), wanted()),
            "HEADER" => {
                let name = wanted();
                SearchKey::Header(name, wanted())
            }
            "UNSEEN" => SearchKey::Unseen,
            "SEEN" => SearchKey::Seen,
            "FLAGGED" => SearchKey::Flagged,
            "UID" => {
                let set = wanted();
                let (first, last) = set.split_once(':').unwrap_or((&set, &set));
                SearchKey::Uids(
                    first.parse().unwrap_or(0),
                    if last == "*" {
                        u32::MAX
                    } else {
                        last.parse().unwrap_or(u32::MAX)
                    },
                )
            }
            _ => SearchKey::Any,
        });
    }
    keys
}

/// The words of a search, a quoted string as one word without its quotes.
fn search_words(query: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut letters = query.chars();
    while let Some(letter) = letters.next() {
        match letter {
            '\\' if quoted => word.extend(letters.next()),
            '"' => {
                if quoted {
                    words.push(std::mem::take(&mut word));
                }
                quoted = !quoted;
            }
            ' ' if !quoted => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            letter => word.push(letter),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// When this message arrived, as `INTERNALDATE` writes it: the clock now, in UTC.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "a calendar date is arithmetic over the clock, bounded by the epoch's own days"
)]
fn received_now() -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let days = i64::try_from(seconds / 86_400).unwrap_or(0);
    let rest = seconds % 86_400;
    // The day counted from 1970-01-01, as a civil date.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let of_era = shifted - era * 146_097;
    let year_of_era = (of_era - of_era / 1_460 + of_era / 36_524 - of_era / 146_096) / 365;
    let day_of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_number = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_number + 2) / 5 + 1;
    let month = if month_number < 10 {
        month_number + 3
    } else {
        month_number - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    let month = MONTHS
        .get(usize::try_from(month).unwrap_or(0).saturating_sub(1))
        .copied()
        .unwrap_or("Jan");
    format!(
        "{day:02}-{month}-{year} {:02}:{:02}:{:02} +0000",
        rest / 3_600,
        (rest / 60) % 60,
        rest % 60
    )
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

/// The secured part of an SMTP connection: what is offered, the sign-in, and the mail it
/// takes, kept in the inbox.
fn smtp_secure<S: Read + Write>(lines: &mut Lines<S>, taking: &Taking) {
    let mut signed_in = false;
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
                    .is_some_and(|(login, password)| taking.takes(login, password));
                let accepted_token = mechanism == "XOAUTH2"
                    && said
                        .as_ref()
                        .is_some_and(|(user, token)| taking.takes_token(user, token));
                signed_in = accepted || accepted_token;
                if signed_in {
                    lines.say("235 2.7.0 signed in")
                } else {
                    lines.say("535 5.7.8 the mail stand-in does not take this login")
                }
            }
            "MAIL" | "RCPT" if signed_in => lines.say("250 ok"),
            "DATA" if signed_in => {
                if !lines.say("354 go ahead") {
                    return;
                }
                let mut raw = Vec::new();
                let mut complete = false;
                while let Some(line) = lines.line() {
                    if line == "." {
                        complete = true;
                        break;
                    }
                    raw.extend_from_slice(line.as_bytes());
                    raw.extend_from_slice(b"\r\n");
                }
                if !complete {
                    return;
                }
                let received = received_now();
                let kept = taking
                    .mail
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .keep("INBOX", Vec::new(), raw, received);
                if kept.is_some() {
                    lines.say("250 2.0.0 queued in the stand-in's inbox")
                } else {
                    lines.say("451 4.3.0 the stand-in's inbox is not there")
                }
            }
            "MAIL" | "RCPT" => lines.say("530 5.7.0 sign in first"),
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
