//! Mail's engine, carried by the host that keeps the account.
//!
//! A page opens no sockets and holds no account secret, so its window does
//! not reach a mail server itself. The host that runs the workspace keeps
//! the person's accounts and their sign-ins, so it reads folders, sends and
//! tells servers what changed through the `tilo.accounts` actions
//! ([`person_accounts`]). This is the page's side of that: it takes the
//! window's engine requests, asks the host, and answers the window with the
//! same events a device's engine produces — so Mail behaves the same
//! wherever it runs, and the account has one store.

use std::sync::mpsc::{self, Receiver, Sender};

use ui::tilo_actions::InvocationUpdate;
use ui::tilo_actions::remote::{
    InvokeArgument, person_accounts,
    person_accounts::{
        BeginSend, FetchMail, FetchedMail, FoundMail, MAX_SEND_PIECE_BYTES, MailAccount,
        MailAccounts, MarkMail, PieceKept, SearchMail, SendFile as WireFile, SendId, SendMail,
        SendPiece, SentMail, StartedSend,
    },
};
use ui::{Arrival, InvocationCall, RemoteActions};

use super::{EngineEvent, EngineHandle, EngineRequest, EngineRequestId, EngineRequestKind};
use crate::model::{Addr, Attachment, Folder, Message, MessageId, ServerPlace};
use crate::sync::Synced;

/// One request asked of the host, and where its answer goes.
struct Asked {
    request: EngineRequest,
    call: InvocationCall,
}

/// A message the window sent, as the host takes it: begun under the host's
/// id, its files' bytes handed over a piece at a time, then sent.
enum Staging {
    Beginning {
        request_id: EngineRequestId,
        files: Vec<crate::model::SendFile>,
        call: InvocationCall,
    },
    Piece {
        request_id: EngineRequestId,
        send: String,
        files: Vec<crate::model::SendFile>,
        file: usize,
        offset: u64,
        call: InvocationCall,
    },
    Sending {
        request_id: EngineRequestId,
        call: InvocationCall,
    },
}

impl Staging {
    fn request_id(&self) -> EngineRequestId {
        match self {
            Self::Beginning { request_id, .. }
            | Self::Piece { request_id, .. }
            | Self::Sending { request_id, .. } => *request_id,
        }
    }

    fn call(&self) -> &InvocationCall {
        match self {
            Self::Beginning { call, .. }
            | Self::Piece { call, .. }
            | Self::Sending { call, .. } => call,
        }
    }
}

/// The bytes of the file at `offset` up to where its next piece ends: what
/// it hands the host this time.
fn piece_end(file: &crate::model::SendFile, offset: u64) -> u64 {
    offset
        .saturating_add(MAX_SEND_PIECE_BYTES as u64)
        .min(file.bytes.len() as u64)
}

/// The page's end of the host's mail session.
pub(crate) struct HostMail {
    /// The window's action client, taken on the first frame: a window is
    /// made before the desktop serves its actions, so asking earlier would
    /// leave every ask on a connection that answers none of them.
    remote: Option<RemoteActions>,
    requests: Receiver<EngineRequest>,
    events: Sender<EngineEvent>,
    asking: Vec<Asked>,
    /// The accounts the host answered since the last read, with the one the
    /// person chose last.
    accounts: Option<MailAccounts>,
    /// The host's live reading of the person's accounts: each item is the
    /// whole listing as the host keeps it now, sent again whenever they
    /// change, so an account added in another window reaches this one
    /// without asking again. Kept for as long as the window lives; dropping
    /// it ends the reading on the host.
    watching: Option<ui::Followed>,
    /// The update of the reading last taken in.
    watched: u64,
    /// The account the person asked to add, while the host keeps it.
    adding: Option<InvocationCall>,
    /// Why the host would not keep the account, once it would not.
    add_error: Option<String>,
    /// The messages the window sent, while the host takes them: one step of
    /// one at a time, in the order they were sent.
    staging: Vec<Staging>,
    /// The begun messages the host keeps as drafts instead of sending.
    drafts: std::collections::HashSet<EngineRequestId>,
    /// Where the window keeps the bytes of a draft's files: a draft kept on
    /// its server is written from them, as a send is.
    content: tilo_content::ContentStore,
}

/// A window with the host's mail session as its engine: what the window
/// posts is asked of the host, and what the host answers is the window's.
pub(super) fn engine(content: tilo_content::ContentStore) -> (EngineHandle, HostMail) {
    let (tx, requests) = mpsc::channel();
    let (events, rx) = mpsc::channel();
    let host = HostMail {
        remote: None,
        requests,
        events,
        asking: Vec::new(),
        accounts: None,
        watching: None,
        watched: 0,
        adding: None,
        add_error: None,
        staging: Vec::new(),
        drafts: std::collections::HashSet::new(),
        content,
    };
    (EngineHandle { tx, rx }, host)
}

impl HostMail {
    fn remote(&mut self) -> RemoteActions {
        self.remote
            .get_or_insert_with(RemoteActions::current)
            .clone()
    }

    fn follow_accounts(&mut self) {
        let remote = self.remote();
        self.watching = Some(remote.follow(
            person_accounts::target(person_accounts::WATCH),
            InvokeArgument::None,
        ));
    }

    /// The accounts the host answered since the last read.
    pub(super) fn take_accounts(&mut self) -> Option<MailAccounts> {
        self.accounts.take()
    }

    /// Why the host would not keep the account the person added, once it
    /// would not.
    pub(super) fn take_add_error(&mut self) -> Option<String> {
        self.add_error.take()
    }

