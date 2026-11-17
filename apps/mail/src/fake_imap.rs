//! A small IMAP server for tests, laid out like the servers people use.
//!
//! It speaks enough `IMAP4rev1` for the engine's whole life with an account:
//! LOGIN, CAPABILITY, LIST with special-use marks (RFC 6154), SELECT with
//! UIDVALIDITY and UIDNEXT, FETCH by position and by UID (`n:*` included),
//! UID SEARCH (TEXT, HEADER, FLAGGED, ALL), UID STORE, UID MOVE, UID
//! EXPUNGE, APPEND (a literal, with flags), CREATE, NOOP and IDLE, over TLS with a throwaway
//! certificate: from the first byte, or on a port that starts in the clear and upgrades with
//! STARTTLS before it lets anyone sign in. A test changes the mailbox as another client would
//! (new mail, a flag) and the server tells an idling connection, as a real one does.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

#[derive(Clone, Copy, Debug)]
pub enum Layout {
    Gmail,
    Outlook,
    /// A plain server, as Dovecot serves a new account with its stock
    /// configuration: only the inbox exists, a folder carries its special-use
    /// mark once a client creates it under the name the server knows, the
    /// hierarchy separator is a dot, and nothing files a sent message.
    Plain,
}

impl Layout {
    fn folders(self) -> Vec<(&'static str, &'static [&'static str])> {
        match self {
            Self::Gmail => vec![
                ("INBOX", &[]),
                ("[Gmail]/All Mail", &["\\All"]),
                ("[Gmail]/Sent Mail", &["\\Sent"]),
                ("[Gmail]/Drafts", &["\\Drafts"]),
                ("[Gmail]/Spam", &["\\Junk"]),
                ("[Gmail]/Trash", &["\\Trash"]),
            ],
            Self::Outlook => vec![
                ("INBOX", &[]),
                ("Archive", &[]),
                ("Sent Items", &[]),
                ("Drafts", &[]),
                ("Junk Email", &[]),
                ("Deleted Items", &[]),
            ],
            Self::Plain => {
                let inbox: (&'static str, &'static [&'static str]) = ("INBOX", &[]);
                vec![inbox]
            }
        }
    }

    /// The special-use marks a folder created as `name` carries.
    fn uses_of(self, name: &str) -> &'static [&'static str] {
        match (self, name) {
            (Self::Plain, "Drafts") => &["\\Drafts"],
            (Self::Plain, "Sent" | "Sent Messages") => &["\\Sent"],
            (Self::Plain, "Junk") => &["\\Junk"],
            (Self::Plain, "Trash") => &["\\Trash"],
            _ => &[],
        }
    }

    const fn separator(self) -> char {
        match self {
            Self::Gmail | Self::Outlook => '/',
            Self::Plain => '.',
        }
    }

    /// What the server says it can do once a connection is secure. Gmail
    /// names itself, which is how a client knows that it files a sent
    /// message on its own.
    const fn capabilities(self) -> &'static str {
        match self {
            Self::Gmail => "IMAP4rev1 IDLE MOVE UIDPLUS SPECIAL-USE X-GM-EXT-1",
            Self::Outlook => "IMAP4rev1 IDLE MOVE UIDPLUS SPECIAL-USE",
            Self::Plain => "IMAP4rev1 ID ENABLE IDLE MOVE UIDPLUS SPECIAL-USE",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Stored {
    pub uid: u32,
    pub flags: Vec<String>,
    pub raw: Vec<u8>,
}

#[derive(Debug)]
struct Box_ {
    name: String,
    uses: Vec<&'static str>,
    validity: u32,
    next_uid: u32,
    messages: Vec<Stored>,
}

#[derive(Debug, Default)]
struct State {
    boxes: Vec<Box_>,
}

impl State {
    fn find(&mut self, name: &str) -> Option<&mut Box_> {
        self.boxes
            .iter_mut()
            .find(|mailbox| mailbox.name.eq_ignore_ascii_case(name))
    }
}

#[derive(Clone)]
pub struct FakeImap {
    pub port: u16,
    layout: Layout,
    state: Arc<Mutex<State>>,
    changed: Arc<tokio::sync::watch::Sender<usize>>,
    idling: Arc<std::sync::atomic::AtomicUsize>,
    logins: Arc<std::sync::atomic::AtomicUsize>,
    cleartext_logins: Arc<std::sync::atomic::AtomicUsize>,
    refuses_appends: Arc<std::sync::atomic::AtomicBool>,
}

impl FakeImap {
    pub async fn start(layout: Layout, tls: Arc<rustls::ServerConfig>) -> Self {
        Self::listen(layout, tls, false).await
    }

    /// A server on a port that starts in the clear, as IMAP's port 143
    /// does: it offers STARTTLS, lets nobody sign in before the upgrade, and
    /// after it is the same server.
    pub async fn start_starttls(layout: Layout, tls: Arc<rustls::ServerConfig>) -> Self {
        Self::listen(layout, tls, true).await
    }

    async fn listen(layout: Layout, tls: Arc<rustls::ServerConfig>, upgrades: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(State {
            boxes: layout
                .folders()
                .into_iter()
                .enumerate()
                .map(|(index, (name, uses))| Box_ {
                    name: name.to_owned(),
                    uses: uses.to_vec(),
                    validity: 100 + u32::try_from(index).unwrap(),
                    next_uid: 1,
                    messages: Vec::new(),
                })
                .collect(),
        }));
        let server = Self {
            port,
            layout,
            state,
            changed: Arc::new(tokio::sync::watch::Sender::new(0)),
            idling: Arc::default(),
            logins: Arc::default(),
            cleartext_logins: Arc::default(),
            refuses_appends: Arc::default(),
        };
        let serving = server.clone();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let tls = Arc::clone(&tls);
                let connection = serving.clone();
                tokio::spawn(async move {
                    let tcp = if upgrades {
                        let Some(tcp) = connection.clear_until_starttls(tcp).await else {
                            return;
                        };
                        tcp
                    } else {
                        tcp
                    };
                    let Ok(stream) = tokio_rustls::TlsAcceptor::from(tls).accept(tcp).await else {
                        return;
                    };
                    // A connection that upgraded was greeted in the clear.
                    connection.serve(stream, !upgrades).await;
                });
            }
        });
        server
    }

