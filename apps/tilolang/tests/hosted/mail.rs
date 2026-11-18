//! Mail against a mail server, as a session runs it: the package's instance in a tilolang host,
//! kept in step with a test IMAP server through the mail connector it reaches over a socket, and
//! sending through a test SMTP server from the outbox. What people do goes through the API, as
//! a block the person runs, the way an agent or the person's own tool runs one.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use ::mail::fake_servers::{
    FakeImap, FakeSmtp, Layout, accepting_client_config, server_tls_config,
};
use ::mail::model::{AccountConfig, AuthMethod};
use serde_json::{Value as Json, json};
use tilolang_mail::serve::{Connector, Wakeup};
use tilolang_tilo::runtime::store::Store as _;
use tilolang_tilo::runtime::{ResourceRef, UiNode, Value as RtValue};

use super::{Session, Window, WorkspaceEnd};
use crate::change_marks::action_id;

const APP: &str = "so.tilo.lang.mail";
const ME: &str = "sam@rivera.example";
const HOST: &str = "tl:host";

/// The longest a server may take to answer before the test says so.
const LIMIT: std::time::Duration = std::time::Duration::from_secs(120);

/// A message as a server holds it, to `to`.
fn mail_to(to: &str, from: &str, subject: &str, body: &str) -> String {
    format!(
        "From: {from}\nTo: {to}\nSubject: {subject}\nMessage-ID: <{subject}@{to}>\n\
Date: Tue, 30 Jun 2026 09:00:00 +0000\n\n{body}\n"
    )
}

/// A message to the person as a server holds it, answering `answers` when it names one.
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

/// The person's servers, their inbox holding a conversation of two and a message read already.
fn servers() -> Servers {
    servers_answering("Agreed on the twelfth.")
}

