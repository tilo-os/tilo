//! A mail account's session with its servers, carried where the account is
//! kept: on the device that holds it, and on the host that runs a person's
//! workspace when their page reaches no mail server.
//!
//! The host reads a folder as a mail program does (the newest page of
//! messages, or what arrived since the newest one the reader holds), tells a
//! server what changed about a message, and hands a written message to the
//! server's SMTP endpoint. Everything is bounded by the same deadline as the
//! sign-in probes; nothing here keeps a secret or answers one.

use std::sync::Arc;
use std::time::Duration;

use tilo_actions::remote::person_accounts::{
    FetchMail, FetchedMail, FetchedMessage, MailAddress, MailAttachment, MailFlag, MarkMail,
    SendMail,
};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// The messages one read answers at most, newest first.
const PAGE: u32 = 50;
/// How far behind the reader's newest message a read still catches up one
/// message at a time instead of starting the folder over.
const CATCH_UP_LIMIT: u32 = 200;
/// How long one session step may take.
const NETWORK_TIMEOUT: Duration = Duration::from_secs(20);

type ImapSession = async_imap::Session<tokio_rustls::client::TlsStream<TcpStream>>;

/// What a host does with the accounts it keeps.
pub trait MailSession: Send + Sync {
    /// Read `asking`'s folder as the server holds it.
    ///
    /// # Errors
    /// The account's server could not be reached, refused the sign-in, or
    /// answered something this cannot read.
    fn fetch(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
        asking: &FetchMail,
    ) -> Result<FetchedMail, String>;

    /// Hand `sending` to its server.
    ///
    /// # Errors
    /// The server could not be reached, refused the sign-in or the message.
    fn send(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
        sending: &SendMail,
    ) -> Result<(), String>;

    /// Tell `marking`'s server what changed about one message.
    ///
    /// # Errors
    /// The server could not be reached or refused the change.
    fn mark(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
        marking: &MarkMail,
    ) -> Result<(), String>;
}

/// The session a host opens with the servers of the accounts it keeps.
#[derive(Default)]
pub struct LiveMailSession;

impl MailSession for LiveMailSession {
    fn fetch(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
        asking: &FetchMail,
    ) -> Result<FetchedMail, String> {
        Self::block(async {
            let mut session = self.login(account, secret).await?;
            fetch_folder(&mut session, asking).await
        })
    }

    fn send(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
        sending: &SendMail,
    ) -> Result<(), String> {
        Self::block(send_message(account, secret, sending))
    }

    fn mark(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
        marking: &MarkMail,
    ) -> Result<(), String> {
        Self::block(async {
            let mut session = self.login(account, secret).await?;
            mark_message(&mut session, account, marking).await
        })
    }
}

impl LiveMailSession {
    fn block<T>(work: impl std::future::Future<Output = Result<T, String>>) -> Result<T, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "could not start the mail session".to_string())?;
        runtime.block_on(work)
    }

    async fn login(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<ImapSession, String> {
        let host = field(account, "imap_host")
            .filter(|host| !host.trim().is_empty())
            .ok_or_else(|| "account has no IMAP server".to_string())?;
        let port = port_of(account, "imap_port", 993);
        let user = field(account, "username")
            .or_else(|| field(account, "email"))
            .ok_or_else(|| "account has no IMAP login name".to_string())?;
        if secret.password.is_empty() {
            return Err("account has no saved password".into());
        }
        let user = login_name(&user)?;
        let password = login_name(&secret.password)?;
        let config = tilo_net::client_config_with_roots(tilo_net::system_roots())
            .map_err(|_| "could not initialize secure protocol versions".to_string())?;
        let server_name = rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|_| "invalid IMAP server name".to_string())?;
        let tcp = connect(&host, port).await?;
        let tls = timeout(
            NETWORK_TIMEOUT,
            tokio_rustls::TlsConnector::from(Arc::new(config)).connect(server_name, tcp),
        )
        .await
        .map_err(|_| "IMAP server did not answer before the timeout".to_string())?
        .map_err(|_| "could not establish the secure IMAP connection".to_string())?;
        let client = async_imap::Client::new(tls);
        timeout(NETWORK_TIMEOUT, client.login(user, password))
            .await
            .map_err(|_| "IMAP server did not answer before the timeout".to_string())?
            .map_err(|(error, _)| match error {
                async_imap::error::Error::No(_) | async_imap::error::Error::Bad(_) => {
                    "IMAP server rejected the saved credential".to_string()
                }
                _ => "IMAP server closed the connection unexpectedly".to_string(),
            })
    }
}