    /// The cleartext start of a connection, until its client asks for the
    /// upgrade: what a server on port 143 answers before it is secure.
    async fn clear_until_starttls(
        &self,
        tcp: tokio::net::TcpStream,
    ) -> Option<tokio::net::TcpStream> {
        let mut plain = BufReader::new(tcp);
        plain
            .write_all(b"* OK [CAPABILITY IMAP4rev1 STARTTLS LOGINDISABLED] ready\r\n")
            .await
            .ok()?;
        plain.flush().await.ok()?;
        let mut line = String::new();
        loop {
            line.clear();
            if plain.read_line(&mut line).await.ok()? == 0 {
                return None;
            }
            let text = line.trim_end();
            let (tag, command) = text.split_once(' ').unwrap_or((text, ""));
            let upper = command.to_ascii_uppercase();
            let reply = if upper.starts_with("STARTTLS") {
                plain
                    .write_all(format!("{tag} OK begin TLS negotiation now\r\n").as_bytes())
                    .await
                    .ok()?;
                plain.flush().await.ok()?;
                // The client waits for this answer before its ClientHello,
                // so nothing of the handshake is buffered here.
                return Some(plain.into_inner());
            } else if upper.starts_with("CAPABILITY") {
                format!("* CAPABILITY IMAP4rev1 STARTTLS LOGINDISABLED\r\n{tag} OK\r\n")
            } else if upper.starts_with("LOGIN") || upper.starts_with("AUTHENTICATE") {
                self.cleartext_logins
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                format!("{tag} NO [PRIVACYREQUIRED] sign-in needs a secure connection\r\n")
            } else if upper.starts_with("LOGOUT") {
                let _ = plain
                    .write_all(format!("* BYE\r\n{tag} OK LOGOUT\r\n").as_bytes())
                    .await;
                let _ = plain.flush().await;
                return None;
            } else if upper.starts_with("NOOP") {
                format!("{tag} OK done\r\n")
            } else {
                format!("{tag} BAD not before STARTTLS\r\n")
            };
            plain.write_all(reply.as_bytes()).await.ok()?;
            plain.flush().await.ok()?;
        }
    }