/// [`servers`], the second message of the conversation saying `reply`.
fn servers_answering(reply: &str) -> Servers {
    let servers = empty_servers();
    let imap = &servers.imap;
    imap.append("INBOX", &plan(), &[]);
    imap.append(
        "INBOX",
        &mail(
            "plan-2@analytical.example",
            "Ada Lovelace <ada@analytical.example>",
            "Re: Launch plan",
            reply,
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
    servers
}

fn plan() -> String {
    mail(
        "plan@hopper.example",
        "Grace Hopper <grace@hopper.example>",
        "Launch plan",
        "Here is the draft plan.",
        None,
    )
}

fn lease() -> String {
    mail(
        "lease@lettings.example",
        "Mill Lane Lettings <office@lettings.example>",
        "Your lease renewal",
        "The new lease is attached.",
        None,
    )
}

/// The two servers with nothing in the inbox yet: what a test puts there once the window stands,
/// as mail arrives at an open window.
fn empty_servers() -> Servers {
    let rt = ui::rt::app();
    let imap = rt
        .block_on(FakeImap::start(Layout::Gmail, server_tls_config()))
        .expect("the IMAP server starts");
    let smtp = rt
        .block_on(FakeSmtp::start(server_tls_config()))
        .expect("the SMTP server starts");
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

/// The config of the second account: another address on `servers`' own ports.
fn work_account(servers: &Servers) -> AccountConfig {
    AccountConfig {
        display_name: "Sam at work".into(),
        email: "work@rivera.example".into(),
        username: "work@rivera.example".into(),
        ..account(servers)
    }
}

/// The mail connector process for the accounts `source` names each time it is asked, as the
/// system runs it for the app: on a thread of its own here. What reaches it attaches an instance.
struct MailConnector {
    wakeups: Sender<Wakeup>,
    online: Arc<AtomicBool>,
}

impl MailConnector {
    fn new(source: impl Fn() -> Vec<AccountConfig> + Send + 'static) -> Self {
        let (wakeups, woken) = std::sync::mpsc::channel::<Wakeup>();
        let looping = wakeups.clone();
        let online = Arc::new(AtomicBool::new(false));
        let device = Arc::clone(&online);
        std::thread::spawn(move || {
            let engine_woken = looping.clone();
            let wake: ::mail::engine::Wake = Arc::new(move || {
                let _ = engine_woken.send(Wakeup::Engine);
            });
            let engine = ::mail::engine::live::LiveEngine::trusting(
                tilo_content::ContentStore::memory(),
                Arc::clone(&wake),
                accepting_client_config(),
            )
            .expect("the engine starts");
            let (requests, events) = ::mail::engine::spawn_waking(Box::new(engine), wake);
            Connector::new(Box::new(source), requests, events, device).run(&woken, &looping);
        });
        Self { wakeups, online }
    }

    fn of(config: AccountConfig) -> Self {
        Self::new(move || vec![config.clone()])
    }

    /// The device comes online again, as the network watcher says.
    fn come_online(&self) {
        self.online.store(true, Ordering::Release);
        self.wakeups.send(Wakeup::Engine).unwrap();
    }

    /// Ada at a Mail window whose instance a tilolang host runs, its origin reaching this
    /// connector over a socket as a session's host reaches it: holding no network and no
    /// credential of its own.
    fn window(&self) -> Session {
        self.attach(Session::opened(APP, "ada", Window::in_harness))
    }

    /// [`Self::window`], its connection reaching the workspace `end`.
    fn window_reaching(&self, end: &WorkspaceEnd) -> Session {
        self.attach(Session::reaching(APP, end, Window::in_harness))
    }

    /// `session`'s origin, reaching this connector.
    fn attach(&self, mut session: Session) -> Session {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        self.wakeups.send(Wakeup::Attach(theirs)).unwrap();
        let origin =
            tilolang_mail::remote_origin(ours, session.woken.waker()).expect("the origin attaches");
        session.instance_mut().add_origin(origin);
        session.settle();
        session
    }
}

/// Ada at a Mail window kept in step with `config`'s servers.
fn window_of(config: AccountConfig) -> (MailConnector, Session) {
    let connector = MailConnector::of(config);
    let session = connector.window();
    (connector, session)
}

impl Session {
    /// The window's frames until `done` holds of it, as the servers answer.
    fn until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        self.wait_for(what, |session| done(session).then_some(()));
    }

    /// The window's frames until `probe` yields what it looks for, and that.
    fn wait_for<T>(&mut self, what: &str, mut probe: impl FnMut(&mut Self) -> Option<T>) -> T {
        let began = std::time::Instant::now();
        loop {
            self.settle();
            if let Some(found) = probe(self) {
                return found;
            }
            assert!(began.elapsed() < LIMIT, "{what} did not happen");
            // Nothing is owed: what the test waits for is a server's answer, which wakes the host.
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
    }

    fn value_of(&self, at: &ResourceRef, field: &str) -> RtValue {
        self.instance_host()
            .runtime()
            .read(at, field)
            .expect("the field reads")
    }

    fn refs(&self, at: &ResourceRef, field: &str) -> Vec<ResourceRef> {
        match self.value_of(at, field) {
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
        match self.value_of(at, field) {
            RtValue::Text(text) => text,
            other => panic!("{field} is not a text: {other:?}"),
        }
    }

    fn flag(&self, at: &ResourceRef, field: &str) -> bool {
        matches!(
            self.value_of(at, field),
            RtValue::Bool(true) | RtValue::Int(1)
        )
    }

    fn mail_root(&self) -> ResourceRef {
        self.instance_host().root_of(APP).expect("the root")
    }

    fn accounts(&self) -> Vec<ResourceRef> {
        self.refs(&self.mail_root(), "accounts")
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

    /// The id of the type `name` of the package: what a change that adds one names.
    fn type_id(&self, name: &str) -> String {
        self.instance_host()
            .package(APP)
            .expect("the mail package")
            .meta
            .type_named(name)
            .unwrap_or_else(|| panic!("Mail keeps {name}"))
            .id
            .clone()
    }

    /// Invoke the Mail action `name` on `on` at the API, as the person at the window presses
    /// it: `arguments` names the action's other parameters. What it answered, or why it was
    /// refused.
    fn try_call(
        &mut self,
        name: &str,
        on: &ResourceRef,
        arguments: &Json,
    ) -> Result<ui::tilo_actions::ActionOutput, String> {
        let bundle = super::package(super::package_of(APP));
        if !bundle
            .meta
            .actions
            .iter()
            .any(|declared| declared.name == name)
        {
            return Err(format!("Mail declares no action {name}"));
        }
        let resource = tilolang_tilo::publish::resource_id(APP, &on.id_string());
        let arguments = arguments.as_object().cloned().unwrap_or_default();
        let asked_before = self.asked(on);
        // The API runs an action by the name the app publishes it under.
        let answered = self.invoke(None, None, &resource, name, &arguments)?;
        self.settle();
        // What the action asks first — a send its code makes — is allowed, as the person allows
        // what the app's sheet asks of them, through the API.
        for asked in self.asked(on) {
            if asked_before.contains(&asked) {
                continue;
            }
            let decide = action_id(self, HOST, "Decide suggestion");
            let decided = self.invoke(
                None,
                None,
                HOST,
                &decide,
                &json!({ "target": asked, "decision": "approve" })
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            )?;
            let state = decided.value.as_ref().map(|value| value["state"].clone());
            if state != Some(json!("done")) {
                return Err(format!("{name}: the ask was not carried out: {decided:?}"));
            }
            self.settle();
        }
        Ok(answered)
    }

    /// What waits for the person's answer on `on`: the suggestions an action asked, by id.
    fn asked(&self, on: &ResourceRef) -> Vec<String> {
        self.instance_host()
            .runtime()
            .proposals_of(on)
            .into_iter()
            .map(|(asked, _)| asked.id_string())
            .collect()
    }

    /// [`Self::try_call`] of an action that runs.
    fn call(
        &mut self,
        name: &str,
        on: &ResourceRef,
        arguments: &Json,
    ) -> ui::tilo_actions::ActionOutput {
        self.try_call(name, on, arguments)
            .unwrap_or_else(|error| panic!("{name}: {error}"))
    }

    /// [`Self::call`] of an action that makes a draft: the draft.
    fn made(&mut self, name: &str, on: &ResourceRef, arguments: &Json) -> ResourceRef {
        let answered = self.call(name, on, arguments);
        let prefix = format!("tl:{APP}:");
        let made = answered
            .resources
            .first()
            .and_then(|made| made.strip_prefix(&prefix))
            .unwrap_or_else(|| panic!("{name} answers what it made: {answered:?}"));
        ResourceRef::parse_id(made, self.type_id("Draft"))
            .unwrap_or_else(|| panic!("{name} answers what it made: {answered:?}"))
    }

    /// Hold a sent message for `secs` seconds: the person's setting.
    fn hold_for(&mut self, secs: i64) {
        let root = self.mail_root();
        self.call("set_hold", &root, &json!({ "seconds": secs }));
    }

    /// A reply to the conversation `subject`, sent: the draft it made.
    fn sent_reply(&mut self, subject: &str) -> ResourceRef {
        let plan = self.thread(subject);
        let draft = self.made("reply", &plan, &json!({}));
        self.call("send", &draft, &json!({}));
        draft
    }

    /// How the draft `draft` stands on its way out, as `delivery(d)` reads it.
    fn delivery_of(&self, draft: &ResourceRef) -> Option<(String, String)> {
        self.instance_host()
            .store()
            .outbox_states()
            .into_iter()
            .find(|(doc, _)| *doc == draft.doc)
            .map(|(_, state)| (state.state, state.reason))
    }

    /// Every text the window of `resource` says, as `open(r)` opens it: the words its marks carry
    /// on hover among them.
    fn view_of(&self, resource: &ResourceRef) -> Vec<String> {
        let view = self
            .instance_host()
            .open_resource(resource, None)
            .expect("its window opens");
        let mut texts = Vec::new();
        texts_in(view.tree(), &mut texts);
        texts
    }

    /// How many conversations the account's window lists in the sidebar's place `place`: the
    /// place is chosen first, as a person clicks it, because a window opens on the place it was
    /// left at.
    fn place_count(&self, account: &ResourceRef, place: &str) -> Option<i64> {
        let mut view = self
            .instance_host()
            .open_resource(account, None)
            .expect("the account's window opens");
        let item = find_kind(view.tree(), "ui.NavItem", "label", place)
            .expect("the sidebar offers the place")
            .key
            .clone();
        view.fire(&item, "select").expect("the place opens");
        view.poll_patches().expect("the window shows the place");
        count_in(view.tree(), "ui.NavShell")
    }

    fn inbox_count(&self, account: &ResourceRef) -> Option<i64> {
        self.place_count(account, "Inbox")
    }

    /// Open the conversation `thread` as a person does, choosing its row in the window's list,
    /// and what each message of it shows in the window, row by row, after the texts above them.
    fn open_conversation(&mut self, thread: &ResourceRef) -> (Vec<String>, Vec<Vec<String>>) {
        // A row names a conversation by who wrote in it, its subject under that.
        let subject = self.text(thread, "senders");
        let began = std::time::Instant::now();
        let (x, y) = self.wait_for("the list to show the conversation", |session| {
            let mut found = Vec::new();
            containing(session.window.root(), &subject, &mut found);
            if let Some(rect) = found.last() {
                return Some((rect.center().x, rect.center().y));
            }
            if began.elapsed() > LIMIT / 4 {
                let mut names = Vec::new();
                accessible_names(session.window.root(), &mut names);
                panic!("the list shows no row of {subject:?}: {names:#?}");
            }
            None
        });
        self.window.click_at(x, y);
        self.settle();
        let view = self.hosted().view().expect("the window is connected");
        let mut page = Vec::new();
        super::texts(
            &view
                .detail_tree()
                .expect("the conversation opens beside the list"),
            &mut page,
        );
        let mut rows = Vec::new();
        for list in view.lists() {
            if !list.starts_with(tilo_app_window::shell::DETAIL) {
                continue;
            }
            let shown = view.rows_of(&list).expect("the list's rows");
            let shown = shown.borrow();
            for key in shown.rendered() {
                let mut texts = Vec::new();
                super::texts(shown.row(&key).expect("a rendered row"), &mut texts);
                rows.push(texts);
            }
        }
        (page, rows)
    }

    /// What the conversation `thread` shows in the window, opened: every text of it.
    fn conversation(&mut self, thread: &ResourceRef) -> Vec<String> {
        let (mut page, rows) = self.open_conversation(thread);
        page.extend(rows.into_iter().flatten());
        page
    }
}

/// The rects of the laid out widgets whose accessible name says `text`.
fn containing(widget: &dyn ui::widget::Widget, text: &str, out: &mut Vec<ui::kurbo::Rect>) {
    if widget.accessibility().is_some_and(|access| {
        access
            .name
            .as_deref()
            .is_some_and(|name| name.contains(text))
    }) && widget.get_rect().width() > 0.0
    {
        out.push(widget.get_rect());
    }
    for child in widget.children() {
        containing(child, text, out);
    }
}

fn accessible_names(widget: &dyn ui::widget::Widget, out: &mut Vec<String>) {
    if let Some(name) = widget.accessibility().and_then(|access| access.name) {
        out.push(name);
    }
    for child in widget.children() {
        accessible_names(child, out);
    }
}

/// Frames until the account's server's mail is in the window as a person sees it: its
/// conversations, filed in the inbox, with the messages the sync read. A sync makes a
/// conversation before it fetches its messages and files it, so the conversations in
/// the account are not the sync's end.
fn synced(session: &mut Session) {
    session.until("the inbox to sync", |session| {
        let threads = session.threads();
        threads.len() == 2
            && threads.iter().all(|thread| {
                matches!(session.value_of(thread, "boxes"), RtValue::List(boxes)
                    if boxes.contains(&RtValue::text("inbox")))
            })
    });
}

/// The key of the first node of `kind` whose prop `name` is `value`.
fn find_kind<'a>(node: &'a UiNode, kind: &str, name: &str, value: &str) -> Option<&'a UiNode> {
    if node.kind == kind && matches!(node.props.get(name), Some(RtValue::Text(t)) if t == value) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_kind(child, kind, name, value))
}

/// The `overview_count` prop of the first node of `kind`.
fn count_in(node: &UiNode, kind: &str) -> Option<i64> {
    if node.kind == kind
        && let Some(RtValue::Int(n)) = node.props.get("overview_count")
    {
        return Some(*n);
    }
    node.children.iter().find_map(|c| count_in(c, kind))
}

/// Every text `node` says, its whole tree walked: what its `value`, `title`, `secondary` and
/// `label` props show, what its `description` says — the words a mark carries on hover — and
/// what a notice says under its title.
fn texts_in(node: &UiNode, out: &mut Vec<String>) {
    for prop in [
        "value",
        "title",
        "secondary",
        "label",
        "description",
        "message",
    ] {
        if let Some(RtValue::Text(text)) = node.props.get(prop) {
            out.push(text.clone());
        }
    }
    for child in &node.children {
        texts_in(child, out);
    }
}

/// The wall clock at `hour:minute:second` on `day` of 2026, in the zone this process reads as
/// the person's.
fn local(month: i8, day: i8, (hour, minute, second): (i8, i8, i8)) -> ui::time::SystemTime {
    let at = ui::clock::instant_of(jiff::civil::date(2026, month, day).at(hour, minute, second, 0))
        .expect("the time exists where the person is");
    let ms = u64::try_from(at.as_millisecond()).expect("after 1970");
    ui::time::UNIX_EPOCH
        .checked_add(ui::time::Duration::from_millis(ms))
        .expect("the clock reaches it")
}

/// The wall clock at `hour:minute:second` on `day` of 2026, in UTC: what a document names, such as
/// the moment a conversation is snoozed until.
fn utc(month: i8, day: i8, (hour, minute, second): (i8, i8, i8)) -> ui::time::SystemTime {
    let at = jiff::civil::date(2026, month, day)
        .at(hour, minute, second, 0)
        .to_zoned(jiff::tz::TimeZone::UTC)
        .expect("the time exists")
        .timestamp();
    let ms = u64::try_from(at.as_millisecond()).expect("after 1970");
    ui::time::UNIX_EPOCH
        .checked_add(ui::time::Duration::from_millis(ms))
        .expect("the clock reaches it")
}

/// A Mail window pinned to a fixed wall clock, synced, as a test reads a snooze's moment.
fn window_at(at: ui::time::SystemTime, servers: &Servers) -> (MailConnector, Session) {
    ui::time::testing::with_wall(at, || {
        let (connector, mut session) = window_of(account(servers));
        synced(&mut session);
        (connector, session)
    })
}

#[test]
fn an_account_syncs_into_mailboxes_and_conversations() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    let account = session.accounts().remove(0);
    assert_eq!(session.text(&account, "address"), ME);
    assert_eq!(session.text(&account, "name"), "Sam Rivera");
    assert!(
        session.text(&account, "fetched") != "1970-01-01T00:00:00Z",
        "the account says when its mail was read from the server"
    );
    let boxes: Vec<String> = session
        .refs(&account, "mailboxes")
        .iter()
        .map(|b| session.text(b, "key"))
        .collect();
    assert_eq!(
        boxes,
        ["inbox", "archive", "sent", "drafts", "junk", "trash"]
    );

    // The reply joins the conversation it answers; the other stands alone.
    let plan = session.thread("Launch plan");
    let messages = session.refs(&plan, "messages");
    assert_eq!(messages.len(), 2);
    assert_eq!(session.text(&messages[0], "from_name"), "Grace Hopper");
    assert_eq!(
        session.text(&messages[1], "body").trim(),
        "Agreed on the twelfth."
    );
    assert!(
        session.flag(&plan, "unread"),
        "an unread message makes it unread"
    );
    assert_eq!(session.text(&plan, "senders"), "Ada Lovelace, Grace Hopper");
    let dinner = session.thread("Dinner on Friday?");
    assert!(
        !session.flag(&dinner, "unread"),
        "it was read on the server"
    );

    // Syncing again finds nothing new and makes nothing twice.
    session.until("the inbox counts", |session| {
        let inbox = session.refs(&session.accounts()[0], "mailboxes")[0].clone();
        matches!(session.value_of(&inbox, "unread"), RtValue::Int(2))
    });
    assert_eq!(session.threads().len(), 2);
}

#[test]
fn reading_and_archiving_reach_the_server_and_come_back() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    let plan = session.thread("Launch plan");

    session.call("mark_read", &plan, &json!({}));
    assert!(!session.flag(&plan, "unread"), "it reads as read at once");
    session.until("the server to hold it read", |_| {
        servers
            .imap
            .messages("INBOX")
            .iter()
            .filter(|m| m.flags.iter().any(|f| f == "\\Seen"))
            .count()
            == 3
    });

    session.call("archive", &plan, &json!({}));
    assert!(
        !matches!(session.value_of(&plan, "boxes"), RtValue::List(ref b) if b.contains(&RtValue::text("inbox"))),
        "it leaves the inbox at once"
    );
    session.until("the server to archive it", |_| {
        servers.imap.messages("INBOX").len() == 1
            && servers.imap.messages("[Gmail]/All Mail").len() == 2
    });

    // Taken back to the inbox, the server moves it back.
    session.call("to_inbox", &plan, &json!({}));
    session.until("the server to hold it in the inbox again", |_| {
        servers.imap.messages("INBOX").len() == 3
    });
    assert_eq!(session.threads().len(), 2, "moving makes nothing twice");
}

