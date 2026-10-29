//! Mail against a mail server: the package's window kept in step with a test IMAP server through
//! the mail origin and native Mail's engine, and sending through a test SMTP server from the
//! outbox. What people do goes through the package's actions, as a press does.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mail::fake_servers::{FakeImap, FakeSmtp, Layout, accepting_client_config, server_tls_config};
use mail::model::{AccountConfig, AuthMethod};
use tilolang_tilo::runtime::{Principal, ResourceRef, Value as RtValue};

use super::Room;
use crate::mail::connector::EngineConnector;
use crate::mail::origin::ImapOrigin;
use crate::window::Window;
use ui::framework::RunMode;

const APP: &str = "so.tilo.lang.mail";
const ME: &str = "sam@rivera.example";

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
    let connector = EngineConnector::new(Box::new(move || vec![config.clone()]), requests, events);
    let origin = std::rc::Rc::new(std::cell::RefCell::new(ImapOrigin::new(
        Box::new(connector),
        woken,
    )));
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