    fn announce(&self) {
        self.changed.send_modify(|changes| *changes += 1);
    }

    pub fn append(&self, folder: &str, raw: &str, flags: &[&str]) -> u32 {
        let uid = {
            let mut state = self.state.lock().unwrap();
            let mailbox = state.find(folder).unwrap();
            let uid = mailbox.next_uid;
            mailbox.next_uid += 1;
            mailbox.messages.push(Stored {
                uid,
                flags: flags.iter().map(|flag| (*flag).to_owned()).collect(),
                raw: raw.replace('\n', "\r\n").into_bytes(),
            });
            drop(state);
            uid
        };
        self.announce();
        uid
    }

    pub fn flag(&self, folder: &str, uid: u32, flag: &str) {
        {
            let mut state = self.state.lock().unwrap();
            let message = state
                .find(folder)
                .and_then(|mailbox| mailbox.messages.iter_mut().find(|m| m.uid == uid))
                .unwrap();
            if !message.flags.iter().any(|kept| kept == flag) {
                message.flags.push(flag.to_owned());
            }
            drop(state);
        }
        self.announce();
    }

    pub fn idling(&self) -> usize {
        self.idling.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// How many times anyone signed in, over every connection.
    pub fn logins(&self) -> usize {
        self.logins.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// How many sign-ins were tried before their connection was secure. A
    /// client that keeps a secret safe never tries one.
    pub fn cleartext_logins(&self) -> usize {
        self.cleartext_logins
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// From now on the server takes no message a client appends, as one
    /// whose account is over its quota.
    pub fn refuse_appends(&self) {
        self.refuses_appends
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// The names of the folders the server holds, in the order they came to be.
    pub fn folders(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap()
            .boxes
            .iter()
            .map(|mailbox| mailbox.name.clone())
            .collect()
    }

    pub fn messages(&self, folder: &str) -> Vec<Stored> {
        self.state
            .lock()
            .unwrap()
            .find(folder)
            .map(|mailbox| mailbox.messages.clone())
            .unwrap_or_default()
    }

    async fn serve<S>(&self, stream: S, greet: bool)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let (read, mut write) = tokio::io::split(stream);
        let mut reader = BufReader::new(read);
        let mut selected: Option<String> = None;
        let mut news = self.changed.subscribe();
        let greeting = format!("* OK [CAPABILITY {}] ready\r\n", self.layout.capabilities());
        if greet && write.write_all(greeting.as_bytes()).await.is_err() {
            return;
        }
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let text = line.trim_end().to_owned();
            let (tag, command) = text.split_once(' ').unwrap_or((&text, ""));
            let upper = command.to_ascii_uppercase();
            let reply = if upper.starts_with("APPEND ") {
                if self
                    .refuses_appends
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    format!("{tag} NO quota exceeded\r\n")
                } else if self.holds(&append_target(command).0) {
                    match receive_literal(&mut reader, &mut write, command).await {
                        Ok(raw) => self.appended(tag, command, raw),
                        Err(Some(refused)) => format!("{tag} {refused}\r\n"),
                        Err(None) => return,
                    }
                } else {
                    // A server refuses a folder it does not hold before it
                    // takes the message, and says a client may create it.
                    format!("{tag} NO [TRYCREATE] no such mailbox\r\n")
                }
            } else if upper.starts_with("LOGOUT") {
                let _ = write
                    .write_all(format!("* BYE\r\n{tag} OK LOGOUT\r\n").as_bytes())
                    .await;
                return;
            } else if upper.starts_with("IDLE") {
                if write.write_all(b"+ idling\r\n").await.is_err() {
                    return;
                }
                let _ = write.flush().await;
                // New mail or a flag ends the wait with an untagged answer, and
                // the client then says DONE. A change made while this connection
                // was between commands is one its receiver has not seen, so the
                // wait answers that at once rather than sleeping through it.
                let mut done = String::new();
                self.idling
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let heard = tokio::select! {
                    _ = news.changed() => Some(true),
                    read = reader.read_line(&mut done) => {
                        read.is_ok_and(|read| read > 0).then_some(false)
                    }
                };
                self.idling
                    .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                let Some(heard) = heard else {
                    return;
                };
                if heard {
                    let count = selected
                        .as_deref()
                        .map_or(0, |name| self.messages(name).len());
                    if write
                        .write_all(format!("* {count} EXISTS\r\n").as_bytes())
                        .await
                        .is_err()
                    {
                        return;
                    }
                    let _ = write.flush().await;
                    if reader.read_line(&mut done).await.unwrap_or(0) == 0 {
                        return;
                    }
                }
                format!("{tag} OK IDLE terminated\r\n")
            } else {
                self.answer(tag, command, &upper, &mut selected)
            };
            if write.write_all(reply.as_bytes()).await.is_err() {
                return;
            }
            let _ = write.flush().await;
        }
    }

    /// Whether the server holds a folder called `name`.
    fn holds(&self, name: &str) -> bool {
        self.state.lock().unwrap().find(name).is_some()
    }

    fn appended(&self, tag: &str, command: &str, raw: Vec<u8>) -> String {
        let (name, flags) = append_target(command);
        let flags: Vec<String> = flags
            .trim()
            .trim_start_matches('(')
            .trim_end_matches(')')
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let mut state = self.state.lock().unwrap();
        let Some(mailbox) = state.find(&name) else {
            return format!("{tag} NO [TRYCREATE] no such mailbox\r\n");
        };
        let uid = mailbox.next_uid;
        mailbox.next_uid += 1;
        mailbox.messages.push(Stored { uid, flags, raw });
        let validity = mailbox.validity;
        drop(state);
        self.announce();
        format!("{tag} OK [APPENDUID {validity} {uid}] APPEND completed\r\n")
    }

    #[expect(
        clippy::too_many_lines,
        clippy::significant_drop_tightening,
        reason = "one IMAP command's dispatch, all of it under the one mailbox lock, as a server answers it"
    )]
    fn answer(
        &self,
        tag: &str,
        command: &str,
        upper: &str,
        selected: &mut Option<String>,
    ) -> String {
        let mut state = self.state.lock().unwrap();
        if upper.starts_with("LOGIN") {
            self.logins
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            return format!("{tag} OK done\r\n");
        }
        if upper.starts_with("NOOP") {
            return format!("{tag} OK done\r\n");
        }
        if upper.starts_with("CAPABILITY") {
            return format!(
                "* CAPABILITY {}\r\n{tag} OK\r\n",
                self.layout.capabilities()
            );
        }
        if upper.starts_with("LIST") {
            let mut out = String::new();
            for mailbox in &state.boxes {
                let mut attributes = vec!["\\HasNoChildren"];
                attributes.extend(mailbox.uses.iter().copied());
                let _ = write!(
                    out,
                    "* LIST ({}) \"{}\" \"{}\"\r\n",
                    attributes.join(" "),
                    self.layout.separator(),
                    mailbox.name
                );
            }
            return format!("{out}{tag} OK LIST\r\n");
        }
        if upper.starts_with("CREATE ") {
            let name = unquote(command.split_once(' ').map_or("", |(_, rest)| rest));
            if state.find(&name).is_some() {
                return format!("{tag} NO mailbox already exists\r\n");
            }
            let validity = 100 + u32::try_from(state.boxes.len()).unwrap();
            let uses = self.layout.uses_of(&name).to_vec();
            state.boxes.push(Box_ {
                name,
                uses,
                validity,
                next_uid: 1,
                messages: Vec::new(),
            });
            return format!("{tag} OK CREATE\r\n");
        }
        if upper.starts_with("SELECT") || upper.starts_with("EXAMINE") {
            let name = unquote(command.split_once(' ').map_or("", |(_, rest)| rest));
            let Some(mailbox) = state.find(&name) else {
                return format!("{tag} NO no such mailbox\r\n");
            };
            *selected = Some(mailbox.name.clone());
            return format!(
                "* {} EXISTS\r\n* 0 RECENT\r\n* FLAGS (\\Seen \\Flagged \\Deleted)\r\n\
* OK [UIDVALIDITY {}] valid\r\n* OK [UIDNEXT {}] next\r\n{tag} OK [READ-WRITE] SELECT\r\n",
                mailbox.messages.len(),
                mailbox.validity,
                mailbox.next_uid
            );
        }
        let Some(mailbox) = selected.as_deref().and_then(|name| state.find(name)) else {
            return format!("{tag} BAD nothing selected\r\n");
        };
        let words: Vec<&str> = command.split_whitespace().collect();
        if upper.starts_with("FETCH ") {
            let set = words.get(1).copied().unwrap_or("");
            let positions = sequence(set, mailbox.messages.len());
            let mut out = String::new();
            for position in positions {
                if let Some(message) = mailbox.messages.get(position - 1) {
                    out.push_str(&fetch_line(position, message, upper));
                }
            }
            return format!("{out}{tag} OK FETCH\r\n");
        }
        if upper.starts_with("UID FETCH ") {
            let set = words.get(2).copied().unwrap_or("");
            let wanted = uid_set(set, &mailbox.messages);
            let mut out = String::new();
            for (index, message) in mailbox.messages.iter().enumerate() {
                if wanted.contains(&message.uid) {
                    out.push_str(&fetch_line(index + 1, message, upper));
                }
            }
            return format!("{out}{tag} OK UID FETCH\r\n");
        }
        if upper.starts_with("UID SEARCH ") {
            let keys = search_keys(command.get("UID SEARCH ".len()..).unwrap_or(""));
            let found: Vec<String> = mailbox
                .messages
                .iter()
                .filter(|message| keys.iter().all(|key| key.matches(message)))
                .map(|message| message.uid.to_string())
                .collect();
            return format!("* SEARCH {}\r\n{tag} OK SEARCH\r\n", found.join(" "));
        }
        if upper.starts_with("UID STORE ") {
            let uid: u32 = words.get(2).and_then(|uid| uid.parse().ok()).unwrap_or(0);
            let adding = words.get(3).is_some_and(|op| op.starts_with('+'));
            let flags: Vec<String> = words
                .get(4..)
                .unwrap_or_default()
                .iter()
                .map(|flag| flag.trim_matches(|c| c == '(' || c == ')').to_owned())
                .collect();
            if let Some(message) = mailbox.messages.iter_mut().find(|m| m.uid == uid) {
                for flag in flags {
                    if adding {
                        if !message.flags.contains(&flag) {
                            message.flags.push(flag);
                        }
                    } else {
                        message.flags.retain(|kept| *kept != flag);
                    }
                }
            }
            return format!("{tag} OK STORE\r\n");
        }
        if upper.starts_with("UID MOVE ") {
            let uid: u32 = words.get(2).and_then(|uid| uid.parse().ok()).unwrap_or(0);
            let destination = unquote(command.splitn(4, ' ').nth(3).unwrap_or_default());
            let Some(index) = mailbox.messages.iter().position(|m| m.uid == uid) else {
                return format!("{tag} NO no such message\r\n");
            };
            let mut moved = mailbox.messages.remove(index);
            let Some(target) = state.find(&destination) else {
                return format!("{tag} NO no such mailbox\r\n");
            };
            moved.uid = target.next_uid;
            target.next_uid += 1;
            target.messages.push(moved);
            return format!("* {} EXPUNGE\r\n{tag} OK MOVE\r\n", index + 1);
        }
        if upper.starts_with("UID EXPUNGE ") {
            mailbox
                .messages
                .retain(|message| !message.flags.iter().any(|flag| flag == "\\Deleted"));
            return format!("{tag} OK EXPUNGE\r\n");
        }
        format!("{tag} BAD unknown command\r\n")
    }
}

