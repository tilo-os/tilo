//! Mail against a mail server: the package's window kept in step with a test IMAP server through
//! the mail origin and native Mail's engine, and sending through a test SMTP server from the
//! outbox. What people do goes through the package's actions, as a press does.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mail::fake_servers::{FakeImap, FakeSmtp, Layout, accepting_client_config, server_tls_config};
use mail::model::{AccountConfig, AuthMethod};
use tilolang_tilo::runtime::{Principal, ResourceRef, Value as RtValue};

use super::Room;
use crate::window::Window;
use tilolang_mail::engine::EngineConnector;
use tilolang_mail::origin::ImapOrigin;
use ui::framework::RunMode;

const APP: &str = "so.tilo.lang.mail";
const ME: &str = "sam@rivera.example";

/// A message as the second account's server holds it: one to `to`.
fn mail_to(to: &str, from: &str, subject: &str, body: &str) -> String {
    format!(
        "From: {from}\nTo: {to}\nSubject: {subject}\nMessage-ID: <{subject}@{to}>\n\
Date: Tue, 30 Jun 2026 09:00:00 +0000\n\n{body}\n"
    )
}

/// A message as a server holds it.
fn mail(id: &str, from: &str, subject: &str, body: &str, answers: Option<&str>) -> String {
    let threading = answers.map_or_else(String::new, |parent| {
        format!("In-Reply-To: <{parent}>\nReferences: <{parent}>\n")
    });
    format!(
        "From: {from}\nTo: {ME}\nSubject: {subject}\nMessage-ID: <{id}>\n{threading}\
Date: Tue, 30 Jun 2026 09:00:00 +0000\n\n{body}\n"
    )
}

struct Servers {
    imap: FakeImap,
    smtp: FakeSmtp,
}

fn servers() -> Servers {
    let rt = ui::rt::app();
    let imap = rt
        .block_on(FakeImap::start(Layout::Gmail, server_tls_config()))
        .expect("the IMAP server starts");
    let smtp = rt
        .block_on(FakeSmtp::start(server_tls_config()))
        .expect("the SMTP server starts");
    imap.append(
        "INBOX",
        &mail(
            "plan@hopper.example",
            "Grace Hopper <grace@hopper.example>",
            "Launch plan",
            "Here is the draft plan.",
            None,
        ),
        &[],
    );
    imap.append(
        "INBOX",
        &mail(
            "plan-2@analytical.example",
            "Ada Lovelace <ada@analytical.example>",
            "Re: Launch plan",
            "Agreed on the twelfth.",
            Some("plan@hopper.example"),
        ),
        &[],
    );
    imap.append(
        "INBOX",
        &mail(
            "dinner@chen.example",
            "Bo Chen <bo@chen.example>",
            "Dinner on Friday?",
            "Shall we try the new place?",
            None,
        ),
        &["\\Seen"],
    );
    Servers { imap, smtp }
}

/// The config of the second account: another address on `servers`' own ports.
fn work_account(servers: &Servers) -> AccountConfig {
    AccountConfig {
        display_name: "Sam at work".into(),
        email: "work@rivera.example".into(),
        username: "work@rivera.example".into(),
        password: "app-password".into(),
        imap_host: "127.0.0.1".into(),
        imap_port: servers.imap.port,
        smtp_host: "127.0.0.1".into(),
        smtp_port: servers.smtp.port,
        auth: AuthMethod::Password,
        ..Default::default()
    }
}

fn account(servers: &Servers) -> AccountConfig {
    AccountConfig {
        display_name: "Sam Rivera".into(),
        email: ME.into(),
        username: ME.into(),
        password: "app-password".into(),
        imap_host: "127.0.0.1".into(),
        imap_port: servers.imap.port,
        smtp_host: "127.0.0.1".into(),
        smtp_port: servers.smtp.port,
        auth: AuthMethod::Password,
        ..Default::default()
    }
}

/// The Mail window kept in step with `config`'s servers.
fn room_with(config: AccountConfig) -> Room {
    room_over(Box::new(move || vec![config.clone()]), Arc::default())
}

/// A room whose accounts are what `accounts` says each time it is asked, which comes online
/// again when `reconnected` is set.
fn room_over(accounts: tilolang_mail::engine::AccountSource, reconnected: Arc<AtomicBool>) -> Room {
    let woken = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&woken);
    let engine = mail::engine::live::LiveEngine::trusting(
        tilo_content::ContentStore::memory(),
        Arc::new(|| {}),
        accepting_client_config(),
    )
    .expect("the engine starts");
    let (requests, events) = mail::engine::spawn_waking(
        Box::new(engine),
        Arc::new(move || flag.store(true, Ordering::Release)),
    );
    let connector = EngineConnector::new(accounts, requests, events);
    let origin = std::rc::Rc::new(std::cell::RefCell::new(
        ImapOrigin::new(Box::new(connector), woken).reconnecting_on(reconnected),
    ));
    room_of(origin)
}