    /// Ask the host to keep `config`'s account, with the sign-in it carries:
    /// the host proves it and seals it in the person's vault, and the
    /// next reading shows the account.
    #[cfg(any(not(feature = "live"), test))]
    pub(super) fn add_account(&mut self, config: &crate::model::AccountConfig) {
        let adding = person_accounts::AddMailAccount {
            address: config.email.clone(),
            display_name: config.display_name.clone(),
            username: Some(config.username.clone())
                .filter(|username| !username.is_empty() && username != &config.email),
            incoming_host: config.imap_host.clone(),
            incoming_port: config.imap_port,
            outgoing_host: config.smtp_host.clone(),
            outgoing_port: config.smtp_port,
            password: config.password.clone(),
            identities: config.identities.clone(),
        };
        let Some(argument) = map(&adding) else {
            self.add_error = Some("the account could not be asked for".into());
            return;
        };
        let remote = self.remote();
        self.adding = Some(remote.invoke(
            person_accounts::target(person_accounts::ADD_MAIL),
            argument,
            None,
        ));
    }

    /// Take every request the window posted and every answer the host gave.
    /// Whether anything changed.
    pub(super) fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(request) = self.requests.try_recv() {
            changed = true;
            // A send is the host's in several calls: begun, its files handed
            // over a piece at a time, then sent. Nothing of it is asked in
            // the one call the other requests make.
            if let EngineRequest::Send {
                request_id,
                account,
                draft,
                files,
            } = request
            {
                self.begin_send(request_id, &account, &draft, files);
                continue;
            }
            // A draft kept on its server is begun as a message is, with the
            // bytes its files hold here, and kept instead of sent.
            if let EngineRequest::SaveDraft {
                request_id,
                account,
                draft,
            } = request
            {
                self.drafts.insert(request_id);
                match crate::attachment_store::materialize_files(&self.content, &draft.attachments)
                {
                    Ok(files) => self.begin_send(request_id, &account, &draft, files),
                    Err(reason) => self.fail_send(request_id, reason),
                }
                continue;
            }
            if let Some((target, argument)) = asking(&request) {
                let remote = self.remote();
                let call = remote.invoke(target, argument, None);
                self.asking.push(Asked { request, call });
            } else {
                // Nothing the host cannot do: what the window keeps itself
                // is done, and nothing reaches a server.
                let done = match &request {
                    // The host keeps the sign-in, so the account is signed
                    // in as soon as the window asks.
                    EngineRequest::Connect {
                        request_id,
                        account,
                    } => EngineEvent::Connected {
                        request_id: *request_id,
                        account_id: account.email.clone(),
                    },
                    EngineRequest::SaveDraft { .. } | EngineRequest::Watch { .. } => {
                        EngineEvent::Completed {
                            request_id: request.request_id(),
                            kind: request.kind(),
                        }
                    }
                    EngineRequest::Search {
                        request_id,
                        account,
                        ..
                    } => EngineEvent::Found {
                        request_id: *request_id,
                        account_id: account.email.clone(),
                        messages: Vec::new(),
                    },
                    _ => EngineEvent::Error {
                        request_id: request.request_id(),
                        kind: request.kind(),
                        context: "host".into(),
                        message: "this window's host does not offer that".into(),
                    },
                };
                let _ = self.events.send(done);
            }
        }
        let mut still = Vec::new();
        for asked in std::mem::take(&mut self.asking) {
            match answered(&asked.call) {
                Some(Ok(value)) => {
                    changed = true;
                    self.answer(&asked.request, value);
                }
                Some(Err(reason)) => {
                    changed = true;
                    tracing::warn!(%reason, "mail: the host refused this window's request");
                    // A search the host would not make finds nothing, as on
                    // a device, and the window says so nowhere.
                    let context = match asked.request {
                        EngineRequest::Search { .. } => "search",
                        _ => "host",
                    };
                    let _ = self.events.send(EngineEvent::Error {
                        request_id: asked.request.request_id(),
                        kind: asked.request.kind(),
                        context: context.into(),
                        message: reason,
                    });
                }
                None => still.push(asked),
            }
        }
        self.asking = still;
        changed |= self.advance_staging();
        changed |= self.poll_accounts();
        changed
    }

    /// Take the host's answer to an account it was asked to keep, and the
    /// host's reading of the person's accounts. Whether anything moved.
    fn poll_accounts(&mut self) -> bool {
        let mut changed = false;
        if let Some(adding) = &self.adding
            && let Some(outcome) = answered(adding)
        {
            changed = true;
            self.adding = None;
            if let Err(reason) = outcome {
                self.add_error = Some(reason);
            }
        }
        changed |= self.read_watched_accounts();
        changed
    }

    /// The person's accounts as the host keeps them: the window follows the
    /// host's live reading of them, so an account another window adds or
    /// removes reaches this one when it happens rather than on a clock.
    /// Whether it moved.
    fn read_watched_accounts(&mut self) -> bool {
        if self.watching.is_none() {
            self.follow_accounts();
        }
        let Some(watching) = &self.watching else {
            return false;
        };
        let call = watching.call();
        let revision = call.revision();
        if revision == self.watched {
            return false;
        }
        self.watched = revision;
        match call.get() {
            Arrival::Ready(InvocationUpdate::Item { value, .. }) => {
                match <MailAccounts as serde::Deserialize>::deserialize(value) {
                    Ok(accounts) => self.accounts = Some(accounts),
                    Err(error) => {
                        tracing::warn!(%error, "mail: the host's accounts are not readable");
                    }
                }
                true
            }
            Arrival::Ready(InvocationUpdate::Failed { error, .. }) => {
                tracing::warn!(reason = %error.message, "mail: the host did not read its accounts for this window");
                true
            }
            Arrival::Ready(InvocationUpdate::Cancelled { message, .. }) => {
                tracing::warn!(
                    reason = message.as_deref().unwrap_or("the host ended the reading"),
                    "mail: the host stopped reading its accounts for this window"
                );
                true
            }
            Arrival::Failed(error) => {
                tracing::warn!(%error, "mail: the host could not be asked for its accounts");
                true
            }
            Arrival::Ready(_) | Arrival::Pending { .. } => false,
        }
    }

    /// Ask the host to begin the message the window sent: what it says, whom
    /// it goes to, its invitation and the files whose bytes follow.
    fn begin_send(
        &mut self,
        request_id: EngineRequestId,
        account: &crate::model::AccountConfig,
        draft: &crate::model::Draft,
        files: Vec<crate::model::SendFile>,
    ) {
        let beginning = BeginSend {
            message: SendMail {
                account: account.email.clone(),
                from_name: Some(account.display_name.clone()).filter(|name| !name.is_empty()),
                from_address: draft
                    .sender
                    .as_ref()
                    .map(|sender| sender.email.clone())
                    .filter(|email| !email.is_empty())
                    .unwrap_or_else(|| account.email.clone()),
                to: draft.to.clone(),
                cc: draft.cc.clone(),
                bcc: draft.bcc.clone(),
                subject: draft.subject.clone(),
                body: draft.body.clone(),
                html_body: draft.html_body.clone(),
                message_id: draft.wire.message_id.clone(),
                in_reply_to: draft.wire.in_reply_to.clone(),
                references: draft.wire.references.clone(),
            },
            calendar_ics: draft.calendar_ics.clone(),
            files: files
                .iter()
                .map(|file| WireFile {
                    filename: file.attachment.filename.clone(),
                    mime: file.attachment.mime.clone(),
                    size: file.bytes.len() as u64,
                })
                .collect(),
        };
        let Some(argument) = map(&beginning) else {
            self.fail_send(request_id, "the message could not be asked for".into());
            return;
        };
        let remote = self.remote();
        let call = remote.invoke(
            person_accounts::target(person_accounts::BEGIN_SEND),
            argument,
            None,
        );
        self.staging.push(Staging::Beginning {
            request_id,
            files,
            call,
        });
    }

    /// Take the host's answer to the step in flight and ask the next: the
    /// begun message, each file's pieces in order, then the send. Whether
    /// anything moved.
    fn advance_staging(&mut self) -> bool {
        let Some(index) = self
            .staging
            .iter()
            .position(|stage| answered(stage.call()).is_some())
        else {
            return false;
        };
        let stage = self.staging.remove(index);
        let Some(outcome) = answered(stage.call()) else {
            return false;
        };
        let request_id = stage.request_id();
        match (stage, outcome) {
            (
                Staging::Beginning {
                    files, request_id, ..
                },
                Ok(value),
            ) => match serde_json::from_value::<StartedSend>(value) {
                Ok(started) => self.send_from_file(request_id, started.send, files, 0),
                Err(error) => self.fail_send(
                    request_id,
                    format!("the host's answer was not a begun message: {error}"),
                ),
            },
            (
                Staging::Piece {
                    send,
                    files,
                    file,
                    offset,
                    ..
                },
                Ok(value),
            ) => match serde_json::from_value::<PieceKept>(value) {
                Ok(kept) => {
                    let Some(this) = files.get(file) else {
                        self.fail_send(request_id, "that message carries no such file".into());
                        return true;
                    };
                    let end = piece_end(this, offset);
                    if kept.written != end {
                        self.fail_send(
                            request_id,
                            format!("the host holds {} bytes of that file", kept.written),
                        );
                    } else if end >= this.bytes.len() as u64 {
                        self.send_from_file(request_id, send, files, file + 1);
                    } else {
                        self.send_piece(request_id, send, files, file, end);
                    }
                }
                Err(error) => self.fail_send(
                    request_id,
                    format!("the host's answer was not a piece: {error}"),
                ),
            },
            (Staging::Sending { .. }, Ok(_)) if self.drafts.remove(&request_id) => {
                let _ = self.events.send(EngineEvent::Completed {
                    request_id,
                    kind: EngineRequestKind::SaveDraft,
                });
            }
            (Staging::Sending { .. }, Ok(value)) => {
                let _ = self.events.send(EngineEvent::Sent {
                    request_id,
                    wire_bytes: 0,
                });
                // A host that says nothing of the copy filed it, or its
                // server files what it sends.
                let unfiled = serde_json::from_value::<SentMail>(value)
                    .ok()
                    .and_then(|sent| sent.unfiled);
                if let Some(reason) = unfiled {
                    let _ = self
                        .events
                        .send(EngineEvent::Unfiled { request_id, reason });
                }
            }
            (_, Err(reason)) => self.fail_send(request_id, reason),
        }
        true
    }

    /// Hand the file at `file` over, or send the message once every file is
    /// the host's.
    fn send_from_file(
        &mut self,
        request_id: EngineRequestId,
        send: String,
        files: Vec<crate::model::SendFile>,
        file: usize,
    ) {
        if file >= files.len() {
            let action = if self.drafts.contains(&request_id) {
                person_accounts::SAVE_DRAFT
            } else {
                person_accounts::SEND
            };
            let remote = self.remote();
            let call = remote.invoke(
                person_accounts::target(action),
                map(&SendId { send }).unwrap_or(InvokeArgument::None),
                None,
            );
            self.staging.push(Staging::Sending { request_id, call });
        } else if files.get(file).is_some_and(|file| file.bytes.is_empty()) {
            self.send_from_file(request_id, send, files, file + 1);
        } else {
            self.send_piece(request_id, send, files, file, 0);
        }
    }

    /// Hand over the piece of the file at `file` that starts at `offset`.
    fn send_piece(
        &mut self,
        request_id: EngineRequestId,
        send: String,
        files: Vec<crate::model::SendFile>,
        file_index: usize,
        offset: u64,
    ) {
        let Some(file) = files.get(file_index) else {
            self.fail_send(request_id, "that message carries no such file".into());
            return;
        };
        let end = piece_end(file, offset);
        let Some(bytes) = file
            .bytes
            .get(offset as usize..end as usize)
            .map(hex::encode)
        else {
            self.fail_send(request_id, "that file has no bytes there".into());
            return;
        };
        let piece = SendPiece {
            send: send.clone(),
            file: file_index as u32,
            offset,
            bytes,
        };
        let Some(argument) = map(&piece) else {
            self.fail_send(request_id, "the piece could not be asked for".into());
            return;
        };
        let remote = self.remote();
        let call = remote.invoke(
            person_accounts::target(person_accounts::SEND_PIECE),
            argument,
            None,
        );
        self.staging.push(Staging::Piece {
            request_id,
            send,
            files,
            file: file_index,
            offset,
            call,
        });
    }

    /// Say why the message the window sent did not leave it, or why its
    /// draft was not kept.
    fn fail_send(&mut self, request_id: EngineRequestId, reason: String) {
        tracing::warn!(%reason, "mail: the host would not take the message");
        let kind = if self.drafts.remove(&request_id) {
            EngineRequestKind::SaveDraft
        } else {
            EngineRequestKind::Send
        };
        let _ = self.events.send(EngineEvent::Error {
            request_id,
            kind,
            context: "host".into(),
            message: reason,
        });
    }

    /// Answer one request with what the host said.
    fn answer(&self, request: &EngineRequest, value: serde_json::Value) {
        let event = match &request {
            EngineRequest::Connect {
                request_id,
                account,
            } => EngineEvent::Connected {
                request_id: *request_id,
                account_id: account.email.clone(),
            },
            EngineRequest::Fetch {
                request_id,
                account,
                folder,
                want,
            } => match serde_json::from_value::<FetchedMail>(value) {
                Ok(fetched) => EngineEvent::Fetched {
                    request_id: *request_id,
                    account_id: account.email.clone(),
                    folder: Folder::from_key(&fetched.folder).unwrap_or(*folder),
                    synced: synced(
                        fetched,
                        &account.email,
                        matches!(want, crate::sync::Want::Older { .. }),
                    ),
                },
                Err(error) => EngineEvent::Error {
                    request_id: *request_id,
                    kind: request.kind(),
                    context: "host".into(),
                    message: format!("the host's answer was not a folder: {error}"),
                },
            },
            EngineRequest::Send { request_id, .. } => EngineEvent::Sent {
                request_id: *request_id,
                wire_bytes: 0,
            },
            EngineRequest::StoreFlag { request_id, .. }
            | EngineRequest::Move { request_id, .. }
            | EngineRequest::Purge { request_id, .. }
            | EngineRequest::OAuthAuthorize { request_id, .. }
            | EngineRequest::SaveDraft { request_id, .. }
            | EngineRequest::Watch { request_id, .. } => EngineEvent::Completed {
                request_id: *request_id,
                kind: request.kind(),
            },
            EngineRequest::Search {
                request_id,
                account,
                ..
            } => match serde_json::from_value::<FoundMail>(value) {
                Ok(found) => EngineEvent::Found {
                    request_id: *request_id,
                    account_id: account.email.clone(),
                    messages: found_messages(found, &account.email),
                },
                Err(error) => EngineEvent::Error {
                    request_id: *request_id,
                    kind: request.kind(),
                    context: "search".into(),
                    message: format!("the host's answer was not what it found: {error}"),
                },
            },
        };
        let _ = self.events.send(event);
    }
}