fn field(account: &accounts::Account, name: &str) -> Option<String> {
    account
        .config
        .get(name)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn port_of(account: &accounts::Account, name: &str, default: u16) -> u16 {
    account
        .config
        .get(name)
        .and_then(serde_json::Value::as_u64)
        .and_then(|port| u16::try_from(port).ok())
        .filter(|port| *port != 0)
        .unwrap_or(default)
}

fn login_name(value: &str) -> Result<&str, String> {
    if value.chars().any(char::is_control) {
        return Err("login contains unsupported control characters".into());
    }
    Ok(value)
}

async fn connect(host: &str, port: u16) -> Result<TcpStream, String> {
    let addresses: Vec<_> = timeout(NETWORK_TIMEOUT, tokio::net::lookup_host((host, port)))
        .await
        .map_err(|_| "server name resolution timed out".to_string())?
        .map_err(|_| "server name could not be resolved".to_string())?
        .collect();
    let Some(deadline) = web_time::Instant::now().checked_add(NETWORK_TIMEOUT) else {
        return Err("no time left to reach the server".into());
    };
    let mut last_error = None;
    for address in addresses {
        let Some(remaining) = deadline.checked_duration_since(web_time::Instant::now()) else {
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

/// Where one of the app's folders lives on this account's server: its
/// special-use mark first, then the names the common servers give it.
fn folder_name(listing: &[(String, Vec<String>)], folder: &str) -> Option<String> {
    let use_of = |wanted: &str| {
        listing
            .iter()
            .find(|(_, uses)| uses.iter().any(|use_| use_ == wanted))
            .map(|(name, _)| name.clone())
    };
    let named = |names: &[&str]| {
        listing
            .iter()
            .find(|(name, _)| names.iter().any(|wanted| name.eq_ignore_ascii_case(wanted)))
            .map(|(name, _)| name.clone())
    };
    match folder {
        "inbox" | "starred" => Some("INBOX".to_owned()),
        "sent" => use_of("Sent")
            .or_else(|| named(&["Sent", "Sent Items", "Sent Mail", "[Gmail]/Sent Mail"])),
        "drafts" => use_of("Drafts").or_else(|| named(&["Drafts", "Drafts", "[Gmail]/Drafts"])),
        "archive" => {
            use_of("Archive").or_else(|| named(&["Archive", "All Mail", "[Gmail]/All Mail"]))
        }
        "trash" => use_of("Trash").or_else(|| named(&["Trash", "Deleted Items", "[Gmail]/Trash"])),
        "junk" => use_of("Junk").or_else(|| named(&["Junk", "Spam", "Junk Email", "[Gmail]/Spam"])),
        _ => None,
    }
}

async fn list_folders(session: &mut ImapSession) -> Result<Vec<(String, Vec<String>)>, String> {
    use async_imap::types::NameAttribute;
    use futures_util::TryStreamExt as _;
    let stream = session
        .list(Some(""), Some("*"))
        .await
        .map_err(|e| e.to_string())?;
    stream
        .map_ok(|listed| {
            let uses = listed
                .attributes()
                .iter()
                .filter_map(|attribute| match attribute {
                    NameAttribute::All => Some("All"),
                    NameAttribute::Archive => Some("Archive"),
                    NameAttribute::Drafts => Some("Drafts"),
                    NameAttribute::Junk => Some("Junk"),
                    NameAttribute::Sent => Some("Sent"),
                    NameAttribute::Trash => Some("Trash"),
                    _ => None,
                })
                .map(str::to_owned)
                .collect();
            (listed.name().to_owned(), uses)
        })
        .try_collect()
        .await
        .map_err(|e| e.to_string())
}

/// The items a read asks the server for: the message whole, its position and
/// its flags.
const FETCH_ITEMS: &str = "(UID FLAGS INTERNALDATE BODY.PEEK[])";

async fn fetch_folder(
    session: &mut ImapSession,
    asking: &FetchMail,
) -> Result<FetchedMail, String> {
    if asking.folder.is_empty() {
        return Err("a read names its folder".into());
    }
    let listing = list_folders(session).await?;
    let mailbox = folder_name(&listing, &asking.folder)
        .ok_or_else(|| format!("this account's server has no {} folder", asking.folder))?;
    let selected = session.select(&mailbox).await.map_err(|e| e.to_string())?;
    let uid_validity = selected.uid_validity.unwrap_or(0);
    let exists = selected.exists;
    let now_min = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs().cast_signed() / 60);
    let mut fetched = FetchedMail {
        account: asking.account.clone(),
        folder: asking.folder.clone(),
        uid_validity,
        newest_uid: selected.uid_next.unwrap_or(0).saturating_sub(1),
        ..FetchedMail::default()
    };
    if asking.folder == "starred" {
        // Starred is the inbox seen through its flag: the newest flagged
        // messages, as a page of their own.
        fetched.reset = true;
        let mut uids: Vec<u32> = session
            .uid_search("FLAGGED")
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .collect();
        uids.sort_unstable();
        let newest: Vec<u32> = uids.into_iter().rev().take(PAGE as usize).collect();
        fetched.messages = fetch_uids(session, uid_validity, &newest, now_min).await?;
        return Ok(fetched);
    }
    if exists == 0 {
        fetched.reset = true;
        return Ok(fetched);
    }
    match (asking.known_uid, asking.known_uid_validity) {
        (Some(known), Some(validity))
            if validity == uid_validity
                && fetched.newest_uid.saturating_sub(known) <= CATCH_UP_LIMIT =>
        {
            // What arrived since the newest message the reader holds.
            // `n:*` always answers with the last message, so keep only the
            // ones past the window.
            let range = format!("{}:*", known.saturating_add(1));
            fetched.messages = fetch_set(session, uid_validity, &range, now_min)
                .await?
                .into_iter()
                .filter(|message| message.uid > known)
                .collect();
            fetched.flags =
                fetch_flags(session, known.saturating_sub(PAGE - 1).max(1), known).await?;
        }
        _ => {
            // Nothing held, the folder renumbered, or too much missed: the
            // newest page, from the start.
            fetched.reset = true;
            let first = exists.saturating_sub(PAGE - 1).max(1);
            fetched.messages = fetch_seq(session, uid_validity, first, exists, now_min).await?;
            fetched.has_older = first > 1;
        }
    }
    Ok(fetched)
}

async fn fetch_seq(
    session: &mut ImapSession,
    uid_validity: u32,
    first: u32,
    last: u32,
    now_min: i64,
) -> Result<Vec<FetchedMessage>, String> {
    use futures_util::TryStreamExt as _;
    let stream = session
        .fetch(format!("{first}:{last}"), FETCH_ITEMS)
        .await
        .map_err(|e| e.to_string())?;
    let fetches: Vec<async_imap::types::Fetch> =
        stream.try_collect().await.map_err(|e| e.to_string())?;
    Ok(parse_all(&fetches, uid_validity, now_min))
}

async fn fetch_set(
    session: &mut ImapSession,
    uid_validity: u32,
    uid_set: &str,
    now_min: i64,
) -> Result<Vec<FetchedMessage>, String> {
    use futures_util::TryStreamExt as _;
    let stream = session
        .uid_fetch(uid_set, FETCH_ITEMS)
        .await
        .map_err(|e| e.to_string())?;
    let fetches: Vec<async_imap::types::Fetch> =
        stream.try_collect().await.map_err(|e| e.to_string())?;
    Ok(parse_all(&fetches, uid_validity, now_min))
}

async fn fetch_uids(
    session: &mut ImapSession,
    uid_validity: u32,
    uids: &[u32],
    now_min: i64,
) -> Result<Vec<FetchedMessage>, String> {
    if uids.is_empty() {
        return Ok(Vec::new());
    }
    let set = uids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    fetch_set(session, uid_validity, &set, now_min).await
}

async fn fetch_flags(
    session: &mut ImapSession,
    oldest: u32,
    newest: u32,
) -> Result<Vec<MailFlag>, String> {
    use futures_util::TryStreamExt as _;
    if oldest > newest {
        return Ok(Vec::new());
    }
    let stream = session
        .uid_fetch(format!("{oldest}:{newest}"), "(UID FLAGS)")
        .await
        .map_err(|e| e.to_string())?;
    let fetches: Vec<async_imap::types::Fetch> =
        stream.try_collect().await.map_err(|e| e.to_string())?;
    Ok(fetches
        .iter()
        .filter_map(|fetch| {
            let uid = fetch.uid?;
            if uid < oldest || uid > newest {
                return None;
            }
            Some(MailFlag {
                uid,
                read: seen(fetch),
                starred: flagged(fetch),
            })
        })
        .collect())
}

fn seen(fetch: &async_imap::types::Fetch) -> bool {
    fetch
        .flags()
        .any(|flag| matches!(flag, async_imap::types::Flag::Seen))
}

fn flagged(fetch: &async_imap::types::Fetch) -> bool {
    fetch
        .flags()
        .any(|flag| matches!(flag, async_imap::types::Flag::Flagged))
}

fn parse_all(
    fetches: &[async_imap::types::Fetch],
    _uid_validity: u32,
    now_min: i64,
) -> Vec<FetchedMessage> {
    fetches
        .iter()
        .filter_map(|fetch| {
            let uid = fetch.uid?;
            let body = fetch.body()?;
            let internal = fetch
                .internal_date()
                .map_or(now_min, |date| date.timestamp() / 60);
            parse_message(body, uid, seen(fetch), flagged(fetch), internal)
        })
        .collect()
}

/// One message as the wire carries it: what a reader shows, never how it is
/// stored.
fn parse_message(
    raw: &[u8],
    uid: u32,
    read: bool,
    starred: bool,
    minutes: i64,
) -> Option<FetchedMessage> {
    use mail_parser::MimeHeaders as _;
    let parsed = mail_parser::MessageParser::default().parse(raw)?;
    let address = |address: &mail_parser::Addr| MailAddress {
        name: address
            .name
            .as_ref()
            .map(std::string::ToString::to_string)
            .filter(|name| !name.trim().is_empty()),
        email: address
            .address
            .as_ref()
            .map(std::string::ToString::to_string)
            .unwrap_or_default(),
    };
    let from = parsed.from().and_then(|from| from.first()).map_or_else(
        || MailAddress {
            name: None,
            email: "unknown@unknown".into(),
        },
        address,
    );
    let to = parsed
        .to()
        .map(|to| to.iter().map(address).collect())
        .unwrap_or_default();
    let cc = parsed
        .cc()
        .map(|cc| cc.iter().map(address).collect())
        .unwrap_or_default();
    let body = parsed
        .body_text(0)
        .map(std::borrow::Cow::into_owned)
        .unwrap_or_default();
    let html_body = parsed
        .html_bodies()
        .find_map(|part| match &part.body {
            mail_parser::PartType::Html(html) => Some(html.to_string()),
            _ => None,
        })
        .filter(|html| !html.trim().is_empty());
    let referenced = html_body
        .as_deref()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let attachments = parsed
        .attachments()
        .map(|part| {
            let inline_id = part
                .content_id()
                .map(|id| {
                    id.trim()
                        .trim_start_matches('<')
                        .trim_end_matches('>')
                        .to_owned()
                })
                .filter(|id| {
                    !id.is_empty()
                        && referenced.contains(&format!("cid:{}", id.to_ascii_lowercase()))
                });
            let filename = part
                .attachment_name()
                .unwrap_or("attachment.bin")
                .to_string();
            let declared = part.content_type().map(|kind| {
                format!("{}/{}", kind.ctype(), kind.subtype().unwrap_or_default())
                    .to_ascii_lowercase()
            });
            MailAttachment {
                mime: declared.unwrap_or_else(|| "application/octet-stream".to_owned()),
                filename,
                size: part.contents().len() as u64,
                inline_id,
            }
        })
        .collect();
    Some(FetchedMessage {
        uid,
        message_id: parsed.message_id().map(std::string::ToString::to_string),
        in_reply_to: parsed
            .references()
            .as_text_list()
            .and_then(|ids| ids.first())
            .map(std::string::ToString::to_string)
            .or_else(|| parsed.in_reply_to().as_text().map(str::to_string)),
        from,
        to,
        cc,
        subject: parsed.subject().unwrap_or_default().to_string(),
        body,
        html_body,
        attachments,
        minutes,
        read,
        starred,
    })
}

async fn mark_message(
    session: &mut ImapSession,
    account: &accounts::Account,
    marking: &MarkMail,
) -> Result<(), String> {
    use futures_util::TryStreamExt as _;
    let listing = list_folders(session).await?;
    let mailbox = folder_name(&listing, &marking.folder)
        .ok_or_else(|| format!("this account's server has no {} folder", marking.folder))?;
    let selected = session.select(&mailbox).await.map_err(|e| e.to_string())?;
    if selected.uid_validity.unwrap_or(0) != marking.uid_validity {
        return Err("the folder changed; its messages are read again".into());
    }
    if let Some(read) = marking.read {
        let flags = format!("{}SILENT (\\Seen)", if read { "+" } else { "-" });
        let stored = session
            .uid_store(marking.uid.to_string(), flags)
            .await
            .map_err(|e| e.to_string())?;
        let _: Vec<_> = stored.try_collect().await.map_err(|e| e.to_string())?;
    }
    if let Some(starred) = marking.starred {
        let flags = format!("{}SILENT (\\Flagged)", if starred { "+" } else { "-" });
        let stored = session
            .uid_store(marking.uid.to_string(), flags)
            .await
            .map_err(|e| e.to_string())?;
        let _: Vec<_> = stored.try_collect().await.map_err(|e| e.to_string())?;
    }
    if let Some(to) = &marking.to_folder {
        let destination = folder_name(&listing, to)
            .ok_or_else(|| format!("this account's server has no {to} folder"))?;
        session
            .uid_mv(marking.uid.to_string(), destination)
            .await
            .map_err(|e| e.to_string())?;
    }
    if marking.delete {
        let deleted = session
            .uid_store(marking.uid.to_string(), "+FLAGS.SILENT (\\Deleted)")
            .await
            .map_err(|e| e.to_string())?;
        let _: Vec<_> = deleted.try_collect().await.map_err(|e| e.to_string())?;
        let expunged = session
            .uid_expunge(marking.uid.to_string())
            .await
            .map_err(|e| format!("server cannot safely expunge this message: {e}"))?;
        let _: Vec<_> = expunged.try_collect().await.map_err(|e| e.to_string())?;
    }
    let _ = account;
    let _ = timeout(NETWORK_TIMEOUT, session.logout()).await;
    Ok(())
}

/// Write `sending` as mail and hand it to the account's server.
async fn send_message(
    account: &accounts::Account,
    secret: &accounts::Secret,
    sending: &SendMail,
) -> Result<(), String> {
    use lettre::AsyncTransport as _;
    use lettre::transport::smtp::authentication::{Credentials, Mechanism};
    use lettre::transport::smtp::client::TlsParameters;
    use lettre::{AsyncSmtpTransport, Tokio1Executor};

    let host = field(account, "smtp_host")
        .filter(|host| !host.trim().is_empty())
        .ok_or_else(|| "account has no SMTP server".to_string())?;
    let port = port_of(account, "smtp_port", 465);
    let user = field(account, "username")
        .or_else(|| field(account, "email"))
        .ok_or_else(|| "account has no SMTP login name".to_string())?;
    if secret.password.is_empty() {
        return Err("account has no saved password".into());
    }
    let mime = write_mime(sending)?;
    let envelope = envelope(sending)?;
    let params = TlsParameters::new(host.clone())
        .map_err(|error| format!("could not prepare the secure SMTP connection: {error}"))?;
    let tls = match port {
        465 => lettre::transport::smtp::client::Tls::Wrapper(params),
        _ => lettre::transport::smtp::client::Tls::Required(params),
    };
    let transport = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
        .port(port)
        .tls(tls)
        .credentials(Credentials::new(user, secret.password.clone()))
        .authentication(vec![Mechanism::Login, Mechanism::Plain])
        .timeout(Some(NETWORK_TIMEOUT))
        .build();
    timeout(NETWORK_TIMEOUT, transport.send_raw(&envelope, &mime))
        .await
        .map_err(|_| "SMTP server did not answer before the timeout".to_string())?
        .map(|_| ())
        .map_err(|error| format!("the message could not be sent: {error}"))
}

/// The MIME of a message as it goes on the wire.
fn write_mime(sending: &SendMail) -> Result<Vec<u8>, String> {
    let from = (
        sending.from_name.clone().unwrap_or_default(),
        sending.from_address.clone(),
    );
    let mut builder = mail_builder::MessageBuilder::new()
        .from(from)
        .subject(sending.subject.clone())
        .text_body(sending.body.clone());
    if let Some(html) = &sending.html_body {
        builder = builder.html_body(html.clone());
    }
    if let Some(message_id) = &sending.message_id {
        builder = builder.message_id(message_id.clone());
    }
    if let Some(parent) = &sending.in_reply_to {
        builder = builder.in_reply_to(parent.clone());
    }
    if !sending.references.is_empty() {
        builder = builder.references(sending.references.clone());
    }
    let to = parse_env_addresses(&sending.to);
    let cc = parse_env_addresses(&sending.cc);
    if !to.is_empty() {
        builder = builder.to(to);
    }
    if !cc.is_empty() {
        builder = builder.cc(cc);
    }
    // Blind recipients belong only in the envelope; a header here would
    // show them to every other recipient.
    builder
        .write_to_vec()
        .map_err(|error| format!("could not write the message: {error}"))
}

fn envelope(sending: &SendMail) -> Result<lettre::address::Envelope, String> {
    let sender = sending
        .from_address
        .parse::<lettre::Address>()
        .map_err(|error| format!("the sending address is not usable: {error}"))?;
    let mut recipients = Vec::new();
    for list in [&sending.to, &sending.cc, &sending.bcc] {
        for address in parse_env_addresses(list) {
            if let Ok(email) = address.1.parse::<lettre::Address>() {
                recipients.push(email);
            }
        }
    }
    if recipients.is_empty() {
        return Err("a message needs a recipient".into());
    }
    lettre::address::Envelope::new(Some(sender), recipients)
        .map_err(|error| format!("the message's addresses are not usable: {error}"))
}

/// `Name <address>, other@example` as mail's own tuple form.
fn parse_env_addresses(list: &str) -> Vec<(String, String)> {
    list.split([';', ','])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            if let Some((name, rest)) = entry.split_once('<')
                && let Some((address, _)) = rest.rsplit_once('>')
            {
                (
                    name.trim().trim_matches('"').to_owned(),
                    address.trim().to_owned(),
                )
            } else {
                (String::new(), entry.to_owned())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

    /// A folder is found by its special-use mark before its name, and a
    /// server that marks nothing is still read by the names people know.
    #[test]
    fn a_folder_is_found_by_its_mark_then_its_name() {
        let listing = |entries: &[(&str, &[&str])]| -> Vec<(String, Vec<String>)> {
            entries
                .iter()
                .map(|(name, uses)| {
                    (
                        (*name).to_owned(),
                        uses.iter().map(|u| (*u).to_owned()).collect(),
                    )
                })
                .collect()
        };
        let gmail = listing(&[
            ("INBOX", &[]),
            ("[Gmail]/All Mail", &["All"]),
            ("[Gmail]/Sent Mail", &["Sent"]),
            ("[Gmail]/Spam", &["Junk"]),
            ("[Gmail]/Trash", &["Trash"]),
        ]);
        assert_eq!(folder_name(&gmail, "inbox").as_deref(), Some("INBOX"));
        assert_eq!(
            folder_name(&gmail, "sent").as_deref(),
            Some("[Gmail]/Sent Mail")
        );
        assert_eq!(folder_name(&gmail, "junk").as_deref(), Some("[Gmail]/Spam"));
        assert_eq!(
            folder_name(&gmail, "trash").as_deref(),
            Some("[Gmail]/Trash")
        );

        let outlook = listing(&[
            ("INBOX", &[]),
            ("Sent Items", &[]),
            ("Deleted Items", &[]),
            ("Junk Email", &[]),
        ]);
        assert_eq!(folder_name(&outlook, "sent").as_deref(), Some("Sent Items"));
        assert_eq!(
            folder_name(&outlook, "trash").as_deref(),
            Some("Deleted Items")
        );
        assert_eq!(folder_name(&outlook, "archive"), None);
    }

    /// Blind recipients reach the delivery envelope and no copy header.
    #[test]
    fn blind_recipients_are_enveloped_and_never_written() {
        let sending = SendMail {
            account: "ada@example.test".into(),
            from_name: Some("Ada".into()),
            from_address: "ada@example.test".into(),
            to: "Bob <bob@example.test>".into(),
            cc: String::new(),
            bcc: "Hidden <hidden@example.test>".into(),
            subject: "Hello".into(),
            body: "Hi".into(),
            ..SendMail::default()
        };
        let envelope = envelope(&sending).unwrap();
        let recipients: Vec<String> = envelope.to().iter().map(ToString::to_string).collect();
        assert_eq!(recipients, vec!["bob@example.test", "hidden@example.test"]);
        let mime = String::from_utf8_lossy(&write_mime(&sending).unwrap()).to_string();
        assert!(mime.contains("bob@example.test"), "{mime}");
        assert!(
            !mime.to_ascii_lowercase().contains("hidden@example.test"),
            "{mime}"
        );
    }

    /// A fetched message is read into what a reader shows, never into how
    /// it is stored.
    #[test]
    fn a_fetched_message_is_read_as_a_reader_shows_it() {
        let raw = b"From: Ada <ada@example.test>\r\n\
To: Bob <bob@example.test>\r\n\
Subject: Hello\r\n\
Message-ID: <42@example.test>\r\n\
In-Reply-To: <1@example.test>\r\n\
Content-Type: text/plain\r\n\r\n\
Hi Bob\r\n";
        let message = parse_message(raw, 42, false, true, 100).unwrap();
        assert_eq!(message.uid, 42);
        assert_eq!(message.message_id.as_deref(), Some("42@example.test"));
        assert_eq!(message.in_reply_to.as_deref(), Some("1@example.test"));
        assert_eq!(message.from.email, "ada@example.test");
        assert_eq!(message.from.name.as_deref(), Some("Ada"));
        assert_eq!(message.to[0].email, "bob@example.test");
        assert_eq!(message.subject, "Hello");
        assert_eq!(message.body.trim(), "Hi Bob");
        assert!(message.starred);
        assert!(!message.read);
        assert!(message.html_body.is_none());
    }
}