#[test]
fn opening_a_conversation_reads_it_here_and_on_the_server() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    let plan = session.thread("Launch plan");
    assert!(session.flag(&plan, "unread"));
    session.open_conversation(&plan);
    assert!(
        !session.flag(&plan, "unread"),
        "it reads as read once opened"
    );
    session.until("the server to hold it read", |_| {
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
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    session.hold_for(0);
    let plan = session.thread("Launch plan");
    let draft = session.made("reply", &plan, &json!({}));
    assert_eq!(session.text(&draft, "to"), "ada@analytical.example");
    assert_eq!(session.text(&draft, "subject"), "Re: Launch plan");
    assert_eq!(
        session.text(&draft, "in_reply_to"),
        "plan-2@analytical.example"
    );

    session.call("send", &draft, &json!({}));
    session.until("the server to take the reply", |_| {
        servers.smtp.sent().len() == 1
    });
    session.until("the outbox to settle", |session| {
        session.instance_host().store().outbox_pending_count() == 0
    });
    let sent = String::from_utf8_lossy(&servers.smtp.sent()[0]).into_owned();
    assert!(sent.contains("Subject: Re: Launch plan"), "{sent}");
    assert!(
        sent.contains("In-Reply-To: <plan-2@analytical.example>"),
        "{sent}"
    );
    assert!(session.flag(&draft, "sent"));
}

#[test]
fn an_unreachable_server_is_said_on_the_account() {
    let servers = servers();
    let mut config = account(&servers);
    // Nothing listens there.
    config.imap_port = 1;
    let (_connector, mut session) = window_of(config);
    session.until("the account to say why", |session| {
        session
            .accounts()
            .first()
            .is_some_and(|a| !session.text(a, "trouble").is_empty())
    });
    let account = session.accounts().remove(0);
    assert!(!session.flag(&account, "online"));
    assert!(session.threads().is_empty());
}

// USER REQUIREMENT: a person whose server cannot be reached sees why in their mail window, and
// the window says nothing more once the server answers.
#[test]
fn a_troubled_account_says_so_in_its_window_and_clears_when_the_server_answers() {
    let servers = servers();
    let reachable = account(&servers);
    // Nothing listens at the account's server yet.
    let config = Arc::new(Mutex::new(AccountConfig {
        imap_port: 1,
        ..reachable.clone()
    }));
    let accounts = Arc::clone(&config);
    let connector = MailConnector::new(move || vec![accounts.lock().unwrap().clone()]);
    let mut session = connector.window();
    session.until("the account to be made", |session| {
        !session.accounts().is_empty()
    });
    let account = session.accounts().remove(0);
    // The window says so where the person reads their mail: over the list, and on the mark
    // beside the account's name, which says why.
    let said = session.wait_for(
        "the window to say the server cannot be reached",
        |session| {
            let said = session.view_of(&account);
            said.iter()
                .any(|text| text.contains("Can't reach"))
                .then_some(said)
        },
    );
    let trouble = session.text(&account, "trouble");
    assert!(
        said.iter()
            .any(|text| text == "Can't reach sam@rivera.example"),
        "the banner names the account the person cannot reach, and says nothing the line under \
         it says again: {said:#?}"
    );
    assert!(
        said.iter()
            .any(|text| text.to_lowercase().contains(&trouble.to_lowercase())),
        "the mark and the banner say why, in the server's own words: {said:#?}"
    );
    assert!(
        said.iter().any(|text| text == "Check now"),
        "the window offers to ask the server again: {said:#?}"
    );
    // The window the person has open says it over the list too.
    let shown = session.shown();
    assert!(
        shown.iter().any(|text| text.contains("Can't reach")),
        "the open window says it over the list: {shown:#?}"
    );
    // Nothing asks the server again on its own: the window keeps saying it.
    for _ in 0..20 {
        session.settle();
    }
    assert!(
        session
            .view_of(&account)
            .iter()
            .any(|text| text.contains("Can't reach")),
        "the window still says it"
    );

    // The server answers, and the person asks for their mail again: it comes in, and the
    // window says nothing more of it.
    *config.lock().unwrap() = reachable;
    session.call("check_now", &account, &json!({}));
    synced(&mut session);
    session.until("the account to say nothing more", |session| {
        session.text(&account, "trouble").is_empty() && session.flag(&account, "online")
    });
    let said = session.wait_for("the window to say nothing more", |session| {
        let said = session.view_of(&account);
        (!said.iter().any(|text| text.contains("Can't reach"))).then_some(said)
    });
    assert!(
        !said.iter().any(|text| text.contains(&trouble)),
        "the words the mark carried are gone with it: {said:#?}"
    );
}

#[test]
fn a_sent_message_waits_for_the_persons_hold_before_it_goes() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    session.hold_for(600);
    let draft = session.sent_reply("Launch plan");
    session.settle();
    // It is held: nothing went, and it says how it stands.
    assert!(session.flag(&draft, "sent"));
    assert!(
        servers.smtp.sent().is_empty(),
        "a held message does not go yet"
    );
    assert_eq!(session.delivery_of(&draft).unwrap().0, "held");
    let shown = session.view_of(&draft);
    assert!(
        shown.iter().any(|text| text == "Sending in a moment"),
        "{shown:#?}"
    );
}