/// What the host is asked for `request`, when it is the host's to do.
fn asking(
    request: &EngineRequest,
) -> Option<(tilo_actions::remote::RemoteActionTarget, InvokeArgument)> {
    let (action, argument) = match request {
        // A send is not asked in one call: [`HostMail::poll`] asks the host
        // to begin it, hands its files over a piece at a time, and then
        // sends it.
        EngineRequest::Connect { .. }
        | EngineRequest::Watch { .. }
        | EngineRequest::SaveDraft { .. }
        | EngineRequest::OAuthAuthorize { .. }
        | EngineRequest::Send { .. } => return None,
        EngineRequest::Search { account, text, .. } => {
            let searching = SearchMail {
                account: account.email.clone(),
                text: Some(text.clone()),
                ..SearchMail::default()
            };
            (person_accounts::SEARCH, map(&searching)?)
        }
        EngineRequest::Fetch {
            account,
            folder,
            want,
            ..
        } => {
            // What arrived since the newest message the window holds, or,
            // for a window scrolled past its oldest one, the page before
            // that.
            let asking = match want {
                crate::sync::Want::Latest { known } => FetchMail {
                    account: account.email.clone(),
                    folder: folder.as_key().to_owned(),
                    known_uid: known.map(|window| window.newest),
                    known_uid_validity: known.map(|window| window.uid_validity),
                    before_uid: None,
                },
                crate::sync::Want::Older { known } => FetchMail {
                    account: account.email.clone(),
                    folder: folder.as_key().to_owned(),
                    known_uid: None,
                    known_uid_validity: Some(known.uid_validity),
                    before_uid: Some(known.oldest),
                },
            };
            (person_accounts::FETCH, map(&asking)?)
        }
        EngineRequest::StoreFlag {
            account, message, ..
        } => {
            let at = message.server?;
            let marking = MarkMail {
                account: account.email.clone(),
                folder: at.folder.as_key().to_owned(),
                uid: at.uid,
                uid_validity: at.uid_validity,
                read: Some(message.read),
                starred: Some(message.starred),
                ..MarkMail::default()
            };
            (person_accounts::MARK, map(&marking)?)
        }
        EngineRequest::Move {
            account,
            message,
            to,
            ..
        } => {
            let at = message.server?;
            let marking = MarkMail {
                account: account.email.clone(),
                folder: at.folder.as_key().to_owned(),
                uid: at.uid,
                uid_validity: at.uid_validity,
                to_folder: Some(to.as_key().to_owned()),
                ..MarkMail::default()
            };
            (person_accounts::MARK, map(&marking)?)
        }
        EngineRequest::Purge {
            account, message, ..
        } => {
            let at = message.server?;
            let marking = MarkMail {
                account: account.email.clone(),
                folder: at.folder.as_key().to_owned(),
                uid: at.uid,
                uid_validity: at.uid_validity,
                delete: true,
                ..MarkMail::default()
            };
            (person_accounts::MARK, map(&marking)?)
        }
    };
    Some((person_accounts::target(action), argument))
}

