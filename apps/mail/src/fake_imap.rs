//! A small IMAP server for tests, laid out like the servers people use.
//!
//! It speaks enough `IMAP4rev1` for the engine's whole life with an account:
//! LOGIN, CAPABILITY, LIST with special-use marks (RFC 6154), SELECT with
//! UIDVALIDITY and UIDNEXT, FETCH by position and by UID (`n:*` included),
//! UID SEARCH (TEXT, HEADER, FLAGGED, ALL), UID STORE, UID MOVE, UID
//! EXPUNGE, NOOP and IDLE, over TLS with a throwaway certificate. A test
//! changes the mailbox as another client would (new mail, a flag) and the
//! server tells an idling connection, as a real one does.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::Notify;

/// The folders a server has, named and marked as one provider does.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Layout {
    /// Gmail: `[Gmail]/…` folders marked with their use; archiving is
    /// "All Mail".
    Gmail,
    /// Outlook: plain names, no marks.
    Outlook,
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
        }
    }
}

/// One message in a folder.
#[derive(Clone, Debug)]
pub(crate) struct Stored {
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

/// A running server: where it listens, and the mailboxes it serves.
#[derive(Clone)]
pub(crate) struct FakeImap {
    pub port: u16,
    state: Arc<Mutex<State>>,
    changed: Arc<Notify>,
    /// How many connections are waiting in IDLE now.
    idling: Arc<std::sync::atomic::AtomicUsize>,
}

impl FakeImap {
    /// Serve `layout`'s folders, empty, on a loopback port, on the current
    /// tokio runtime.
    pub(crate) async fn start(layout: Layout, tls: Arc<rustls::ServerConfig>) -> Self {
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
            state,
            changed: Arc::new(Notify::new()),
            idling: Arc::default(),
        };
        let serving = server.clone();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let tls = Arc::clone(&tls);
                let connection = serving.clone();
                tokio::spawn(async move {
                    let Ok(stream) = tokio_rustls::TlsAcceptor::from(tls).accept(tcp).await else {
                        return;
                    };
                    connection.serve(stream).await;
                });
            }
        });
        server
    }

    /// Put a message into `folder` as another client or a delivery would,
    /// and tell idling connections. Its UID.
    pub(crate) fn append(&self, folder: &str, raw: &str, flags: &[&str]) -> u32 {
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
        self.changed.notify_waiters();
        uid
    }

    /// Set `flag` on a message, as another client would.
    pub(crate) fn flag(&self, folder: &str, uid: u32, flag: &str) {
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
        self.changed.notify_waiters();
    }

    /// How many connections wait in IDLE for news of the mailbox.
    pub(crate) fn idling(&self) -> usize {
        self.idling.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn messages(&self, folder: &str) -> Vec<Stored> {
        self.state
            .lock()
            .unwrap()
            .find(folder)
            .map(|mailbox| mailbox.messages.clone())
            .unwrap_or_default()
    }

    async fn serve<S>(&self, stream: S)
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let (read, mut write) = tokio::io::split(stream);
        let mut reader = BufReader::new(read);
        let mut selected: Option<String> = None;
        if write
            .write_all(b"* OK [CAPABILITY IMAP4rev1 IDLE MOVE UIDPLUS SPECIAL-USE] ready\r\n")
            .await
            .is_err()
        {
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
            let reply = if upper.starts_with("LOGOUT") {
                let _ = write
                    .write_all(format!("* BYE\r\n{tag} OK LOGOUT\r\n").as_bytes())
                    .await;
                return;
            } else if upper.starts_with("IDLE") {
                if write.write_all(b"+ idling\r\n").await.is_err() {
                    return;
                }
                let _ = write.flush().await;
                // New mail or a flag ends the wait with an untagged answer;
                // the client then says DONE.
                let mut done = String::new();
                let waiting = self.changed.notified();
                tokio::pin!(waiting);
                waiting.as_mut().enable();
                self.idling
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let leave = || {
                    self.idling
                        .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                };
                tokio::select! {
                    () = waiting => {
                        leave();
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
                    read = reader.read_line(&mut done) => {
                        leave();
                        if read.unwrap_or(0) == 0 {
                            return;
                        }
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

    /// The whole answer to one command but IDLE and LOGOUT.
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
        if upper.starts_with("LOGIN") || upper.starts_with("NOOP") {
            return format!("{tag} OK done\r\n");
        }
        if upper.starts_with("CAPABILITY") {
            return format!("* CAPABILITY IMAP4rev1 IDLE MOVE UIDPLUS SPECIAL-USE\r\n{tag} OK\r\n");
        }
        if upper.starts_with("LIST") {
            let mut out = String::new();
            for mailbox in &state.boxes {
                let mut attributes = vec!["\\HasNoChildren"];
                attributes.extend(mailbox.uses.iter().copied());
                let _ = write!(
                    out,
                    "* LIST ({}) \"/\" \"{}\"\r\n",
                    attributes.join(" "),
                    mailbox.name
                );
            }
            return format!("{out}{tag} OK LIST\r\n");
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
            let query = command.get("UID SEARCH ".len()..).unwrap_or("");
            let upper_query = query.to_ascii_uppercase();
            let needle = |prefix: &str| {
                unquote(query.get(prefix.len()..).unwrap_or("")).to_ascii_lowercase()
            };
            let found: Vec<String> = mailbox
                .messages
                .iter()
                .filter(|message| {
                    let raw = String::from_utf8_lossy(&message.raw).to_ascii_lowercase();
                    if upper_query.starts_with("TEXT ") {
                        raw.contains(&needle("TEXT "))
                    } else if upper_query.starts_with("HEADER MESSAGE-ID ") {
                        raw.contains(&needle("HEADER MESSAGE-ID "))
                    } else if upper_query.starts_with("FLAGGED") {
                        message.flags.iter().any(|flag| flag == "\\Flagged")
                    } else {
                        true
                    }
                })
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

/// One FETCH answer: the UID always, the flags unless only the UID was
/// asked for, and the whole message when it was.
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

/// The positions a sequence set names in a folder of `count` messages.
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

/// The UIDs a UID set names among `messages`; `n:*` always takes the
/// newest message, as RFC 3501 has it.
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