#[test]
fn a_held_message_is_taken_back_and_never_goes() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    session.hold_for(600);
    let draft = session.sent_reply("Launch plan");
    assert_eq!(session.delivery_of(&draft).unwrap().0, "held");

    // Its author takes it back: it never leaves, and it is a draft again.
    session.call("undo_send", &draft, &json!({}));
    assert!(!session.flag(&draft, "sent"), "it is a draft again");
    assert!(
        session.flag(&draft, "recalled"),
        "and it says it was taken back"
    );
    assert_eq!(session.delivery_of(&draft).unwrap().0, "withdrawn");
    for _ in 0..20 {
        session.settle();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(servers.smtp.sent().is_empty(), "it never went");
}

#[test]
fn a_message_that_went_cannot_be_taken_back() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    // No hold: it goes at once.
    session.hold_for(0);
    let draft = session.sent_reply("Launch plan");
    session.until("the server to take the message", |_| {
        servers.smtp.sent().len() == 1
    });
    session.until("the outbox to settle", |session| {
        session.instance_host().store().outbox_pending_count() == 0
    });
    let refused = session.try_call("undo_send", &draft, &json!({}));
    assert!(
        refused.is_err(),
        "a message that went is not taken back: {refused:?}"
    );
    assert_eq!(servers.smtp.sent().len(), 1);
}