fn map(argument: &impl serde::Serialize) -> Option<InvokeArgument> {
    match serde_json::to_value(argument).ok()? {
        serde_json::Value::Object(map) => Some(InvokeArgument::Json(map)),
        _ => None,
    }
}

/// What the host answered, once it has.
fn answered(call: &InvocationCall) -> Option<Result<serde_json::Value, String>> {
    match call.get() {
        Arrival::Ready(InvocationUpdate::Succeeded { output, .. }) => {
            Some(Ok(output.value.unwrap_or(serde_json::Value::Null)))
        }
        Arrival::Ready(InvocationUpdate::Failed { error, .. }) => Some(Err(error.message)),
        Arrival::Ready(InvocationUpdate::Cancelled { message, .. }) => {
            Some(Err(message.unwrap_or_else(|| {
                "the request to the host was cancelled".into()
            })))
        }
        Arrival::Failed(error) => Some(Err(error.to_string())),
        Arrival::Ready(_) | Arrival::Pending { .. } => None,
    }
}

/// What the host found, as the window's engine reports it: each message in
/// its folder, and none with a place a refresh counts, since a search
/// reaches outside what the window syncs.
fn found_messages(found: FoundMail, account: &str) -> Vec<Message> {
    found
        .folders
        .into_iter()
        .flat_map(|found| {
            let folder = Folder::from_key(&found.folder).unwrap_or_default();
            let uid_validity = found.uid_validity;
            found.messages.into_iter().map(move |message| Message {
                server: None,
                ..message_of(message, folder, uid_validity, account)
            })
        })
        .collect()
}