/// One key of a search, as far as this server answers them. A message has
/// to match every key of its search.
enum SearchKey {
    /// Words anywhere in the message.
    Text(String),
    /// Words in the header of this name alone: a reply's References name
    /// the message it answers too, and are not its Message-ID.
    Header(String, String),
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
            Self::Flagged => message.flags.iter().any(|flag| flag == "\\Flagged"),
            Self::Uids(first, last) => (*first..=*last).contains(&message.uid),
            Self::Any => true,
        }
    }
}

/// The keys of the search `query` asks: `TEXT`, `FROM`, `TO`, `SUBJECT`,
/// `HEADER name`, each with the words it looks for, `FLAGGED`, and `UID`
/// with a range.
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
            "FLAGGED" => SearchKey::Flagged,
            "UID" => {
                let set = wanted();
                let (first, last) = set.split_once(':').unwrap_or((&set, &set));
                SearchKey::Uids(first.parse().unwrap_or(0), last.parse().unwrap_or(u32::MAX))
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

/// The folder an `APPEND` names and the flags it gives, from its command
/// line.
fn append_target(command: &str) -> (String, &str) {
    let head = command.rsplit_once('{').map_or(command, |(head, _)| head);
    let rest = head.trim_start()["APPEND ".len()..].trim();
    if let Some(quoted) = rest.strip_prefix('"') {
        let end = quoted.find('"').unwrap_or(quoted.len());
        (
            quoted[..end].to_owned(),
            quoted[end..].trim_start_matches('"'),
        )
    } else {
        let (name, flags) = rest.split_once(' ').unwrap_or((rest, ""));
        (name.to_owned(), flags)
    }
}

async fn receive_literal<R, W>(
    reader: &mut BufReader<R>,
    write: &mut W,
    command: &str,
) -> Result<Vec<u8>, Option<String>>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let size = command
        .rsplit_once('{')
        .and_then(|(_, size)| size.strip_suffix('}'))
        .and_then(|size| size.parse::<usize>().ok())
        .ok_or_else(|| Some("BAD APPEND needs a literal".to_owned()))?;
    write.write_all(b"+ go on\r\n").await.map_err(|_| None)?;
    write.flush().await.map_err(|_| None)?;
    let mut raw = vec![0_u8; size];
    tokio::io::AsyncReadExt::read_exact(reader, &mut raw)
        .await
        .map_err(|_| None)?;
    let mut rest = String::new();
    if reader.read_line(&mut rest).await.unwrap_or(0) == 0 {
        return Err(None);
    }
    Ok(raw)
}