#[test]
fn the_window_wakes_when_a_held_message_is_due() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    session.hold_for(600);
    let draft = session.sent_reply("Launch plan");
    session.settle();
    // The window waits for something that is not nothing: the moment the message is due, or
    // the minute the inbox's snooze filter next reads differently.
    let wait = session
        .instance()
        .instance()
        .wakes_in()
        .expect("a held message sets the window's next wake");
    assert!(
        wait <= std::time::Duration::from_secs(610),
        "it waits no longer than the hold: {wait:?}"
    );
    let held = session
        .instance_host()
        .runtime()
        .held_for()
        .expect("the message is held");
    assert!(
        held > std::time::Duration::from_secs(500),
        "the message is held for the person's hold: {held:?}"
    );
    assert_eq!(session.delivery_of(&draft).unwrap().0, "held");
}

// USER REQUIREMENT: a reply's quotation of the message it answers folds in the conversation, as
// Gmail and Apple Mail fold it, so a thread reads as what each person said.
#[test]
fn a_replys_quotation_folds_in_the_conversation() {
    // A message of the conversation that answers another, as its sender's client wrote it: what
    // it says, then the message it answers quoted under it.
    let servers = servers_answering(
        "The fifth works for the beta.\n\nOn 2026-06-30, Grace Hopper wrote:\n> Here is the draft \
         plan.",
    );
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    let plan = session.thread("Launch plan");
    let (_, rows) = session.open_conversation(&plan);
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

#[test]
fn a_message_waits_to_go_online_and_goes_when_the_network_is_back() {
    let servers = servers();
    let reachable = account(&servers);
    // The SMTP server cannot be reached yet: nothing listens there.
    let config = Arc::new(Mutex::new(AccountConfig {
        smtp_port: 1,
        ..reachable.clone()
    }));
    let accounts = Arc::clone(&config);
    let connector = MailConnector::new(move || vec![accounts.lock().unwrap().clone()]);
    let mut session = connector.window();
    synced(&mut session);
    session.hold_for(0);
    let draft = session.sent_reply("Launch plan");
    session.until("the message to wait to go online", |session| {
        session
            .delivery_of(&draft)
            .is_some_and(|(state, reason)| state == "waiting" && !reason.is_empty())
    });
    // The draft says so where it is written.
    let shown = session.view_of(&draft);
    assert!(
        shown
            .iter()
            .any(|text| text.starts_with("Waiting to go online")),
        "{shown:#?}"
    );
    // It is not tried again on its own.
    for _ in 0..20 {
        session.settle();
    }
    assert!(servers.smtp.sent().is_empty());
    assert_eq!(session.delivery_of(&draft).unwrap().0, "waiting");

    // The network comes back, and with it the server: the message goes, once.
    *config.lock().unwrap() = reachable;
    connector.come_online();
    session.until("the message to be sent", |session| {
        session
            .delivery_of(&draft)
            .is_some_and(|(state, _)| state == "sent")
    });
    assert_eq!(servers.smtp.sent().len(), 1);
    let shown = session.view_of(&draft);
    assert!(shown.iter().any(|text| text == "Sent"), "{shown:#?}");
}

#[test]
fn a_window_closed_and_opened_again_has_the_mail_that_came_meanwhile_and_sends_nothing_twice() {
    let servers = servers();
    let connector = MailConnector::of(account(&servers));
    let mut session = connector.window();
    synced(&mut session);
    session.hold_for(0);
    let draft = session.sent_reply("Launch plan");
    session.until("the message to be sent", |session| {
        session
            .delivery_of(&draft)
            .is_some_and(|(state, _)| state == "sent")
    });
    assert_eq!(servers.smtp.sent().len(), 1);

    // The window closes; mail arrives meanwhile; the next window has it.
    session.close();
    servers.imap.append("INBOX", &lease(), &[]);
    let mut session = connector.window();
    session.until("the mail that came while it was closed", |session| {
        session
            .threads()
            .iter()
            .any(|t| session.text(t, "subject") == "Your lease renewal")
    });
    assert_eq!(servers.smtp.sent().len(), 1, "nothing was sent twice");
}

/// A second account's server, synced alongside the first: another address, its own inbox.
fn second_account() -> Servers {
    let servers = empty_servers();
    servers.imap.append(
        "INBOX",
        &mail_to(
            "work@rivera.example",
            "holiday@team.example",
            "Team holiday",
            "The office is closed on the first.",
        ),
        &[],
    );
    servers
}

// USER REQUIREMENT: every account shows in the sidebar and its mail is reachable from the one
// window, not only the first account's.
#[test]
fn every_account_shows_in_the_sidebar_and_opens_its_own_mail() {
    let servers = servers();
    let second = second_account();
    let connector = MailConnector::new(move || vec![account(&servers), work_account(&second)]);
    let mut session = connector.window();
    session.until("both accounts' mail", |session| {
        session.accounts().len() == 2
            && session
                .accounts()
                .iter()
                .all(|a| !session.refs(a, "threads").is_empty())
    });
    let texts = session.shown();
    for want in ["Sam Rivera", "Sam at work"] {
        assert!(
            texts.iter().any(|text| text == want),
            "the sidebar names {want}: {texts:#?}"
        );
    }
    // The second account's own window shows its inbox, as its row in the sidebar opens it.
    let work = session
        .accounts()
        .into_iter()
        .find(|a| session.text(a, "address") == "work@rivera.example")
        .expect("the second account");
    let shown = session.view_of(&work);
    assert!(
        shown.iter().any(|text| text == "Inbox"),
        "the account's window shows its mailboxes: {shown:#?}"
    );
    assert!(
        session
            .refs(&work, "threads")
            .iter()
            .any(|t| session.text(t, "subject") == "Team holiday"),
        "the second account holds its own mail"
    );
}

// USER REQUIREMENT: snoozing a conversation takes it out of the inbox until a moment, and it
// comes back then with no window open.
#[test]
fn snoozing_takes_a_conversation_out_of_the_inbox_until_its_moment() {
    use ui::time::testing::with_wall;
    let _settled = ui::motion::testing::settled();
    let servers = servers();
    let (_connector, mut session) = window_at(local(6, 30, (10, 0, 0)), &servers);
    let account = session.accounts().remove(0);
    let before = with_wall(local(6, 30, (10, 0, 0)), || session.inbox_count(&account))
        .expect("the inbox counts its mail");
    assert_eq!(
        before, 2,
        "the account's two conversations are in the inbox"
    );
    let plan = session.thread("Launch plan");
    let snoozed = with_wall(local(6, 30, (10, 0, 0)), || {
        session.call("snooze", &plan, &json!({ "until": "2026-06-30T10:30:00Z" }));
        session.inbox_count(&account)
    });
    assert_eq!(
        snoozed,
        Some(before - 1),
        "a snoozed conversation leaves the inbox"
    );
    // The inbox reads the clock (the snooze), so the window waits for the moment the conversation
    // comes back, and for no minute in between: it comes back on its own, with no window open.
    let wait = with_wall(local(6, 30, (10, 0, 0)), || {
        session.instance().instance().wakes_in()
    })
    .expect("a snoozed conversation sets the window's next wake");
    let moment = utc(6, 30, (10, 30, 0))
        .duration_since(local(6, 30, (10, 0, 0)))
        .expect("after the moment the window reads the time at");
    assert_eq!(
        wait, moment,
        "the window waits for the snooze's moment: {wait:?}"
    );
    // The moment passes with no window open: the conversation is in the inbox again.
    let shown = with_wall(local(6, 30, (10, 31, 0)), || {
        session.settle();
        session.inbox_count(&account)
    });
    assert_eq!(
        shown,
        Some(before),
        "back in the inbox once its moment passed"
    );
    assert_eq!(
        with_wall(local(6, 30, (10, 31, 0)), || {
            session.call("unsnooze", &plan, &json!({}));
            session.inbox_count(&account)
        }),
        Some(before),
        "brought back, it is in the inbox again"
    );
}

// USER REQUIREMENT: labels are the person's own; a label is given and taken away, and each
// label is a place of its own that narrows the list to the conversations that carry it.
#[test]
fn a_label_is_given_taken_away_and_lists_its_own_conversations() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    let plan = session.thread("Launch plan");
    session.call("label", &plan, &json!({ "name": "Work", "on": true }));
    let labels = session.value_of(&plan, "labels");
    assert!(
        matches!(&labels, RtValue::List(items) if items.iter().any(|l| l == &RtValue::text("Work"))),
        "the label is on the conversation: {labels:?}"
    );
    // The label is the person's own: the root holds it, so it is a place from then on.
    let named: Vec<String> = session
        .refs(&session.mail_root(), "labels")
        .iter()
        .map(|l| session.text(l, "name"))
        .collect();
    assert_eq!(named, ["Work"], "the person's labels hold it once");

    // Its place narrows the window to the conversation that carries it.
    let account = session.accounts().remove(0);
    assert_eq!(
        session.place_count(&account, "Work"),
        Some(1),
        "only the conversation with the label is listed"
    );

    // Taken away, the conversation no longer carries it.
    session.call("label", &plan, &json!({ "name": "Work", "on": false }));
    let labels = session.value_of(&plan, "labels");
    assert!(
        !matches!(&labels, RtValue::List(items) if items.iter().any(|l| l == &RtValue::text("Work"))),
        "the label is gone: {labels:?}"
    );
}