/// A room kept in step with a mail server by `origin`.
fn room_of(origin: std::rc::Rc<std::cell::RefCell<ImapOrigin>>) -> Room {
    crate::app::builtin::choose("mail");
    let meta = crate::app::builtin::package_named("mail").expect("the mail package");
    let sink = tilolang_tilo::runtime::MemorySink::new();
    let window = Window::new(meta.app.clone(), "ada".to_owned(), RunMode::Live)
        .with_sink(sink.clone())
        .with_origin(origin);
    let mut room = Room {
        doc: ui::automerge::AutoCommit::new(),
        window,
        sink,
    };
    room.settle();
    room
}

impl Room {
    fn host(&self) -> tilolang_tilo::host::Host {
        self.window.current_host().expect("the host started")
    }

    /// Frames until `done` holds of the room, as the servers answer.
    fn until(&mut self, what: &str, done: impl Fn(&mut Self) -> bool) {
        for _ in 0..400 {
            self.settle();
            if done(self) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        panic!("timed out waiting for {what}");
    }

    fn read(&self, at: &ResourceRef, field: &str) -> RtValue {
        self.host()
            .runtime()
            .read(at, field)
            .expect("the field reads")
    }

    fn refs(&self, at: &ResourceRef, field: &str) -> Vec<ResourceRef> {
        match self.read(at, field) {
            RtValue::List(items) => items
                .into_iter()
                .filter_map(|v| match v {
                    RtValue::Ref(r) => Some((*r.target).clone()),
                    _ => None,
                })
                .collect(),
            other => panic!("{field} is not a list: {other:?}"),
        }
    }

    fn text(&self, at: &ResourceRef, field: &str) -> String {
        super::rt_text(&self.read(at, field))
    }

    fn flag(&self, at: &ResourceRef, field: &str) -> bool {
        matches!(self.read(at, field), RtValue::Bool(true) | RtValue::Int(1))
    }

    fn accounts(&self) -> Vec<ResourceRef> {
        let root = self.host().root_of(APP).expect("the root");
        self.refs(&root, "accounts")
    }

    fn threads(&self) -> Vec<ResourceRef> {
        self.accounts()
            .first()
            .map(|a| self.refs(a, "threads"))
            .unwrap_or_default()
    }

    fn thread(&self, subject: &str) -> ResourceRef {
        self.threads()
            .into_iter()
            .find(|t| self.text(t, "subject") == subject)
            .unwrap_or_else(|| panic!("no conversation {subject}"))
    }

    /// Invoke the package's action `name` as the person at the window, as a press does.
    fn press(&mut self, name: &str, args: Vec<RtValue>) -> RtValue {
        let meta = crate::app::builtin::package_named("mail").expect("the mail package");
        let action = meta
            .actions
            .iter()
            .find(|a| a.name == name)
            .unwrap_or_else(|| panic!("no action {name}"));
        let out = self
            .host()
            .runtime()
            .invoke_action(&Principal::new("ada"), (action.id.as_str(), args))
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        self.window.commit_planned();
        self.settle();
        match out {
            tilolang_tilo::runtime::InvokeOutcome::Done(value) => value,
            other => panic!("{name} did not run: {other:?}"),
        }
    }
}

fn synced(room: &mut Room) {
    room.until("the inbox to sync", |room| room.threads().len() == 2);
}

#[test]
fn an_account_syncs_into_mailboxes_and_conversations() {
    let servers = servers();
    let mut room = room_with(account(&servers));
    synced(&mut room);
    let account = room.accounts().remove(0);
    assert_eq!(room.text(&account, "address"), ME);
    assert_eq!(room.text(&account, "name"), "Sam Rivera");
    let boxes: Vec<String> = room
        .refs(&account, "mailboxes")
        .iter()
        .map(|b| room.text(b, "key"))
        .collect();
    assert_eq!(
        boxes,
        ["inbox", "archive", "sent", "drafts", "junk", "trash"]
    );

    // The reply joins the conversation it answers; the other stands alone.
    let plan = room.thread("Launch plan");
    let messages = room.refs(&plan, "messages");
    assert_eq!(messages.len(), 2);
    assert_eq!(room.text(&messages[0], "from_name"), "Grace Hopper");
    assert_eq!(
        room.text(&messages[1], "body").trim(),
        "Agreed on the twelfth."
    );
    assert!(
        room.flag(&plan, "unread"),
        "an unread message makes it unread"
    );
    assert_eq!(room.text(&plan, "senders"), "Ada Lovelace, Grace Hopper");
    let dinner = room.thread("Dinner on Friday?");
    assert!(!room.flag(&dinner, "unread"), "it was read on the server");

    // Syncing again finds nothing new and makes nothing twice.
    room.until("the inbox counts", |room| {
        let inbox = room.refs(&room.accounts()[0], "mailboxes")[0].clone();
        matches!(room.read(&inbox, "unread"), RtValue::Int(2))
    });
    assert_eq!(room.threads().len(), 2);
}

#[test]
fn reading_and_archiving_reach_the_server_and_come_back() {
    let servers = servers();
    let mut room = room_with(account(&servers));
    synced(&mut room);
    let plan = room.thread("Launch plan");

    room.press("mark_read", vec![RtValue::resource(&plan)]);
    assert!(!room.flag(&plan, "unread"), "it reads as read at once");
    room.until("the server to hold it read", |_| {
        servers
            .imap
            .messages("INBOX")
            .iter()
            .filter(|m| m.flags.iter().any(|f| f == "\\Seen"))
            .count()
            == 3
    });

    room.press("archive", vec![RtValue::resource(&plan)]);
    assert!(
        !matches!(room.read(&plan, "boxes"), RtValue::List(ref b) if b.contains(&RtValue::text("inbox"))),
        "it leaves the inbox at once"
    );
    room.until("the server to archive it", |_| {
        servers.imap.messages("INBOX").len() == 1
            && servers.imap.messages("[Gmail]/All Mail").len() == 2
    });

    // Taken back to the inbox, the server moves it back.
    room.press("to_inbox", vec![RtValue::resource(&plan)]);
    room.until("the server to hold it in the inbox again", |_| {
        servers.imap.messages("INBOX").len() == 3
    });
    assert_eq!(room.threads().len(), 2, "moving makes nothing twice");
}

#[test]
fn opening_a_conversation_reads_it_here_and_on_the_server() {
    let servers = servers();
    let mut room = room_with(account(&servers));
    synced(&mut room);
    let plan = room.thread("Launch plan");
    assert!(room.flag(&plan, "unread"));
    let row = room
        .items()
        .into_iter()
        .find(|(_, item)| {
            item.item
                .as_ref()
                .is_some_and(|shown| shown.0 == plan.id_string())
        })
        .map(|(key, _)| key)
        .expect("the inbox lists the conversation");
    room.activate(&row);
    assert!(!room.flag(&plan, "unread"), "it reads as read once opened");
    room.until("the server to hold it read", |_| {
        servers
            .imap
            .messages("INBOX")
            .iter()
            .all(|m| m.flags.iter().any(|f| f == "\\Seen"))
    });
}

#[test]
fn a_reply_goes_out_through_the_outbox_once_the_server_takes_it() {
    let servers = servers();
    let mut room = room_with(account(&servers));
    synced(&mut room);
    hold_for(&mut room, 0);
    let plan = room.thread("Launch plan");
    let RtValue::Ref(draft) = room.press("reply", vec![RtValue::resource(&plan)]) else {
        panic!("a reply is a draft");
    };
    let draft = (*draft.target).clone();
    assert_eq!(room.text(&draft, "to"), "ada@analytical.example");
    assert_eq!(room.text(&draft, "subject"), "Re: Launch plan");
    assert_eq!(
        room.text(&draft, "in_reply_to"),
        "plan-2@analytical.example"
    );

    room.press("send", vec![RtValue::resource(&draft)]);
    room.until("the server to take the reply", |_| {
        servers.smtp.sent().len() == 1
    });
    room.until("the outbox to settle", |room| {
        room.host().store().outbox_pending_count() == 0
    });
    let sent = String::from_utf8_lossy(&servers.smtp.sent()[0]).into_owned();
    assert!(sent.contains("Subject: Re: Launch plan"), "{sent}");
    assert!(
        sent.contains("In-Reply-To: <plan-2@analytical.example>"),
        "{sent}"
    );
    assert!(room.flag(&draft, "sent"));
    assert!(
        room.sink.delivered().is_empty(),
        "a send is the connector's, never the development log's"
    );
}

#[test]
fn an_unreachable_server_is_said_on_the_account() {
    let servers = servers();
    let mut config = account(&servers);
    // Nothing listens there.
    config.imap_port = 1;
    let mut room = room_with(config);
    room.until("the account to say why", |room| {
        room.accounts()
            .first()
            .is_some_and(|a| !room.text(a, "trouble").is_empty())
    });
    let account = room.accounts().remove(0);
    assert!(!room.flag(&account, "online"));
    assert!(room.threads().is_empty());
}

/// What the draft's window shows, as text.
fn editor_of(room: &Room, draft: &ResourceRef) -> String {
    let view = room
        .host()
        .open_resource(draft, None)
        .expect("the draft's window opens");
    format!("{:?}", view.tree())
}

/// Hold a sent message for `secs` seconds: the person's setting, written where Mail keeps it,
/// as a step of theirs in the record.
fn hold_for(room: &mut Room, secs: i64) {
    let root = room.host().root_of(APP).expect("the root");
    let meta = crate::app::builtin::package_named("mail").expect("the mail package");
    let field = meta
        .types
        .iter()
        .find(|t| t.name == "Mail")
        .and_then(|t| t.fields.iter().find(|f| f.name == "hold"))
        .expect("Mail keeps a hold")
        .id
        .clone();
    use ui::tilo_resource::Edit;
    let edit = Edit::new(
        vec![ui::tilo_resource::Segment::Field(field)],
        ui::tilo_resource::EditOp::Set {
            value: serde_json::json!(secs),
        },
    );
    let action = tilolang_tilo::action::DynAction {
        app: APP.to_owned(),
        kind: format!("tl.{APP}.Mail"),
        action_id: "$edit".to_owned(),
        args: serde_json::Value::Null,
        authorship: tilolang_tilo::action::Authorship::default(),
        edits: vec![tilolang_tilo::action::DynEdit::Edit {
            id: root.doc.clone(),
            edit,
        }],
    };
    let context =
        ui::tilo_actions::ActionContext::for_resource(&root.doc, &action.kind, &action.kind)
            .with_app(APP, "tilolang")
            .with_invoker(ui::tilo_actions::ActionInvoker::actor("ada".to_owned()));
    ui::tilo_actions::apply_local_with_context(
        &mut room.doc,
        &action,
        action.encode().expect("it encodes"),
        Some(b"ada"),
        context,
    )
    .expect("the hold is written");
    room.doc.commit();
    room.settle();
}

/// A reply to the conversation `subject`, sent: the draft it made.
fn sent_reply(room: &mut Room, subject: &str) -> ResourceRef {
    let plan = room.thread(subject);
    let RtValue::Ref(draft) = room.press("reply", vec![RtValue::resource(&plan)]) else {
        panic!("a reply is a draft");
    };
    let draft = (*draft.target).clone();
    room.press("send", vec![RtValue::resource(&draft)]);
    draft
}

#[test]
fn a_sent_message_waits_for_the_persons_hold_before_it_goes() {
    let servers = servers();
    let mut room = room_with(account(&servers));
    synced(&mut room);
    hold_for(&mut room, 600);
    let draft = sent_reply(&mut room, "Launch plan");
    room.settle();
    // It is held: nothing went, and it says how it stands.
    assert!(room.flag(&draft, "sent"));
    assert!(
        servers.smtp.sent().is_empty(),
        "a held message does not go yet"
    );
    assert_eq!(delivery_of(&room, &draft).unwrap().0, "held");
    let shown = editor_of(&room, &draft);
    assert!(shown.contains("Sending in a moment"), "{shown}");
}

#[test]
fn a_held_message_is_taken_back_and_never_goes() {
    let servers = servers();
    let mut room = room_with(account(&servers));
    synced(&mut room);
    hold_for(&mut room, 600);
    let draft = sent_reply(&mut room, "Launch plan");
    assert_eq!(delivery_of(&room, &draft).unwrap().0, "held");

    // Its author takes it back: it never leaves, and it is a draft again.
    room.press("undo_send", vec![RtValue::resource(&draft)]);
    room.settle();
    assert!(!room.flag(&draft, "sent"), "it is a draft again");
    assert!(
        room.flag(&draft, "recalled"),
        "and it says it was taken back"
    );
    assert_eq!(delivery_of(&room, &draft).unwrap().0, "withdrawn");
    for _ in 0..20 {
        room.settle();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(servers.smtp.sent().is_empty(), "it never went");
}

#[test]
fn a_message_that_went_cannot_be_taken_back() {
    let servers = servers();
    let mut room = room_with(account(&servers));
    synced(&mut room);
    // No hold: it goes at once.
    hold_for(&mut room, 0);
    let draft = sent_reply(&mut room, "Launch plan");
    room.until("the server to take the message", |_| {
        servers.smtp.sent().len() == 1
    });
    room.until("the outbox to settle", |room| {
        room.host().store().outbox_pending_count() == 0
    });
    let meta = crate::app::builtin::package_named("mail").expect("the mail package");
    let action = meta
        .actions
        .iter()
        .find(|a| a.name == "undo_send")
        .expect("undo_send");
    let refused = room.host().runtime().invoke_action(
        &Principal::new("ada"),
        (action.id.as_str(), vec![RtValue::resource(&draft)]),
    );
    assert!(refused.is_err(), "a message that went is not taken back");
    assert_eq!(servers.smtp.sent().len(), 1);
}

#[test]
fn the_window_wakes_when_a_held_message_is_due() {
    let servers = servers();
    let mut room = room_with(account(&servers));
    synced(&mut room);
    hold_for(&mut room, 600);
    let draft = sent_reply(&mut room, "Launch plan");
    room.settle();
    // The window waits for the moment the message is due, not for nothing.
    let wait = room
        .window
        .instance()
        .wakes_in()
        .expect("a held message sets the window's next wake");
    assert!(
        wait > std::time::Duration::from_secs(500),
        "it waits for the hold: {wait:?}"
    );
    assert_eq!(delivery_of(&room, &draft).unwrap().0, "held");
}

/// The row of `message` in the conversation `thread`, as activating its list row made it: what
/// it shows, and the keys of the controls under the row's fold.
fn message_rows(room: &mut Room, thread: &ResourceRef) -> Vec<Vec<String>> {
    let row = room
        .items()
        .into_iter()
        .find(|(_, item)| {
            item.item
                .as_ref()
                .is_some_and(|shown| shown.0 == thread.id_string())
        })
        .map(|(key, _)| key)
        .expect("the inbox lists the conversation");
    room.activate(&row);
    let detail = room
        .window
        .instance()
        .view(tilo_render_protocol::patch::Lane::Detail)
        .expect("the view runs");
    let list = first_kind(detail.borrow().tree(), "ui.VirtualList").expect("a transcript");
    let len = detail.borrow().list_len(&list).unwrap_or(0);
    let mut out = Vec::new();
    for index in 0..len {
        let shown = detail
            .borrow_mut()
            .row(&list, index)
            .expect("a row renders")
            .cloned();
        let mut texts = Vec::new();
        if let Some(shown) = &shown {
            texts_in(shown, &mut texts);
        }
        out.push(texts);
    }
    room.settle();
    out
}

// USER REQUIREMENT: a reply's quotation of the message it answers folds in the conversation, as
// Gmail and Apple Mail fold it, so a thread reads as what each person said.
#[test]
fn a_replys_quotation_folds_in_the_conversation() {
    let servers = servers();
    let mut room = room_with(account(&servers));
    synced(&mut room);
    hold_for(&mut room, 0);
    let plan = room.thread("Launch plan");
    // A message of the conversation that answers another, as the composer makes one: what it
    // says, then the message it answers quoted under it, as its sender's client wrote it.
    let replies = room.refs(&plan, "messages");
    let reply = replies[1].clone();
    add_quote(
        &mut room,
        &reply,
        "The fifth works for the beta.",
        "Here is the draft plan.",
    );
    let rows = message_rows(&mut room, &plan);
    let reply = rows.iter().find(|texts| {
        texts
            .iter()
            .any(|t| t.contains("The fifth works for the beta."))
    });
    let reply = reply.unwrap_or_else(|| {
        panic!("the conversation shows the reply's quotation folded: {rows:#?}")
    });
    assert!(
        reply
            .iter()
            .any(|t| t == "Show quoted text" || t == "Hide quoted text"),
        "the quotation folds under a control: {reply:#?}"
    );
    assert!(
        !reply.iter().any(|t| t.contains("Here is the draft plan.")),
        "the quotation is folded away until it is opened: {reply:#?}"
    );
    // The first message of the conversation quotes nothing: it has no fold.
    let first = &rows[0];
    assert!(
        !first
            .iter()
            .any(|t| t == "Show quoted text" || t == "Hide quoted text"),
        "a message that quotes nothing has no fold: {first:#?}"
    );
}

/// Say `said` in the message `message`, quoting `quote` under it, as a client that answers
/// writes it: a step of the person's in the record.
fn add_quote(room: &mut Room, message: &ResourceRef, said: &str, quote: &str) {
    let meta = crate::app::builtin::package_named("mail").expect("the mail package");
    let field = |ty: &str, name: &str| {
        meta.types
            .iter()
            .find(|t| t.name == ty)
            .and_then(|t| t.fields.iter().find(|f| f.name == name))
            .expect("the field")
            .id
            .clone()
    };
    let body = format!(
        "{said}\r\n\r\nOn 2026-06-30, Grace Hopper wrote:\r\n> {}",
        quote.replace('\n', "\r\n> ")
    );
    let edit = ui::tilo_resource::Edit::new(
        vec![
            ui::tilo_resource::Segment::Item(ui::tilo_resource::Id::from_raw(
                message.item.as_deref().expect("the message is an item"),
            )),
            ui::tilo_resource::Segment::Field(field("Message", "body")),
        ],
        ui::tilo_resource::EditOp::Replace { text: body },
    );
    let action = tilolang_tilo::action::DynAction {
        app: APP.to_owned(),
        kind: format!("tl.{APP}.Message"),
        action_id: "$edit".to_owned(),
        args: serde_json::Value::Null,
        authorship: tilolang_tilo::action::Authorship::default(),
        edits: vec![tilolang_tilo::action::DynEdit::Edit {
            id: message.doc.clone(),
            edit,
        }],
    };
    let context =
        ui::tilo_actions::ActionContext::for_resource(&message.doc, &action.kind, &action.kind)
            .with_app(APP, "tilolang")
            .with_invoker(ui::tilo_actions::ActionInvoker::actor("ada".to_owned()));
    ui::tilo_actions::apply_local_with_context(
        &mut room.doc,
        &action,
        action.encode().expect("it encodes"),
        Some(b"ada"),
        context,
    )
    .expect("the message says it");
    room.doc.commit();
    room.settle();
}

/// How the draft `draft` stands on its way out, as `delivery(d)` reads it.
fn delivery_of(room: &Room, draft: &ResourceRef) -> Option<(String, String)> {
    use tilolang_tilo::runtime::store::Store as _;
    room.host()
        .store()
        .outbox_states()
        .into_iter()
        .find(|(doc, _)| *doc == draft.doc)
        .map(|(_, state)| (state.state, state.reason))
}

#[test]
fn a_message_waits_to_go_online_and_goes_when_the_network_is_back() {
    let servers = servers();
    let reachable = account(&servers);
    // The SMTP server cannot be reached yet: nothing listens there.
    let config = Arc::new(std::sync::Mutex::new(AccountConfig {
        smtp_port: 1,
        ..reachable.clone()
    }));
    let accounts = Arc::clone(&config);
    let reconnected = Arc::new(AtomicBool::new(false));
    let mut room = room_over(
        Box::new(move || vec![accounts.lock().unwrap().clone()]),
        Arc::clone(&reconnected),
    );
    synced(&mut room);
    hold_for(&mut room, 0);
    let plan = room.thread("Launch plan");
    let RtValue::Ref(draft) = room.press("reply", vec![RtValue::resource(&plan)]) else {
        panic!("a reply is a draft");
    };
    let draft = (*draft.target).clone();
    room.press("send", vec![RtValue::resource(&draft)]);
    room.until("the message to wait to go online", |room| {
        delivery_of(room, &draft)
            .is_some_and(|(state, reason)| state == "waiting" && !reason.is_empty())
    });
    // The draft says so where it is written.
    let shown = editor_of(&room, &draft);
    assert!(shown.contains("Waiting to go online"), "{shown}");
    // It is not tried again on its own.
    for _ in 0..20 {
        room.settle();
    }
    assert!(servers.smtp.sent().is_empty());
    assert_eq!(delivery_of(&room, &draft).unwrap().0, "waiting");

    // The network comes back, and with it the server: the message goes, once.
    *config.lock().unwrap() = reachable;
    reconnected.store(true, Ordering::Release);
    room.until("the message to be sent", |room| {
        delivery_of(room, &draft).is_some_and(|(state, _)| state == "sent")
    });
    assert_eq!(servers.smtp.sent().len(), 1);
    let shown = editor_of(&room, &draft);
    assert!(shown.contains("Sent"), "{shown}");
}

/// The mail connector process for `config`'s servers, as the system runs it for the app: on a
/// thread of its own here. What reaches it attaches an instance.
fn connector_for(config: AccountConfig) -> std::sync::mpsc::Sender<tilolang_mail::serve::Wakeup> {
    use tilolang_mail::serve::{Connector, Wakeup};
    let (woken, wakeups) = std::sync::mpsc::channel::<Wakeup>();
    let looping = woken.clone();
    std::thread::spawn(move || {
        let engine = mail::engine::live::LiveEngine::trusting(
            tilo_content::ContentStore::memory(),
            Arc::new(|| {}),
            accepting_client_config(),
        )
        .expect("the engine starts");
        let engine_woken = looping.clone();
        let (requests, events) = mail::engine::spawn_waking(
            Box::new(engine),
            Arc::new(move || {
                let _ = engine_woken.send(Wakeup::Engine);
            }),
        );
        let connector = Connector::new(
            Box::new(move || vec![config.clone()]),
            requests,
            events,
            Arc::default(),
        );
        connector.run(&wakeups, &looping);
    });
    woken
}

/// A room as an instance in a tilolang host keeps it: its origin reaches the connector over a
/// socket, holding no network and no credential of its own.
fn room_attached(connector: &std::sync::mpsc::Sender<tilolang_mail::serve::Wakeup>) -> Room {
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    connector
        .send(tilolang_mail::serve::Wakeup::Attach(theirs))
        .unwrap();
    let origin = tilolang_mail::remote_origin(ours, Arc::new(|| {})).expect("the origin attaches");
    room_of(origin)
}

#[test]
fn an_instance_without_network_syncs_and_sends_through_the_mail_connector() {
    let servers = servers();
    let connector = connector_for(account(&servers));
    let mut room = room_attached(&connector);
    synced(&mut room);
    hold_for(&mut room, 0);
    let plan = room.thread("Launch plan");
    let RtValue::Ref(draft) = room.press("reply", vec![RtValue::resource(&plan)]) else {
        panic!("a reply is a draft");
    };
    let draft = (*draft.target).clone();
    room.press("send", vec![RtValue::resource(&draft)]);
    room.until("the message to be sent", |room| {
        delivery_of(room, &draft).is_some_and(|(state, _)| state == "sent")
    });
    assert_eq!(servers.smtp.sent().len(), 1);

    // The window closes; mail arrives meanwhile; the next window has it.
    drop(room);
    servers.imap.append(
        "INBOX",
        &mail(
            "lease@lettings.example",
            "Mill Lane Lettings <office@lettings.example>",
            "Your lease renewal",
            "The new lease is attached.",
            None,
        ),
        &[],
    );
    let mut room = room_attached(&connector);
    room.until("the mail that came while it was closed", |room| {
        room.threads()
            .iter()
            .any(|t| room.text(t, "subject") == "Your lease renewal")
    });
    assert_eq!(servers.smtp.sent().len(), 1, "nothing was sent twice");
}

/// The key of the first node of `kind` in `node`, its whole tree searched.
fn first_kind(node: &tilolang_tilo::runtime::UiNode, kind: &str) -> Option<String> {
    if node.kind == kind {
        return Some(node.key.clone());
    }
    node.children
        .iter()
        .find_map(|child| first_kind(child, kind))
}

/// Every text the host carries in `node`, its whole tree walked: what its `value`, `title`,
/// `secondary` and `label` props say.
fn host_texts_in(node: &tilo_ui_host::UiNode, out: &mut Vec<String>) {
    for prop in ["value", "title", "secondary", "label"] {
        if let Some(tilo_ui_host::Value::Text(text)) = node.props.get(prop) {
            out.push(text.clone());
        }
    }
    for child in &node.children {
        host_texts_in(child, out);
    }
}

/// Every text `node` shows, its whole tree walked: what its `value`, `title`, `secondary` and
/// `label` props say.
fn texts_in(node: &tilolang_tilo::runtime::UiNode, out: &mut Vec<String>) {
    for prop in ["value", "title", "secondary", "label"] {
        if let Some(tilolang_tilo::runtime::Value::Text(text)) = node.props.get(prop) {
            out.push(text.clone());
        }
    }
    for child in &node.children {
        texts_in(child, out);
    }
}

/// What a conversation's window shows, opened as activating its row opens it: every text its
/// detail view rendered, its transcript's rows included (a virtual list sends its rows as rows).
fn conversation_of(room: &mut Room, thread: &ResourceRef) -> Vec<String> {
    let row = room
        .items()
        .into_iter()
        .find(|(_, item)| {
            item.item
                .as_ref()
                .is_some_and(|shown| shown.0 == thread.id_string())
        })
        .map(|(key, _)| key)
        .expect("the inbox lists the conversation");
    room.activate(&row);
    let detail = room
        .window
        .instance()
        .view(tilo_render_protocol::patch::Lane::Detail)
        .expect("the conversation's view runs");
    let list = first_kind(detail.borrow().tree(), "ui.VirtualList")
        .expect("the conversation has a transcript");
    // Render every row, as a window tall enough for them all does.
    let len = detail.borrow().list_len(&list).unwrap_or(0);
    for index in 0..len {
        detail
            .borrow_mut()
            .row(&list, index)
            .expect("a row renders");
    }
    let tree = detail.borrow().tree().clone();
    let mut out = Vec::new();
    texts_in(&tree, &mut out);
    room.settle();
    out
}

/// A second account's server, synced alongside the first: another address, its own inbox.
fn second_account() -> Servers {
    let rt = ui::rt::app();
    let imap = rt
        .block_on(FakeImap::start(Layout::Gmail, server_tls_config()))
        .expect("the second IMAP server starts");
    let smtp = rt
        .block_on(FakeSmtp::start(server_tls_config()))
        .expect("the second SMTP server starts");
    imap.append(
        "INBOX",
        &mail_to(
            "work@rivera.example",
            "holiday@team.example",
            "Team holiday",
            "The office is closed on the first.",
        ),
        &[],
    );
    Servers { imap, smtp }
}

// USER REQUIREMENT: every account shows in the sidebar and its mail is reachable from the one
// window, not only the first account's.
#[test]
fn every_account_shows_in_the_sidebar_and_opens_its_own_mail() {
    let servers = servers();
    let second = second_account();
    let mut room = room_over(
        Box::new(move || vec![account(&servers), work_account(&second)]),
        Arc::default(),
    );
    room.until("both accounts' mail", |room| {
        room.accounts().len() == 2
            && room
                .accounts()
                .iter()
                .all(|a| !room.refs(a, "threads").is_empty())
    });
    let mut texts = Vec::new();
    let root = room.window.root_tree().expect("the mail window runs");
    host_texts_in(&root, &mut texts);
    for want in ["Sam Rivera", "Sam at work"] {
        assert!(
            texts.iter().any(|text| text == want),
            "the sidebar names {want}: {texts:#?}"
        );
    }
    // The second account's own window shows its inbox, as its row in the sidebar opens it.
    let work = room
        .accounts()
        .into_iter()
        .find(|a| room.text(a, "address") == "work@rivera.example")
        .expect("the second account");
    let shown = conversation_titles(&mut room, &work);
    assert!(
        shown.iter().any(|text| text == "Inbox"),
        "the account's window shows its mailboxes: {shown:#?}"
    );
    assert!(
        room.refs(&work, "threads")
            .iter()
            .any(|t| room.text(t, "subject") == "Team holiday"),
        "the second account holds its own mail"
    );
}

/// The subjects a mail window over `of` lists, its root view's texts walked.
fn conversation_titles(room: &mut Room, of: &ResourceRef) -> Vec<String> {
    let view = room
        .host()
        .open_resource(of, None)
        .expect("the account's window opens");
    let mut texts = Vec::new();
    texts_in(view.tree(), &mut texts);
    texts
}

// USER REQUIREMENT: a conversation must read as a mail client's does — its subject, each
// message's sender and recipients, and what came with it — not as a run of bare bodies.
#[test]
fn a_conversation_shows_its_subject_senders_recipients_and_attachments() {
    let servers = servers();
    let mut room = room_with(account(&servers));
    synced(&mut room);
    let plan = room.thread("Launch plan");
    let shown = conversation_of(&mut room, &plan);
    assert!(
        shown.iter().any(|text| text.contains("Launch plan")),
        "the subject heads the conversation: {shown:#?}"
    );
    assert!(
        shown.iter().any(|text| text == "Grace Hopper"),
        "each message names who wrote it: {shown:#?}"
    );
    assert!(
        shown.iter().any(|text| text.starts_with("to ")),
        "each message names who it went to: {shown:#?}"
    );

    // A message that came with a file says so, by name.
    servers.imap.append(
        "INBOX",
        &mail(
            "lease@lettings.example",
            "Mill Lane Lettings <office@lettings.example>",
            "Your lease renewal",
            "The new lease is attached.",
            None,
        ),
        &[],
    );
    room.until("the message with the lease", |room| {
        room.threads()
            .iter()
            .any(|t| room.text(t, "subject") == "Your lease renewal")
    });
    let lease = room.thread("Your lease renewal");
    let shown = conversation_of(&mut room, &lease);
    assert!(
        shown.iter().any(|text| text.contains("lease")),
        "the conversation shows what it says: {shown:#?}"
    );
}

// USER REQUIREMENT: replying from the conversation itself, with what was typed, quoting what it
// answers — the answer daily mail is, without going through a separate draft window.
#[test]
fn a_reply_typed_in_the_conversation_goes_out_once_quoting_the_message_it_answers() {
    let servers = servers();
    let mut room = room_with(account(&servers));
    synced(&mut room);
    hold_for(&mut room, 0);
    let plan = room.thread("Launch plan");
    let thread = RtValue::resource(&plan);
    room.press("answer", vec![thread, RtValue::text("Thursday suits me.")]);
    room.until("the server to take the answer", |_| {
        servers.smtp.sent().len() == 1
    });
    let sent = String::from_utf8_lossy(&servers.smtp.sent()[0]).into_owned();
    assert!(sent.contains("Thursday suits me."), "{sent}");
    assert!(
        sent.contains("Ada Lovelace") && sent.contains("Agreed on the twelfth."),
        "it quotes the message it answers: {sent}"
    );
    assert!(
        sent.contains("Subject: Re: Launch plan"),
        "it answers under the subject: {sent}"
    );
    assert!(
        sent.contains("ada@analytical.example"),
        "it goes to whoever wrote the newest: {sent}"
    );
}

// USER REQUIREMENT: an empty answer is nothing to send; the action refuses rather than
// mailing a blank message.
#[test]
fn a_reply_with_nothing_said_is_refused() {
    let servers = servers();
    let mut room = room_with(account(&servers));
    synced(&mut room);
    let plan = room.thread("Launch plan");
    let thread = RtValue::resource(&plan);
    let refused = room.host().runtime().invoke_action(
        &Principal::new("ada"),
        ("answer", vec![thread, RtValue::text("")]),
    );
    assert!(refused.is_err(), "an empty answer is refused: {refused:?}");
    assert!(servers.smtp.sent().is_empty());
}