/// A folder the host read, as the window's engine reports one. An `older`
/// page adds to what the window holds and names no window of its own.
fn synced(fetched: FetchedMail, account: &str, older: bool) -> Synced {
    let folder = Folder::from_key(&fetched.folder).unwrap_or_default();
    let uid_validity = fetched.uid_validity;
    let messages: Vec<Message> = fetched
        .messages
        .into_iter()
        .map(|message| message_of(message, folder, uid_validity, account))
        .collect();
    let window = messages
        .iter()
        .filter_map(|message| message.server.map(|at| at.uid))
        .fold(None::<(u32, u32)>, |window, uid| match window {
            None => Some((uid, uid)),
            Some((oldest, newest)) => Some((oldest.min(uid), newest.max(uid))),
        })
        .filter(|_| !older);
    Synced {
        uid_validity,
        reset: fetched.reset,
        messages,
        flags: fetched
            .flags
            .into_iter()
            .map(|flag| (flag.uid, flag.read, flag.starred))
            .collect(),
        window,
        has_older: Some(fetched.has_older),
        originals: Vec::new(),
    }
}

/// One message the host read, as the window keeps one.
fn message_of(
    message: person_accounts::FetchedMessage,
    folder: Folder,
    uid_validity: u32,
    account: &str,
) -> Message {
    let at = ServerPlace {
        folder,
        uid: message.uid,
        uid_validity,
    };
    Message {
        id: message_id(&message),
        rfc_message_id: message.message_id,
        in_reply_to: message.in_reply_to,
        from: Addr::new(message.from.name.unwrap_or_default(), message.from.email),
        to: message
            .to
            .into_iter()
            .map(|address| Addr::new(address.name.unwrap_or_default(), address.email))
            .collect(),
        cc: message
            .cc
            .into_iter()
            .map(|address| Addr::new(address.name.unwrap_or_default(), address.email))
            .collect(),
        subject: message.subject,
        body: message.body,
        html_body: message.html_body,
        attachments: message
            .attachments
            .into_iter()
            .map(|attachment| Attachment {
                filename: attachment.filename,
                mime: attachment.mime,
                size: attachment.size,
                hydrated: false,
                content_id: None,
                content: Vec::new(),
                kept: false,
                inline_id: attachment.inline_id,
            })
            .collect(),
        timestamp_min: message.minutes,
        read: message.read,
        starred: message.starred,
        folder,
        labels: Vec::new(),
        account: account.to_owned(),
        server: Some(at),
    }
}

/// The id the window keeps a message under: the same one a device derives
/// from its `Message-ID`, so both windows agree on a message.
fn message_id(message: &person_accounts::FetchedMessage) -> MessageId {
    message
        .message_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .map_or_else(uuid::Uuid::new_v4, crate::transport::stable_message_id)
}