// USER REQUIREMENT: a muted conversation stays out of the inbox while new messages keep coming,
// and is found under Muted until it is brought back, as Gmail mutes a busy thread.
#[test]
fn muting_keeps_a_conversation_out_of_the_inbox_until_it_is_brought_back() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    let account = session.accounts().remove(0);
    let before = session
        .inbox_count(&account)
        .expect("the inbox counts its mail");
    let plan = session.thread("Launch plan");
    session.call("mute", &plan, &json!({}));
    assert!(session.flag(&plan, "muted"));
    assert_eq!(
        session.inbox_count(&account),
        Some(before - 1),
        "a muted conversation leaves the inbox"
    );
    // New mail in it does not bring it back: with it unread again, as a new message leaves it,
    // it is still out of the inbox.
    session.call("mark_read", &plan, &json!({ "unread": true }));
    assert!(session.flag(&plan, "unread"));
    assert_eq!(
        session.inbox_count(&account),
        Some(before - 1),
        "the muted conversation stays out of the inbox while it is answered"
    );
    // Its place of its own lists it, so it is not lost.
    assert_eq!(
        session.place_count(&account, "Muted"),
        Some(1),
        "Muted lists the muted conversation"
    );
    // Bringing it back unmutes it: it is in the inbox again.
    session.call("to_inbox", &plan, &json!({}));
    assert!(!session.flag(&plan, "muted"));
    assert_eq!(
        session.inbox_count(&account),
        Some(before),
        "brought back, it is in the inbox again"
    );
}