fn fetch_line(position: usize, message: &Stored, asked: &str) -> String {
    let mut items = format!("UID {}", message.uid);
    if asked.contains("FLAGS") {
        let _ = write!(items, " FLAGS ({})", message.flags.join(" "));
    }
    if asked.contains("BODY") {
        let _ = write!(
            items,
            " INTERNALDATE \"01-Jan-2026 09:{:02}:00 +0000\" BODY[] {{{}}}\r\n{}",
            message.uid % 60,
            message.raw.len(),
            String::from_utf8_lossy(&message.raw)
        );
    }
    format!("* {position} FETCH ({items})\r\n")
}

fn sequence(set: &str, count: usize) -> Vec<usize> {
    let mut positions = Vec::new();
    for part in set.split(',') {
        let (first, last) = part.split_once(':').unwrap_or((part, part));
        let bound = |end: &str| {
            if end == "*" {
                count
            } else {
                end.parse().unwrap_or(0)
            }
        };
        let (first, last) = (bound(first), bound(last));
        positions.extend(first.min(last)..=first.max(last));
    }
    positions.retain(|position| *position >= 1 && *position <= count);
    positions
}

fn uid_set(set: &str, messages: &[Stored]) -> Vec<u32> {
    let newest = messages.iter().map(|m| m.uid).max().unwrap_or(0);
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
                .map(|m| m.uid)
                .filter(|uid| *uid >= first.min(last) && *uid <= first.max(last)),
        );
    }
    wanted
}

fn unquote(text: &str) -> String {
    let text = text.trim();
    text.strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(text)
        .replace("\\\"", "\"")
        .replace("\\\\", "\\")
}