/// The fields a window shows of one account the host keeps, without any
/// secret: the host opens the sessions itself.
pub(super) fn account(listed: &MailAccount) -> crate::account::Account {
    let mut account = crate::account::Account::new(crate::model::AccountConfig {
        display_name: listed.display_name.clone(),
        email: listed.address.clone(),
        username: listed.address.clone(),
        imap_host: listed.incoming_host.clone(),
        imap_port: listed.incoming_port,
        imap_tls: true,
        smtp_host: listed.outgoing_host.clone(),
        smtp_port: listed.outgoing_port,
        smtp_tls: true,
        auth: crate::model::AuthMethod::Password,
        import_pending: false,
        password: String::new(),
        identities: Vec::new(),
    });
    account.id.clone_from(&listed.id);
    account
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use std::sync::{Arc, Mutex};

    use serde_json::json;
    use ui::tilo_actions::remote::{
        ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, Completer,
        InvocationReceipt, InvocationState, InvokeRequest, Pending as HostPending, ReceiptStatus,
        pending,
    };
    use ui::tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};

    use super::*;

    /// The host that keeps the person's accounts, as a window asks it for
    /// them: the live reading it follows, and the accounts it keeps.
    #[derive(Default)]
    struct Keeping {
        accounts: Mutex<Vec<MailAccount>>,
        /// The reading the window follows: the update the host last sent, and
        /// the waits it wakes when the accounts change.
        reading: Mutex<Reading>,
        asked: Mutex<Vec<InvokeRequest>>,
    }

    #[derive(Default)]
    struct Reading {
        update: Option<InvocationUpdate>,
        waiting: Vec<Completer<()>>,
    }

    fn adas_account() -> MailAccount {
        MailAccount {
            id: "ada@example.test".into(),
            address: "ada@example.test".into(),
            display_name: "Ada".into(),
            incoming_host: "imap.example.test".into(),
            incoming_port: 993,
            outgoing_host: "smtp.example.test".into(),
            outgoing_port: 465,
            has_secret: true,
        }
    }

    impl Keeping {
        fn of(accounts: Vec<MailAccount>) -> Self {
            Self {
                accounts: Mutex::new(accounts),
                ..Self::default()
            }
        }

        /// The accounts as the window's reading answers them.
        fn listing(&self) -> serde_json::Value {
            let accounts = self.accounts.lock().unwrap();
            let active = accounts.first().map(|account| account.id.clone());
            serde_json::to_value(MailAccounts {
                accounts: accounts.clone(),
                active: active.unwrap_or_default(),
            })
            .unwrap()
        }

        /// Another window adds `account` through this host: it is kept, and
        /// the reading this window follows moves.
        fn added(&self, account: MailAccount) {
            self.accounts.lock().unwrap().push(account);
            let listing = self.listing();
            let waiting = {
                let mut reading = self.reading.lock().unwrap();
                let Some(InvocationUpdate::Item {
                    invocation,
                    sequence,
                    ..
                }) = reading.update.clone()
                else {
                    return;
                };
                reading.update = Some(InvocationUpdate::Item {
                    invocation,
                    sequence: sequence.saturating_add(1),
                    value: listing,
                });
                std::mem::take(&mut reading.waiting)
            };
            for wake in waiting {
                wake.complete(());
            }
        }

        fn asked(&self) -> Vec<InvokeRequest> {
            self.asked.lock().unwrap().clone()
        }
    }

    impl ActionHost for Keeping {
        fn query(
            &self,
            _: &Caller,
            _: CatalogueQuery,
        ) -> HostPending<Result<CataloguePage, ActionHostError>> {
            HostPending::ready(Ok(CataloguePage::default()))
        }

        fn invoke(
            &self,
            _: &Caller,
            request: InvokeRequest,
        ) -> HostPending<Result<InvocationReceipt, ActionHostError>> {
            self.asked.lock().unwrap().push(request.clone());
            let invocation = request.invocation;
            // The window's reading of the accounts: the whole listing now,
            // and again when they change.
            if request.target.address.action == person_accounts::WATCH {
                self.reading.lock().unwrap().update = Some(InvocationUpdate::Item {
                    invocation,
                    sequence: 1,
                    value: self.listing(),
                });
                return HostPending::ready(Ok(InvocationReceipt {
                    invocation,
                    status: ReceiptStatus::Accepted,
                }));
            }
            let value = match request.target.address.action.as_str() {
                person_accounts::FETCH => json!({
                    "account": "ada@example.test",
                    "folder": "inbox",
                    "uidValidity": 7,
                    "reset": true,
                    "hasOlder": false,
                    "newestUid": 42,
                    "messages": [{
                        "uid": 42,
                        "messageId": "42@example.test",
                        "from": {"name": "Ada", "email": "ada@example.test"},
                        "to": [{"email": "bob@example.test"}],
                        "cc": [],
                        "subject": "Hello",
                        "body": "Hi",
                        "minutes": 100,
                        "read": false,
                        "starred": false
                    }],
                    "flags": []
                }),
                person_accounts::SEARCH => json!({
                    "folders": [{
                        "folder": "archive",
                        "uidValidity": 9,
                        "messages": [{
                            "uid": 7,
                            "messageId": "old@example.test",
                            "from": {"name": "Ada", "email": "ada@example.test"},
                            "subject": "Quarterly numbers",
                            "body": "From last year.",
                            "minutes": 50,
                            "read": true,
                            "starred": false
                        }]
                    }]
                }),
                person_accounts::BEGIN_SEND => json!({ "send": "test-send" }),
                person_accounts::SEND_PIECE => {
                    let InvokeArgument::Json(argument) = &request.argument else {
                        return HostPending::ready(Err(ActionHostError::InvalidRequest {
                            reason: "a piece is a map".into(),
                        }));
                    };
                    let offset = argument["offset"].as_u64().unwrap_or(0);
                    let bytes = argument["bytes"]
                        .as_str()
                        .and_then(|hex| hex::decode(hex).ok())
                        .unwrap_or_default();
                    json!({ "written": offset + bytes.len() as u64 })
                }
                _ => serde_json::Value::Null,
            };
            HostPending::ready(Ok(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Completed {
                    output: ActionOutput {
                        value: Some(value),
                        resources: Vec::new(),
                    },
                },
            }))
        }

        fn observe(&self, _: &Caller, id: &InvocationId) -> Option<InvocationState> {
            let update = self.reading.lock().unwrap().update.clone()?;
            Some(InvocationState::new(*id, update))
        }

        fn next_change(&self, _: &Caller, _: &InvocationId) -> Option<HostPending<()>> {
            let (wake, waiting) = pending();
            self.reading.lock().unwrap().waiting.push(wake);
            Some(waiting)
        }

        fn cancel(
            &self,
            _: &Caller,
            id: &InvocationId,
        ) -> HostPending<Result<(), ActionHostError>> {
            self.reading.lock().unwrap().update = Some(InvocationUpdate::Cancelled {
                invocation: *id,
                message: None,
            });
            HostPending::ready(Ok(()))
        }
    }

    /// Take the accounts the host answered, polling as the window's frames
    /// do until it has.
    fn taken_accounts(mail: &mut HostMail) -> MailAccounts {
        for _ in 0..8 {
            mail.poll();
            if let Some(listed) = mail.take_accounts() {
                return listed;
            }
        }
        panic!("the host answered its accounts");
    }

    /// A page window's accounts and mail come from the host that keeps
    /// them, and the window's engine events are the host's answers.
    #[test]
    fn a_window_reads_its_accounts_and_mail_through_the_host() {
        let serving = Arc::new(Keeping::of(vec![adas_account()]));
        let _served = RemoteActions::enter(serving, Caller::local_person("ada"));
        let (handle, mut mail) = engine(tilo_content::ContentStore::memory());

        // The host reads its accounts, and the window takes what it sent.
        let listed = taken_accounts(&mut mail);
        let account = account(&listed.accounts[0]);
        assert_eq!(account.id, "ada@example.test");
        assert_eq!(account.config.email, "ada@example.test");
        assert!(account.config.password.is_empty());

        let config = account.config;
        handle
            .tx
            .send(EngineRequest::connect(config.clone()))
            .unwrap();
        handle
            .tx
            .send(EngineRequest::fetch(config, Folder::Inbox))
            .unwrap();
        assert!(mail.poll());

        let events: Vec<EngineEvent> = std::iter::from_fn(|| handle.rx.try_recv().ok()).collect();
        assert!(matches!(
            events.first(),
            Some(EngineEvent::Connected { account_id, .. }) if account_id == "ada@example.test"
        ));
        let fetched = events
            .iter()
            .find_map(|event| match event {
                EngineEvent::Fetched { folder, synced, .. } => Some((folder, synced)),
                _ => None,
            })
            .expect("the host's folder read reaches the window");
        assert_eq!(*fetched.0, Folder::Inbox);
        assert!(fetched.1.reset);
        assert_eq!(fetched.1.uid_validity, 7);
        assert_eq!(fetched.1.messages[0].subject, "Hello");
        assert_eq!(fetched.1.messages[0].from.email, "ada@example.test");
        assert_eq!(fetched.1.messages[0].server.map(|at| at.uid), Some(42));
    }

    /// A window scrolled past its oldest message asks the host for the page
    /// before it, and a search asks the host's server. What is found comes
    /// back as messages outside what a refresh counts.
    #[test]
    fn a_window_asks_the_host_for_older_mail_and_for_what_a_search_finds() {
        let serving = Arc::new(Keeping::of(vec![adas_account()]));
        let _served = RemoteActions::enter(serving.clone(), Caller::local_person("ada"));
        let (handle, mut mail) = engine(tilo_content::ContentStore::memory());
        let config = crate::model::AccountConfig {
            email: "ada@example.test".into(),
            ..crate::model::AccountConfig::default()
        };
        handle
            .tx
            .send(EngineRequest::fetch_wanting(
                config.clone(),
                Folder::Inbox,
                crate::sync::Want::Older {
                    known: crate::sync::Window {
                        uid_validity: 7,
                        newest: 42,
                        oldest: 12,
                    },
                },
            ))
            .unwrap();
        handle
            .tx
            .send(EngineRequest::search(config, "quarterly".into()))
            .unwrap();
        assert!(mail.poll());

        let asked = serving.asked();
        let argument = |action: &str| {
            let request = asked
                .iter()
                .find(|request| request.target.address.action == action)
                .expect("the host is asked");
            let InvokeArgument::Json(argument) = &request.argument else {
                panic!("the request carries what it asks");
            };
            argument.clone()
        };
        let older = argument(person_accounts::FETCH);
        assert_eq!(older["beforeUid"], json!(12));
        assert_eq!(older["knownUidValidity"], json!(7));
        assert!(older.get("knownUid").is_none());
        assert_eq!(
            argument(person_accounts::SEARCH)["text"],
            json!("quarterly")
        );

        let events: Vec<EngineEvent> = std::iter::from_fn(|| handle.rx.try_recv().ok()).collect();
        let page = events
            .iter()
            .find_map(|event| match event {
                EngineEvent::Fetched { synced, .. } => Some(synced),
                _ => None,
            })
            .expect("the older page reaches the window");
        assert_eq!(
            page.window, None,
            "an older page names no window of its own"
        );
        let found = events
            .iter()
            .find_map(|event| match event {
                EngineEvent::Found { messages, .. } => Some(messages),
                _ => None,
            })
            .expect("what the host found reaches the window");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].subject, "Quarterly numbers");
        assert_eq!(found[0].folder, Folder::Archive);
        assert!(found[0].server.is_none());
    }

    /// An account another window adds through the host reaches this window
    /// through what it follows: no timer asks the host again, and the
    /// window's listing is the host's whole answer.
    #[test]
    fn an_account_added_in_another_window_reaches_this_one() {
        let serving = Arc::new(Keeping::of(vec![adas_account()]));
        let _served = RemoteActions::enter(serving.clone(), Caller::local_person("ada"));
        let (_handle, mut mail) = engine(tilo_content::ContentStore::memory());
        let listed = taken_accounts(&mut mail);
        assert_eq!(listed.accounts.len(), 1);

        serving.added(MailAccount {
            id: "bob@example.test".into(),
            address: "bob@example.test".into(),
            display_name: "Bob".into(),
            incoming_host: "imap.example.test".into(),
            incoming_port: 993,
            outgoing_host: "smtp.example.test".into(),
            outgoing_port: 465,
            has_secret: true,
        });
        let listed = taken_accounts(&mut mail);
        assert_eq!(
            listed
                .accounts
                .iter()
                .map(|account| account.address.as_str())
                .collect::<Vec<_>>(),
            ["ada@example.test", "bob@example.test"]
        );
        assert!(
            serving
                .asked()
                .iter()
                .all(|request| request.target.address.action != person_accounts::LIST),
            "the window asks for the listing once and follows it, never listing again"
        );
    }

    /// An account a page takes in — added in Settings or moved in by another
    /// mail program — is the host's to keep, with the sign-in and the other
    /// names it sends as that travelled with it.
    #[test]
    fn a_page_hands_the_account_it_takes_in_to_the_host() {
        let serving = Arc::new(Keeping::of(vec![adas_account()]));
        let _served = RemoteActions::enter(serving.clone(), Caller::local_person("ada"));
        let (_handle, mut mail) = engine(tilo_content::ContentStore::memory());
        let config = crate::model::AccountConfig {
            display_name: "Ada".into(),
            email: "ada@example.test".into(),
            username: "ada@example.test".into(),
            imap_host: "imap.example.test".into(),
            imap_port: 993,
            smtp_host: "smtp.example.test".into(),
            smtp_port: 465,
            identities: vec!["Ada at work <ada@work.example.test>".into()],
            password: "thunderbird-secret".into(),
            ..crate::model::AccountConfig::default()
        };
        mail.add_account(&config);
        assert!(mail.poll());

        let asked = serving.asked();
        let adding = asked
            .iter()
            .rev()
            .find(|request| request.target.address.action == person_accounts::ADD_MAIL)
            .expect("the host is asked to keep the account");
        let InvokeArgument::Json(argument) = &adding.argument else {
            panic!("the account is an argument");
        };
        assert_eq!(argument["address"], json!("ada@example.test"));
        assert_eq!(argument["password"], json!("thunderbird-secret"));
        assert_eq!(
            argument["identities"],
            json!(["Ada at work <ada@work.example.test>"])
        );
    }

    /// A draft the window keeps on its server is begun with the host as a
    /// message is, and kept in the account's Drafts folder instead of sent.
    #[test]
    fn a_draft_is_begun_with_the_host_and_kept_instead_of_sent() {
        let serving = Arc::new(Keeping::of(vec![adas_account()]));
        let _served = RemoteActions::enter(serving.clone(), Caller::local_person("ada"));
        let (handle, mut mail) = engine(tilo_content::ContentStore::memory());
        handle
            .tx
            .send(EngineRequest::save_draft(
                crate::model::AccountConfig {
                    email: "ada@example.test".into(),
                    ..crate::model::AccountConfig::default()
                },
                Box::new(crate::model::Draft {
                    to: "bob@example.test".into(),
                    subject: "Not yet".into(),
                    body: "Still thinking.".into(),
                    ..crate::model::Draft::default()
                }),
            ))
            .unwrap();

        let mut kept = false;
        for _ in 0..8 {
            mail.poll();
            while let Ok(event) = handle.rx.try_recv() {
                assert!(
                    !matches!(event, EngineEvent::Error { .. } | EngineEvent::Sent { .. }),
                    "{event:?}"
                );
                kept |= matches!(
                    event,
                    EngineEvent::Completed {
                        kind: EngineRequestKind::SaveDraft,
                        ..
                    }
                );
            }
            if kept {
                break;
            }
        }
        assert!(kept, "the host's answer settles the draft");
        let actions: Vec<String> = serving
            .asked()
            .iter()
            .map(|request| request.target.address.action.clone())
            .filter(|action| action != person_accounts::WATCH)
            .collect();
        assert_eq!(
            actions,
            [person_accounts::BEGIN_SEND, person_accounts::SAVE_DRAFT]
        );
    }

    /// A send written in the page is begun with the host that keeps the
    /// account, its file's bytes handed over a piece at a time, and sent,
    /// carrying no secret: the host signs in with the one it keeps.
    #[test]
    fn a_send_hands_its_files_to_the_host_a_piece_at_a_time() {
        let serving = Arc::new(Keeping::of(vec![adas_account()]));
        let _served = RemoteActions::enter(serving.clone(), Caller::local_person("ada"));
        let (handle, mut mail) = engine(tilo_content::ContentStore::memory());
        let file = |name: &str, bytes: &[u8]| crate::model::SendFile {
            attachment: crate::model::DraftAttachment {
                content_id: format!("mail-attachment:blake3:{}", "0".repeat(64)),
                filename: name.into(),
                mime: "application/pdf".into(),
                size: bytes.len() as u64,
            },
            bytes: bytes.to_vec(),
        };
        let bytes = vec![7u8; MAX_SEND_PIECE_BYTES + 10];
        handle
            .tx
            .send(EngineRequest::send(
                crate::model::AccountConfig {
                    email: "ada@example.test".into(),
                    ..crate::model::AccountConfig::default()
                },
                Box::new(crate::model::Draft {
                    to: "bob@example.test".into(),
                    subject: "Contract".into(),
                    body: "Attached.".into(),
                    calendar_ics: Some("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n".into()),
                    attachments: vec![file("contract.pdf", &bytes).attachment],
                    ..crate::model::Draft::default()
                }),
                vec![file("contract.pdf", &bytes)],
            ))
            .unwrap();

        let mut sent = false;
        for _ in 0..40 {
            mail.poll();
            while let Ok(event) = handle.rx.try_recv() {
                assert!(!matches!(event, EngineEvent::Error { .. }), "{event:?}");
                sent |= matches!(event, EngineEvent::Sent { .. });
            }
            if sent {
                break;
            }
        }
        assert!(sent, "the host's send answers the window");

        let asked = serving.asked();
        let steps: Vec<&ui::tilo_actions::remote::InvokeRequest> = asked
            .iter()
            .filter(|request| {
                matches!(
                    request.target.address.action.as_str(),
                    person_accounts::BEGIN_SEND
                        | person_accounts::SEND_PIECE
                        | person_accounts::SEND
                )
            })
            .collect();
        let actions: Vec<&str> = steps
            .iter()
            .map(|request| request.target.address.action.as_str())
            .collect();
        assert_eq!(
            actions,
            [
                person_accounts::BEGIN_SEND,
                person_accounts::SEND_PIECE,
                person_accounts::SEND_PIECE,
                person_accounts::SEND,
            ],
            "the message is begun, then its file's pieces, then sent"
        );
        for request in &steps {
            let InvokeArgument::Json(argument) = &request.argument else {
                panic!("every step is a map");
            };
            assert!(
                !format!("{argument:?}").contains("password"),
                "{argument:?}"
            );
        }
        let InvokeArgument::Json(beginning) = &steps[0].argument else {
            panic!("a beginning is a map");
        };
        assert_eq!(beginning["message"]["to"], json!("bob@example.test"));
        assert_eq!(
            beginning["files"][0]["size"],
            json!((MAX_SEND_PIECE_BYTES + 10) as u64)
        );
        assert_eq!(
            beginning["calendarIcs"],
            json!("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n")
        );
        let InvokeArgument::Json(first) = &steps[1].argument else {
            panic!("a piece is a map");
        };
        assert_eq!(first["offset"], json!(0));
        let InvokeArgument::Json(second) = &steps[2].argument else {
            panic!("a piece is a map");
        };
        assert_eq!(second["offset"], json!(MAX_SEND_PIECE_BYTES as u64));
    }
}