#[test]
fn marking_all_read_reads_every_conversation_and_reaches_the_server() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    let account = session.accounts().remove(0);
    session.call("mark_all_read", &account, &json!({}));
    for t in session.refs(&account, "threads") {
        assert!(!session.flag(&t, "unread"), "every conversation is read");
    }
    session.until("the server to hold them read", |_| {
        servers
            .imap
            .messages("INBOX")
            .iter()
            .all(|m| m.flags.iter().any(|f| f == "\\Seen"))
    });
}

// USER REQUIREMENT: a message can go at a moment instead of now, as Gmail's Schedule send does,
// and until it goes it can be cancelled and is a draft again.
#[test]
fn a_message_sent_later_waits_for_its_moment_and_is_cancelled_before_it() {
    use ui::time::testing::with_wall;
    let _settled = ui::motion::testing::settled();
    let servers = servers();
    let (_connector, mut session) = window_at(local(6, 30, (10, 0, 0)), &servers);
    // Everything up to its cancellation happens at the pinned clock, so the moment it goes is
    // still ahead: the frame settles without carrying it out.
    let (draft, wait) = with_wall(local(6, 30, (10, 0, 0)), || {
        session.hold_for(0);
        let plan = session.thread("Launch plan");
        let draft = session.made("reply", &plan, &json!({}));
        session.call(
            "send_later",
            &draft,
            &json!({ "at": "2026-06-30T18:00:00Z" }),
        );
        assert!(
            session.flag(&draft, "sent"),
            "it is on its way, at a moment"
        );
        assert_eq!(
            session.text(&draft, "send_at"),
            "2026-06-30T18:00:00Z",
            "the moment is recorded on the message"
        );
        assert_eq!(session.delivery_of(&draft).unwrap().0, "held");
        assert!(
            servers.smtp.sent().is_empty(),
            "a message sent later does not go yet"
        );
        // The window waits for its moment, and no longer.
        let wait = session
            .instance()
            .instance()
            .wakes_in()
            .expect("a message sent later sets the window's next wake");
        (draft, wait)
    });
    assert!(
        wait <= std::time::Duration::from_secs(8 * 3600 + 60),
        "it waits until the moment it goes: {wait:?}"
    );
    // Cancelled before it goes: it is a draft again, and it never leaves.
    with_wall(local(6, 30, (10, 1, 0)), || {
        session.call("cancel_later", &draft, &json!({}));
    });
    assert!(!session.flag(&draft, "sent"), "it is a draft again");
    assert!(session.flag(&draft, "recalled"));
    assert_eq!(session.delivery_of(&draft).unwrap().0, "withdrawn");
    for _ in 0..20 {
        session.settle();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(servers.smtp.sent().is_empty(), "it never went");
}

// USER REQUIREMENT: a conversation must read as a mail client's does — its subject, each
// message's sender and recipients, and what came with it — not as a run of bare bodies.
#[test]
fn a_conversation_shows_its_subject_senders_recipients_and_attachments() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    let plan = session.thread("Launch plan");
    let shown = session.conversation(&plan);
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
    servers.imap.append("INBOX", &lease(), &[]);
    session.until("the message with the lease", |session| {
        session
            .threads()
            .iter()
            .any(|t| session.text(t, "subject") == "Your lease renewal")
    });
    let lease = session.thread("Your lease renewal");
    let shown = session.conversation(&lease);
    assert!(
        shown.iter().any(|text| text.contains("lease")),
        "the conversation shows what it says: {shown:#?}"
    );
}

// USER REQUIREMENT: an empty answer is nothing to send: nothing is written and no blank
// message goes.
#[test]
fn a_reply_with_nothing_said_sends_nothing() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    let plan = session.thread("Launch plan");
    let drafts = session.refs(&session.mail_root(), "drafts").len();
    let answered = session.try_call("answer", &plan, &json!({ "body": "" }));
    assert!(
        answered
            .as_ref()
            .is_ok_and(|output| output.resources.is_empty()),
        "an empty answer makes nothing: {answered:?}"
    );
    assert_eq!(
        session.refs(&session.mail_root(), "drafts").len(),
        drafts,
        "no blank message is written"
    );
    for _ in 0..20 {
        session.settle();
    }
    assert!(servers.smtp.sent().is_empty(), "and none goes");
}

// USER REQUIREMENT: each account signs what it writes with the person's own signature, above
// what a reply quotes, as every mail client does.
#[test]
fn a_new_message_and_an_answer_carry_the_accounts_signature() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    let account = session.accounts().remove(0);
    assert_eq!(
        session.text(&account, "signature"),
        "",
        "an account starts without a signature"
    );
    session.call(
        "set_signature",
        &account,
        &json!({ "signature": "Sam\nhttps://rivera.example" }),
    );
    assert_eq!(
        session.text(&account, "signature"),
        "Sam\nhttps://rivera.example"
    );

    // A new message starts with it.
    let draft = session.made("new_message", &account, &json!({}));
    assert_eq!(
        session.text(&draft, "body"),
        "Sam\nhttps://rivera.example",
        "a new message carries the signature"
    );

    // An answer puts it over the quotation it answers.
    session.hold_for(0);
    let plan = session.thread("Launch plan");
    let reply = session.made("reply", &plan, &json!({}));
    let body = session.text(&reply, "body");
    assert!(
        body.starts_with("Sam\nhttps://rivera.example\n\nOn "),
        "the signature comes before the quotation: {body:?}"
    );

    // A message typed in the conversation goes with it, over the quotation too.
    session.call("answer", &plan, &json!({ "body": "Thursday suits me." }));
    session.until("the server to take the answer", |_| {
        servers.smtp.sent().len() == 1
    });
    let sent = String::from_utf8_lossy(&servers.smtp.sent()[0]).into_owned();
    let said = sent
        .find("Thursday suits me.")
        .expect("it says what was typed");
    let signature = sent
        .find("https://rivera.example")
        .expect("it carries the signature");
    let quotation = signature
        + sent
            .get(signature..)
            .and_then(|after| after.find("On "))
            .expect("it quotes what it answers");
    let quoted = sent
        .get(quotation..)
        .and_then(|rest| rest.lines().next())
        .unwrap_or_default();
    assert!(
        !quoted.contains("2026-"),
        "it names the day it answers as the person reads one, not as a machine does: {quoted:?}"
    );
    assert!(
        said < signature && signature < quotation,
        "the answer, its signature, then what it quotes: {sent}"
    );
}

// USER REQUIREMENT: replying from the conversation itself, with what was typed, quoting what it
// answers — the answer daily mail is, without going through a separate draft window.
#[test]
fn a_reply_typed_in_the_conversation_goes_out_once_quoting_the_message_it_answers() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    session.hold_for(0);
    let plan = session.thread("Launch plan");
    session.call("answer", &plan, &json!({ "body": "Thursday suits me." }));
    session.until("the server to take the answer", |_| {
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
    for _ in 0..20 {
        session.settle();
    }
    assert_eq!(servers.smtp.sent().len(), 1, "it goes once");
}

// USER REQUIREMENT: mail the workspace refuses is not lost: it stands in the window, which says
// it is not saved yet, and once the workspace takes changes again "Try again" hands it all of
// it, each message once.
#[test]
fn a_sync_the_workspace_refuses_is_kept_and_reaches_it_once_it_takes_changes() {
    let servers = empty_servers();
    let workspace = WorkspaceEnd::default();
    let connector = MailConnector::of(account(&servers));
    let mut session = connector.window_reaching(&workspace);
    workspace.refusing.store(true, Ordering::SeqCst);
    servers.imap.append("INBOX", &plan(), &[]);
    servers.imap.append(
        "INBOX",
        &mail(
            "dinner@chen.example",
            "Bo Chen <bo@chen.example>",
            "Dinner on Friday?",
            "Shall we try the new place?",
            None,
        ),
        &[],
    );
    session.until("the mail to stand on this device", |session| {
        session.threads().len() == 2
    });
    let said = ui::window_toolbar::notice().expect("the window says the mail is not saved");
    assert!(said.text.contains("kept on this device"), "{said:?}");
    assert!(ui::window_toolbar::edited(), "the window marks it unsaved");
    assert!(!workspace.holds(&session), "the workspace does not hold it");

    // The workspace takes changes again, and the person asks to save again, as the bar offers.
    workspace.refusing.store(false, Ordering::SeqCst);
    ui::window_toolbar::deliver(ui::window_toolbar::ToolbarEvent::Action {
        id: ui::SAVE_RETRY_ACTION.to_owned(),
    });
    synced(&mut session);
    assert!(
        workspace.holds(&session),
        "the workspace holds all the mail"
    );
    assert!(
        !ui::window_toolbar::edited() && ui::window_toolbar::notice().is_none(),
        "nothing is marked unsaved any more"
    );
    assert_eq!(
        session
            .refs(&session.thread("Launch plan"), "messages")
            .len(),
        1,
        "each message once"
    );
    let account = session.accounts().remove(0);
    assert_eq!(
        session.text(&account, "trouble"),
        "",
        "the account says nothing"
    );
}

// USER REQUIREMENT: a read the workspace cannot answer — its index has not caught up with what
// it holds — leaves no mail half kept: the account says the mail could not be saved, the
// window says what it cannot do and how old the mail is, and the next sync reads the workspace
// again, never taking it as "the account is gone". An account stays one account, and the mail
// the step waited on comes in once the workspace answers again.
#[test]
fn a_read_the_workspace_cannot_answer_keeps_one_account_and_its_inbox_fills() {
    let servers = empty_servers();
    let (_connector, mut session) = window_of(account(&servers));
    servers.imap.append("INBOX", &plan(), &[]);
    session.until("the conversation to come in", |session| {
        session.threads().len() == 1
    });
    let account = session.accounts().remove(0);
    let root = session.mail_root();
    let store = session.instance_host().store().clone();
    // An answer arrives, and the workspace cannot read the conversation it joins: the step that
    // keeps it waits for the next sync, and the account says so.
    store.fail_reads_of(&session.thread("Launch plan").id_string());
    servers.imap.append(
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
    session.call("check_now", &account, &json!({}));
    session.until("the account to say the mail is not saved", |session| {
        session
            .accounts()
            .first()
            .is_some_and(|a| session.text(a, "trouble").contains("could not be saved"))
    });
    assert!(
        session.flag(&account, "online"),
        "the server answered: it is the workspace that could not take what it said"
    );
    // The window says it over the mail the server did answer with, and how old that mail is:
    // the notice adds what the account cannot say of itself.
    let said = session.view_of(&account);
    assert!(
        said.iter().any(|text| text.contains("Can't save what")),
        "the notice says what the window cannot do: {said:#?}"
    );
    let when = said.iter().find_map(|text| {
        text.split_once("Last fetched ")
            .map(|(_, at)| at.to_owned())
    });
    let when = when.unwrap_or_else(|| panic!("the notice says when the mail was read: {said:#?}"));
    // The time reads in the clock the person chose in Settings, as every time the window shows
    // does: `14:05` or `2:05 PM`.
    let clock = ui::clock::configured();
    assert!(
        (0..24).any(|hour| (0..60).any(|minute| ui::clock::hour_minute(hour, minute) == when)),
        "the clock time a person reads: {when}, read in {clock:?}"
    );
    // The step waits for the next sync — and the store answers none of the account's documents
    // while it takes it in again, as an index that has not caught up with the workspace does.
    store.fail_reads_of(&root.id_string());
    for _ in 0..3 {
        session.settle();
    }
    // The workspace answers again: the account it holds is still the one account, and the mail
    // the step waited on comes in, once.
    store.take_reads();
    session.call("check_now", &account, &json!({}));
    let accounts = session.accounts();
    assert_eq!(
        accounts.len(),
        1,
        "a read that failed is no account gone, and no account was made again: {accounts:?}"
    );
    let account = accounts.into_iter().next().expect("the account");
    assert_eq!(session.text(&account, "address"), ME);
    session.until("the answer to come in", |session| {
        session
            .refs(&session.thread("Launch plan"), "messages")
            .len()
            == 2
    });
    assert_eq!(session.threads().len(), 1, "one conversation");
    assert_eq!(
        session.text(&account, "trouble"),
        "",
        "and the account says nothing is wrong"
    );
    assert!(session.flag(&account, "online"), "its server answers");
}
