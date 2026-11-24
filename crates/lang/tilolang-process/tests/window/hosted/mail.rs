//! Mail against a mail server, as a session runs it: the package's instance in a tilolang host,
//! kept in step with a test IMAP server through the mail connector it reaches over a socket, and
//! sending through a test SMTP server from the outbox. What people do goes through the API, as
//! a block the person runs, the way an agent or the person's own tool runs one.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use ::mail_engine::model::{AccountConfig, AuthMethod};
use serde_json::{Value as Json, json};
use tilo_account_probe::stand_in::{Layout, Serving, Started, started};
use tilolang_mail::serve::{Connector, Wakeup};
use tilolang_tilo::runtime::store::Store as _;
use tilolang_tilo::runtime::{ResourceRef, UiNode, Value as RtValue};

use super::{Session, Window};

const APP: &str = "so.tilo.mail";
const ME: &str = "sam@rivera.example";

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

/// One account's mail servers: its IMAP and SMTP are the one stand-in.
struct Servers {
    imap: Started,
    smtp: Started,
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

/// The lease as a PDF, as the letting agent sends it.
const LEASE_PDF: &[u8] = b"%PDF-1.4\n% lease 2026\n";

/// The lease renewal with the lease attached, as a server holds it.
fn lease_with_pdf() -> String {
    format!(
        "From: Mill Lane Lettings <office@lettings.example>\nTo: {ME}\nSubject: Your lease renewal\n\
Message-ID: <lease@lettings.example>\nDate: Tue, 30 Jun 2026 09:00:00 +0000\nMIME-Version: 1.0\n\
Content-Type: multipart/mixed; boundary=\"lease\"\n\n--lease\nContent-Type: text/plain; charset=utf-8\n\n\
The new lease is attached.\n--lease\nContent-Type: application/pdf; name=\"Lease 2026.pdf\"\n\
Content-Disposition: attachment; filename=\"Lease 2026.pdf\"\nContent-Transfer-Encoding: base64\n\n\
JVBERi0xLjQKJSBsZWFzZSAyMDI2Cg==\n--lease--\n"
    )
}

/// A message its sender formatted: the words as text, and the same as the HTML part a mail
/// program shows when it renders formatted mail.
fn mail_formatted(id: &str, from: &str, subject: &str, text: &str, html: &str) -> String {
    format!(
        "From: {from}\nTo: {ME}\nSubject: {subject}\nMessage-ID: <{id}>\n\
Date: Tue, 30 Jun 2026 09:00:00 +0000\nMIME-Version: 1.0\n\
Content-Type: multipart/alternative; boundary=\"formed\"\n\n\
--formed\nContent-Type: text/plain; charset=utf-8\n\n{text}\n\
--formed\nContent-Type: text/html; charset=utf-8\n\n{html}\n--formed--\n"
    )
}

/// The two servers with nothing in the inbox yet: what a test puts there once the window stands,
/// as mail arrives at an open window.
fn empty_servers() -> Servers {
    servers_of(ME)
}

/// Empty servers that take `login`'s sign-in.
fn servers_of(login: &str) -> Servers {
    let imap = started(Serving {
        layout: Layout::Gmail,
        ..Serving::local(login, "app-password")
    })
    .expect("the mail servers start");
    Servers {
        smtp: imap.clone(),
        imap,
    }
}

fn account(servers: &Servers) -> AccountConfig {
    AccountConfig {
        display_name: "Sam Rivera".into(),
        email: ME.into(),
        username: ME.into(),
        password: "app-password".into(),
        imap_host: "127.0.0.1".into(),
        imap_port: servers.imap.imap.port(),
        smtp_host: "127.0.0.1".into(),
        smtp_port: servers.smtp.smtp.port(),
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
        Self::keeping(source, None)
    }

    /// [`Self::new`], keeping the files messages carry in `content`: the device's content store,
    /// which the windows read them from.
    fn keeping(
        source: impl Fn() -> Vec<AccountConfig> + Send + 'static,
        content: Option<tilo_content::ContentStore>,
    ) -> Self {
        let (wakeups, woken) = std::sync::mpsc::channel::<Wakeup>();
        let looping = wakeups.clone();
        let online = Arc::new(AtomicBool::new(false));
        let device = Arc::clone(&online);
        std::thread::spawn(move || {
            let engine_woken = looping.clone();
            let wake: ::mail_engine::engine::Wake = Arc::new(move || {
                let _ = engine_woken.send(Wakeup::Engine);
            });
            let engine = ::mail_engine::engine::live::LiveEngine::trusting(
                tilo_content::ContentStore::memory(),
                Arc::clone(&wake),
                // Each account's stand-in starts after the connector, signed by the one
                // authority every stand-in is.
                vec![tilo_account_probe::stand_in::authority().expect("the authority")],
            )
            .expect("the engine starts");
            let (requests, events) = ::mail_engine::engine::spawn_waking(Box::new(engine), wake);
            let source = move || source().into_iter().map(Into::into).collect();
            Connector::new(Box::new(source), (requests, events), device, content)
                .run(&woken, &looping);
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

/// Ada at a Mail window kept in step with `config`'s servers, the files messages carry kept in
/// `content`, the device's store, which the connector and the window share.
fn window_keeping(
    config: AccountConfig,
    content: &tilo_content::ContentStore,
) -> (MailConnector, Session) {
    Session::keeping_files_in(content);
    let connector = MailConnector::keeping(move || vec![config.clone()], Some(content.clone()));
    let session = connector.window();
    (connector, session)
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
        // The API runs an action by the name the app publishes it under. What the action asks
        // on the way (a send its code makes) waits in its asked record, and the person allows
        // it, as they do in the shell's asking view.
        let answered = self.invoke_allowing(None, None, &resource, name, &arguments)?;
        self.settle();
        Ok(answered)
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

    /// Open `resource` through the `open` the app publishes for it, by the address the
    /// catalogue offers it under, as a person's row or an agent's call does; then settle.
    fn open_showing(&mut self, resource: &ResourceRef) {
        let published = tilolang_tilo::publish::resource_id(APP, &resource.id_string());
        if let Err(error) = self.invoke(None, None, &published, "open", &serde_json::Map::new()) {
            panic!("{published}'s open answers: {error}");
        }
        self.settle();
    }

    /// The rows of the page the window opened beside its list, each row's texts: what a person
    /// reads in the mailbox they opened.
    fn page_rows(&mut self) -> Vec<Vec<String>> {
        let view = self.hosted().view().expect("the window is connected");
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
        rows
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

/// Every node under `node` whose `label` is `label`.
fn labelled<'a>(node: &'a UiNode, label: &str) -> Vec<&'a UiNode> {
    let mut found = Vec::new();
    if matches!(node.props.get("label"), Some(RtValue::Text(text)) if text == label) {
        found.push(node);
    }
    for child in &node.children {
        found.extend(labelled(child, label));
    }
    found
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

// USER REQUIREMENT: a mailbox holding more than one page of messages can be read back past the
// newest page, so mail older than the first fifty is reachable.
#[test]
fn a_mailbox_longer_than_a_page_reads_the_page_before_it_when_asked() {
    let servers = empty_servers();
    for i in 0..60 {
        servers.imap.append(
            "INBOX",
            &mail(
                &format!("note-{i}@hopper.example"),
                "Grace Hopper <grace@hopper.example>",
                &format!("Note {i}"),
                "A line.",
                None,
            ),
            &[],
        );
    }
    let (_connector, mut session) = window_of(account(&servers));
    session.until("the newest page", |session| session.threads().len() == 50);
    let account = session.accounts().remove(0);
    let inbox = session.refs(&account, "mailboxes")[0].clone();
    assert!(
        session.flag(&inbox, "more"),
        "the mailbox says its server holds older mail"
    );
    session.call("older_messages", &inbox, &json!({}));
    session.until("the page before it", |session| {
        session.threads().len() == 60
    });
    assert!(
        !session.flag(&inbox, "more"),
        "the folder's first message is here, and it offers no more"
    );
}

// USER REQUIREMENT: a mailbox holding more than one page reads the page before it as the person
// reads the list to its end, without pressing anything.
#[test]
fn a_mailbox_read_to_the_lists_end_reads_the_page_before_it() {
    let servers = empty_servers();
    sixty_messages(&servers);
    let (_connector, mut session, inbox) = window_on_a_long_inbox(&servers);
    // The window's list tells the person's reading to its end; what the list says it runs reads
    // the page before the rows. The toolkit's own reading of a scroll to the end is the list's
    // (`ui::widget::virtual_list`), so the event it fires is what the window sends here.
    let composed = session
        .hosted()
        .view()
        .and_then(tilo_app_window::AppWindow::composed)
        .cloned()
        .expect("the window composed its shell");
    let list =
        first_window_kind(&composed, "ui.VirtualList").expect("the window lists conversations");
    assert!(
        list.events.contains_key("end"),
        "the list reads its end: {:?}",
        list.events.keys().collect::<Vec<_>>()
    );
    let key = list.key.clone();
    session
        .hosted()
        .view()
        .expect("the window is connected")
        .act(tilo_app_window::reconcile::UiEvent::Fire(
            key,
            "end".to_owned(),
            None,
            None,
        ));
    session.settle();
    // Reading to the list's end is the person's own act: the page before the rows is read at
    // once, and nobody is asked about it.
    session.until("the page before it", |session| {
        assert!(
            session.asked().is_empty(),
            "nobody is asked about their own reading: {:?}",
            session.asked()
        );
        session.threads().len() == 60
    });
    assert!(
        !session.flag(&inbox, "more"),
        "the folder's first message is here, and it offers no more"
    );
}

/// The first node of `kind` in the shell `node` the window composed, itself included.
fn first_window_kind<'a>(
    node: &'a tilo_app_window::UiNode,
    kind: &str,
) -> Option<&'a tilo_app_window::UiNode> {
    if node.kind == kind {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| first_window_kind(child, kind))
}

/// Sixty messages in `servers`' inbox, more than the one page an account syncs first.
fn sixty_messages(servers: &Servers) {
    for i in 0..60 {
        servers.imap.append(
            "INBOX",
            &mail(
                &format!("note-{i}@hopper.example"),
                "Grace Hopper <grace@hopper.example>",
                &format!("Note {i}"),
                "A line.",
                None,
            ),
            &[],
        );
    }
}

/// The window kept in step with `servers`, its account holding the newest page of sixty
/// messages: the inbox whose server still holds the page before it.
fn window_on_a_long_inbox(servers: &Servers) -> (MailConnector, Session, ResourceRef) {
    let (connector, mut session) = window_of(account(servers));
    session.until("the newest page", |session| session.threads().len() == 50);
    let account = session.accounts().remove(0);
    let inbox = session.refs(&account, "mailboxes")[0].clone();
    assert!(
        session.flag(&inbox, "more"),
        "the mailbox says its server holds older mail"
    );
    (connector, session, inbox)
}

// USER REQUIREMENT: mail that another client deletes here goes, and a folder the server
// numbered again is read once more rather than kept stale or doubled.
#[test]
fn mail_deleted_elsewhere_leaves_and_a_renumbered_folder_is_read_again() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    let dinner = servers
        .imap
        .messages("INBOX")
        .into_iter()
        .find(|held| String::from_utf8_lossy(&held.raw).contains("Dinner on Friday?"))
        .expect("the dinner message");
    servers.imap.remove("INBOX", dinner.uid);
    session.until("the deleted message to leave", |session| {
        session
            .threads()
            .iter()
            .all(|thread| session.text(thread, "subject") != "Dinner on Friday?")
    });
    servers.imap.renumber("INBOX");
    servers.imap.append("INBOX", &lease(), &[]);
    session.until("the renumbered inbox", |session| {
        let threads = session.threads();
        threads.len() == 2
            && threads
                .iter()
                .filter(|thread| session.text(thread, "subject") == "Launch plan")
                .count()
                == 1
            && threads
                .iter()
                .any(|thread| session.text(thread, "subject") == "Your lease renewal")
    });
}

// USER REQUIREMENT: mail the server files as junk shows in Mail, under its Junk place.
#[test]
fn junk_the_server_holds_shows_in_its_place() {
    let servers = servers();
    servers.imap.append(
        "[Gmail]/Spam",
        &mail(
            "offer@deals.example",
            "Prize Draw <win@deals.example>",
            "You have won a cruise",
            "Claim your prize.",
            None,
        ),
        &[],
    );
    let (_connector, mut session) = window_of(account(&servers));
    session.until("the mail to sync", |session| session.threads().len() == 3);
    let account = session.accounts().remove(0);
    assert!(
        session.threads().iter().any(|thread| {
            session.text(thread, "subject") == "You have won a cruise"
                && matches!(session.value_of(thread, "boxes"), RtValue::List(boxes)
                    if boxes.contains(&RtValue::text("junk")))
        }),
        "the junk is filed under junk"
    );
    assert_eq!(
        session.place_count(&account, "Junk"),
        Some(1),
        "the Junk place lists it"
    );
}

/// An editor of the workspace's mailbox, which holds its own keys, is offered Check now like
/// its owner, and the press runs: Mail's author is trusted for the account check it declares,
/// so an admitted call is not asked again. What an author may not do still asks before it runs.
#[test]
fn an_editors_check_now_is_offered_and_runs_admitted() {
    let _settled = ui::motion::testing::settled();
    let servers = servers();
    // The server tells no idling connection of new mail: what comes is read when it is checked.
    servers.imap.without_idle();
    let connector = MailConnector::of(account(&servers));
    let mut session = connector.attach(Session::bo_editing(APP, Window::in_harness));
    synced(&mut session);
    let check = session
        .hosted()
        .view()
        .expect("the window is connected")
        .actions()
        .iter()
        .find(|action| action.action_label == "Check now")
        .expect("the window offers Check now")
        .contract
        .availability
        .clone();
    assert_eq!(check, ui::tilo_actions::Availability::Available);
    servers.imap.append(
        "INBOX",
        &mail(
            "lunch@chen.example",
            "Bo Chen <bo@chen.example>",
            "Lunch on Monday?",
            "Shall we meet at noon?",
            None,
        ),
        &[],
    );
    let pressed = std::time::Instant::now();
    super::choose(&mut session, "Check now");
    assert_eq!(
        session.asked(),
        Vec::<String>::new(),
        "an admitted call runs without asking again"
    );
    session.until("the server to be checked", |session| {
        session
            .threads()
            .iter()
            .any(|t| session.text(t, "subject") == "Lunch on Monday?")
    });
    // The press checks the server at once, not whenever the window next writes something.
    assert!(
        pressed.elapsed() < std::time::Duration::from_secs(15),
        "the check waited {:?} after the press",
        pressed.elapsed()
    );
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

/// What Calendar can hand Mail for a guest it invites: a message with the event's invitation,
/// which goes out with its calendar part once the person sends it.
#[test]
fn a_calendar_invitation_is_a_draft_that_goes_out_with_its_calendar_part() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    session.hold_for(0);
    let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Tilo//Calendar//EN\r\nMETHOD:REQUEST\r\n\
               BEGIN:VEVENT\r\nUID:launch@calendar.example\r\nDTSTAMP:20261112T090000Z\r\n\
               DTSTART:20261113T100000Z\r\nDTEND:20261113T110000Z\r\nSUMMARY:Launch review\r\n\
               ATTENDEE:mailto:grace@hopper.example\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let root = session.mail_root();
    assert!(
        session
            .try_call(
                "prepare_calendar_invitation",
                &root,
                &json!({"to": "grace@hopper.example", "subject": "Invitation: Launch review",
                        "body": "You're invited.", "calendar_ics": "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n"}),
            )
            .is_err(),
        "a calendar that invites to nothing is no invitation"
    );
    let draft = session.made(
        "prepare_calendar_invitation",
        &root,
        &json!({"to": "grace@hopper.example", "subject": "Invitation: Launch review",
                "body": "You're invited.", "calendar_ics": ics}),
    );
    assert_eq!(session.text(&draft, "to"), "grace@hopper.example");
    assert!(
        !session.flag(&draft, "sent"),
        "nothing is sent until the person sends it"
    );
    assert!(servers.smtp.sent().is_empty());

    session.call("send", &draft, &json!({}));
    session.until("the server to take the invitation", |_| {
        servers.smtp.sent().len() == 1
    });
    let sent = String::from_utf8_lossy(&servers.smtp.sent()[0]).into_owned();
    assert!(
        sent.contains("Subject: Invitation: Launch review"),
        "{sent}"
    );
    assert!(sent.contains("text/calendar; method=REQUEST"), "{sent}");
    assert!(sent.contains("invitation.ics"), "{sent}");
}

/// Mail moved in from another program arrives a message at a time, as `tilo migrate` hands each
/// over: it is kept with the mail held outside the accounts, in its conversation and folder, and
/// a message handed over again is the one already kept.
#[test]
fn a_message_delivered_from_outside_is_kept_once_in_its_conversation() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    let root = session.mail_root();
    let first = json!({
        "from": "Ada Lovelace <ada@analytical.example>", "to": ME,
        "subject": "Engine notes", "body": "The notes are attached.\r\nAda",
        "folder": "archive", "read": true, "date": "2024-03-01T09:00:00Z",
        "conversation": "notes@analytical.example", "message_id": "notes@analytical.example",
        "original": "tilo.content:abc",
        "attachments": [{"file": {"name": "notes.pdf", "hash": "def", "size": 3}}],
        "parts": [r#"{"at":120,"size":12,"pieces":["tilo.content:def"]}"#],
    });
    let delivered = session.call("deliver_message", &root, &first);
    let again = session.call("deliver_message", &root, &first);
    assert_eq!(
        delivered.resources, again.resources,
        "the message handed over again is the one kept"
    );
    session.call(
        "deliver_message",
        &root,
        &json!({
            "from": ME, "to": "ada@analytical.example", "subject": "Re: Engine notes",
            "body": "Thank you.", "folder": "sent", "read": false,
            "date": "2024-03-02T10:00:00Z", "conversation": "notes@analytical.example",
            "message_id": "thanks@rivera.example",
        }),
    );

    let kept = session.refs(&root, "recovered_mail");
    let [thread] = kept.as_slice() else {
        panic!("one conversation: {kept:?}");
    };
    assert_eq!(session.text(thread, "subject"), "Engine notes");
    assert!(
        session
            .text(thread, "snippet")
            .starts_with("The notes are attached."),
        "a conversation reads as its first message"
    );
    assert!(
        session.flag(thread, "unread"),
        "the unread reply leaves it unread"
    );
    let messages = session.refs(thread, "messages");
    assert_eq!(messages.len(), 2, "each message once");
    let notes = &messages[0];
    assert_eq!(session.text(notes, "from"), "ada@analytical.example");
    assert_eq!(session.text(notes, "from_name"), "Ada Lovelace");
    assert_eq!(session.text(notes, "mailbox"), "archive");
    assert_eq!(session.text(notes, "original"), "tilo.content:abc");
    assert!(
        matches!(
            session.value_of(notes, "attachments"),
            RtValue::List(files) if matches!(files.as_slice(), [RtValue::File(f)] if f.name == "notes.pdf" && f.hash == "def")
        ),
        "the file it carries is kept as the content it names"
    );
    assert!(session.text(notes, "parts").contains("tilo.content:def"));
    assert!(!session.flag(notes, "unread"));
    assert_eq!(
        session.text(notes, "body"),
        "The notes are attached.\r\nAda"
    );
    assert!(
        session
            .try_call("deliver_message", &root, &json!({"subject": "Nobody"}))
            .is_err(),
        "a message says who it is from"
    );

    // What came in answers as any message does.
    let reply = session.made("reply_to", notes, &json!({}));
    assert_eq!(session.text(&reply, "to"), "ada@analytical.example");
    assert_eq!(session.text(&reply, "subject"), "Re: Engine notes");
    // It came to no account the workspace has, so the answer goes from the one it reads now.
    assert_eq!(session.text(&reply, "from"), ME);
    let reply = session.made("reply", thread, &json!({}));
    assert_eq!(session.text(&reply, "from"), ME);
    assert!(
        session.view_of(&reply).iter().any(|text| text == ME),
        "the composer says the address it goes from"
    );
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
        session.instance_host().store().outbox_pending().is_empty()
    });
    let sent = String::from_utf8_lossy(&servers.smtp.sent()[0]).into_owned();
    assert!(sent.contains("Subject: Re: Launch plan"), "{sent}");
    assert!(
        sent.contains("In-Reply-To: <plan-2@analytical.example>"),
        "{sent}"
    );
    assert!(session.flag(&draft, "sent"));
}

/// The conversation's reply bar answers and sends in one press: the person is asked once, and
/// allowing it sends the answer the bar made, as the Send of a draft does.
#[test]
fn an_answer_typed_under_a_conversation_goes_out_once_allowed() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    session.hold_for(0);
    let plan = session.thread("Launch plan");
    session.call("answer", &plan, &json!({ "body": "Thursday works." }));
    session.until("the server to take the answer", |_| {
        servers.smtp.sent().len() == 1
    });
    session.until("the outbox to settle", |session| {
        session.instance_host().store().outbox_pending().is_empty()
    });
    let sent = String::from_utf8_lossy(&servers.smtp.sent()[0]).into_owned();
    assert!(sent.contains("Subject: Re: Launch plan"), "{sent}");
    assert!(sent.contains("Thursday works."), "{sent}");
}

/// The node of `kind` whose prop `prop` says `value` in the shell the window composed: what the
/// person sees and acts on.
fn on_page(
    session: &mut Session,
    kind: &str,
    prop: &str,
    value: &str,
) -> Option<tilo_app_window::UiNode> {
    fn find(
        node: &tilo_app_window::UiNode,
        hit: &dyn Fn(&tilo_app_window::UiNode) -> bool,
    ) -> Option<tilo_app_window::UiNode> {
        if hit(node) {
            return Some(node.clone());
        }
        node.children.iter().find_map(|child| find(child, hit))
    }
    let composed = session.hosted().view()?.composed()?;
    find(composed, &|node| {
        node.kind == kind && node.props.get(prop) == Some(&tilo_app_window::Value::text(value))
    })
}

/// The reply bar under a conversation, pressed: the press is the person's own, so nobody is
/// asked about it. The answer it makes goes once, and the bar empties for the next reply.
#[test]
fn a_reply_pressed_on_the_reply_bar_goes_without_asking_and_the_bar_clears() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    session.hold_for(0);
    let plan = session.thread("Launch plan");
    session.open_conversation(&plan);
    let input = on_page(
        &mut session,
        "ui.TextInput",
        "placeholder",
        "Reply to Launch plan",
    )
    .expect("the conversation offers its reply bar");
    session
        .hosted()
        .view()
        .expect("the window is connected")
        .act(tilo_app_window::reconcile::UiEvent::Edit(
            input.key.clone(),
            "Thursday suits me.".to_owned(),
        ));
    session.settle();
    let typed = |session: &mut Session| {
        session
            .hosted()
            .view()
            .and_then(|view| view.bound_value(&input.key))
    };
    assert_eq!(
        typed(&mut session),
        Some(tilo_app_window::Value::text("Thursday suits me.")),
        "the bar holds what was typed"
    );
    let reply = on_page(&mut session, "ui.Button", "label", "Reply").expect("the bar's Reply");
    session
        .hosted()
        .view()
        .expect("the window is connected")
        .act(tilo_app_window::reconcile::UiEvent::Fire(
            reply.key,
            "press".to_owned(),
            None,
            None,
        ));
    session.settle();
    assert!(
        session.asked().is_empty(),
        "nobody is asked about their own press: {:?}",
        session.asked()
    );
    session.until("the server to take the answer", |_| {
        servers.smtp.sent().len() == 1
    });
    let sent = String::from_utf8_lossy(&servers.smtp.sent()[0]).into_owned();
    assert!(sent.contains("Thursday suits me."), "{sent}");
    assert!(sent.contains("Subject: Re: Launch plan"), "{sent}");
    session.until("the bar to be emptied for the next reply", |session| {
        typed(session) == Some(tilo_app_window::Value::text(""))
    });
    session.until("the outbox to settle", |session| {
        session.instance_host().store().outbox_pending().is_empty()
    });
    session.settle();
    assert_eq!(servers.smtp.sent().len(), 1, "it goes once");
    assert!(session.asked().is_empty(), "nothing is asked after it either");
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
        session.instance_host().store().outbox_pending().is_empty()
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
    let servers = servers_of("work@rivera.example");
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
    // Each account shows by its address: one person's two accounts carry the same name, and
    // only the address tells them apart.
    let texts = session.shown();
    for want in [ME, "work@rivera.example"] {
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

// USER REQUIREMENT: All inboxes is every account's inbox in one list, not only the first
// account's.
#[test]
fn all_inboxes_lists_every_accounts_inbox_in_one_list() {
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
    // The window opens on All inboxes: the conversations of both accounts stand in its one
    // list.
    session.until("both inboxes in one list", |session| {
        let titles = session.titles();
        titles.len() == 3
            && titles
                .iter()
                .any(|title| title == "Ada Lovelace, Grace Hopper")
            && titles.iter().any(|title| title == "Bo Chen")
            && titles.iter().any(|title| title == "holiday@team.example")
    });
    let root = session.mail_root();
    let view = session
        .instance_host()
        .open_resource(&root, None)
        .expect("the mail window opens");
    assert_eq!(
        count_in(view.tree(), "ui.NavShell"),
        Some(3),
        "the list counts both accounts' inboxes"
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

// USER REQUIREMENT: a file that came with a message is a real file: it saves as the bytes the
// server sent, it goes along when the message is sent on, and a reply takes it as an attachment.
#[test]
fn an_attachment_saves_as_its_file_and_goes_along_with_what_is_sent() {
    let servers = servers();
    let content = tilo_content::ContentStore::memory();
    let (_connector, mut session) = window_keeping(account(&servers), &content);
    synced(&mut session);
    session.hold_for(0);
    servers.imap.append("INBOX", &lease_with_pdf(), &[]);
    let message = session.wait_for("the lease with its file", |session| {
        let lease = session
            .threads()
            .into_iter()
            .find(|t| session.text(t, "subject") == "Your lease renewal")?;
        session.refs(&lease, "messages").into_iter().next()
    });
    let shown = session.conversation(&session.thread("Your lease renewal"));
    assert!(
        shown.iter().any(|text| text == "Lease 2026.pdf"),
        "the message shows the file it came with: {shown:#?}"
    );

    // It is the file the server sent, byte for byte, kept in the device's content.
    let RtValue::List(files) = session.value_of(&message, "attachments") else {
        panic!("a message holds its files");
    };
    let [RtValue::File(file)] = files.as_slice() else {
        panic!("the lease came with one file: {files:?}");
    };
    assert_eq!(file.name, "Lease 2026.pdf");
    assert_eq!(file.media, "application/pdf");
    let hash = file.hash.clone();
    assert_eq!(content.read(&hash).as_deref(), Some(LEASE_PDF));
    session.call("attachment", &message, &json!({"name": "Lease 2026.pdf"}));
    assert!(
        session
            .try_call("attachment", &message, &json!({"name": "Other.pdf"}))
            .is_err(),
        "a file it did not come with is none"
    );

    // Sent on, it carries the file.
    let forward = session.made("forward_message", &message, &json!({}));
    assert!(
        matches!(
            session.value_of(&forward, "attachments"),
            RtValue::List(files) if matches!(files.as_slice(), [RtValue::File(f)] if f.hash == hash)
        ),
        "the forward takes the lease along"
    );

    // A reply takes the file as an attachment, as the person picks it, and the server gets it.
    let reply = session.made("reply_to", &message, &json!({}));
    session.call(
        "attach",
        &reply,
        &json!({"file": {"file": {
            "name": "Lease 2026.pdf",
            "media": "application/pdf",
            "size": LEASE_PDF.len(),
            "hash": hash,
        }}}),
    );
    session.call("send", &reply, &json!({}));
    session.until("the server to take the reply", |_| {
        servers.smtp.sent().len() == 1
    });
    let sent = String::from_utf8_lossy(&servers.smtp.sent()[0]).into_owned();
    assert!(sent.contains("Lease 2026.pdf"), "{sent}");
    assert!(sent.contains("JVBERi0xLjQKJSBsZWFzZSAyMDI2Cg=="), "{sent}");
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

// USER REQUIREMENT: a reply the workspace has no account to send says so where it names the
// sender, and where a Send would stand it offers the way to Settings' accounts, rather than a
// blank From and a press that does nothing.
#[test]
fn a_reply_with_no_account_to_send_it_says_so_and_does_not_send() {
    let connector = MailConnector::new(Vec::new);
    let mut session = connector.window();
    session.settle();
    let root = session.mail_root();
    session.call(
        "deliver_message",
        &root,
        &json!({
            "from": "Grace Hopper <grace@navy.example>", "to": "Office <office@company.example>",
            "subject": "Quarterly invoices", "body": "Please confirm the totals by Friday.",
            "date": "2026-11-12T09:30:00Z", "folder": "inbox",
            "message_id": "<invoices@navy.example>",
        }),
    );
    let kept = session.refs(&root, "recovered_mail");
    let [thread] = kept.as_slice() else {
        panic!("one conversation: {kept:?}");
    };
    let draft = session.made("reply", thread, &json!({}));
    assert_eq!(session.text(&draft, "subject"), "Re: Quarterly invoices");
    assert_eq!(session.text(&draft, "to"), "grace@navy.example");
    assert_eq!(session.text(&draft, "from"), "");

    let shown = session.view_of(&draft);
    for said in ["No mail account to send from", "Add an account in Settings"] {
        assert!(
            shown.iter().any(|text| text == said),
            "{said:?} in {shown:?}"
        );
    }
    // Where a composer's Send stands, the way to add an account: pressing it opens the Settings
    // page that adds one, and the draft stays as it is.
    let window = session
        .instance_host()
        .open_resource(&draft, None)
        .expect("its window opens");
    let links = labelled(window.tree(), "Add an account to send");
    let link = links
        .first()
        .unwrap_or_else(|| panic!("the composer offers the way to an account: {shown:?}"));
    assert_eq!(link.kind, "ui.Link");
    assert_eq!(
        link.props.get("url"),
        Some(&RtValue::Text("settings://external_accounts".to_owned())),
        "it opens the page that adds an account: {:?}",
        link.props
    );
    // The composer itself sends nothing with no account; the window's own Send says why.
    for send in labelled(window.tree(), "Send") {
        if send.kind == "ui.Button" {
            assert_eq!(
                send.props.get("enabled"),
                Some(&RtValue::Int(0)),
                "a Send with no account is unavailable: {:?}",
                send.props
            );
        }
    }
    let refused = session
        .try_call("send", &draft, &json!({}))
        .expect_err("a draft with no account is not sent");
    assert!(
        refused.contains("Add a mail account in Settings first"),
        "{refused}"
    );
    assert!(
        session.refs(&root, "drafts").contains(&draft),
        "the draft stays where it was written"
    );
}

// USER REQUIREMENT: a person always writes a message. With no account it waits in Drafts, and
// once an account is added the same draft goes from it — nothing written is lost to the wait.
#[test]
fn a_message_written_with_no_account_is_kept_and_sends_once_one_is_added() {
    let servers = servers();
    let reachable = account(&servers);
    let config = Arc::new(Mutex::new(Vec::<AccountConfig>::new()));
    let accounts = Arc::clone(&config);
    let connector = MailConnector::new(move || accounts.lock().unwrap().clone());
    let mut session = connector.window();
    session.settle();
    let root = session.mail_root();
    assert!(session.accounts().is_empty(), "no account yet");
    session.call(
        "deliver_message",
        &root,
        &json!({
            "from": "Ada Lovelace <ada@analytical.example>",
            "to": "Sam Rivera <sam@rivera.example>",
            "subject": "Engine notes",
            "body": "Could you look at the timing?",
            "date": "2026-11-12T09:30:00Z",
            "message_id": "<engine@analytical.example>",
        }),
    );
    let kept = session.refs(&root, "recovered_mail");
    let [thread] = kept.as_slice() else {
        panic!("one conversation: {kept:?}");
    };

    // The answer is written with no account, and stays in Drafts.
    let draft = session.made("reply", thread, &json!({}));
    assert_eq!(session.text(&draft, "from"), "");
    assert_eq!(session.text(&draft, "to"), "ada@analytical.example");
    assert!(
        session.refs(&root, "drafts").contains(&draft),
        "it waits in Drafts"
    );
    assert!(session.try_call("send", &draft, &json!({})).is_err());
    assert!(!session.flag(&draft, "sent"));

    // The person adds an account, as Settings' accounts page does.
    *config.lock().unwrap() = vec![reachable];
    connector.come_online();
    session.until("the account to be added", |session| {
        !session.accounts().is_empty()
    });
    let account = session.accounts().remove(0);
    let address = session.text(&account, "address");
    session.call(
        "send_from",
        &draft,
        &json!({ "a": {
            "app": APP,
            "kind": format!("tl.{APP}.Account"),
            "id": account.id_string(),
            "label": address,
        }}),
    );
    assert_eq!(session.text(&draft, "from"), session.text(&account, "key"));

    // The same draft goes, once.
    session.hold_for(0);
    session.call("send", &draft, &json!({}));
    session.until("the message to be sent", |session| {
        session
            .delivery_of(&draft)
            .is_some_and(|(state, _)| state == "sent")
    });
    assert_eq!(servers.smtp.sent().len(), 1, "it goes once");
    let sent = String::from_utf8_lossy(&servers.smtp.sent()[0]).into_owned();
    assert!(
        sent.contains("Subject: Re: Engine notes"),
        "the draft that waited goes as it was written: {sent}"
    );
}

// USER REQUIREMENT: every message being written waits in Drafts — a new one, one sent on, and
// an answer — and an answer opens with the conversation it answers.
#[test]
fn drafts_lists_every_message_being_written_and_an_answer_shows_its_conversation() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    let account = session.accounts().remove(0);
    let fresh = session.made("new_message", &account, &json!({}));
    let plan = session.thread("Launch plan");
    let reply = session.made("reply", &plan, &json!({}));
    let _forwarded = session.made("forward", &plan, &json!({}));
    assert_eq!(
        session.text(&reply, "conversation"),
        session.text(&plan, "key"),
        "the answer names the conversation it answers"
    );
    assert_eq!(
        session.text(&fresh, "conversation"),
        "",
        "a new message answers nothing"
    );

    // The Drafts place lists all three kinds.
    let root = session.mail_root();
    let mut view = session
        .instance_host()
        .open_resource(&root, None)
        .expect("the mail window opens");
    let drafts = find_kind(view.tree(), "ui.NavItem", "label", "Drafts")
        .expect("the sidebar offers Drafts")
        .key
        .clone();
    view.fire(&drafts, "select").expect("Drafts opens");
    view.poll_patches().expect("the place shows");
    assert_eq!(
        count_in(view.tree(), "ui.NavShell"),
        Some(3),
        "Drafts lists the new, sent-on and reply drafts"
    );

    // An answer opens with the conversation it answers; a new message has none to show.
    let shown = session.view_of(&reply);
    assert!(
        shown.iter().any(|text| text == "This answers Launch plan"),
        "{shown:#?}"
    );
    let shown = session.view_of(&fresh);
    assert!(
        !shown.iter().any(|text| text.starts_with("This answers ")),
        "{shown:#?}"
    );
}

/// How many messages the Drafts place of `session`'s mail window lists, choosing the place as a
/// person does.
fn drafts_count(session: &Session) -> Option<i64> {
    let root = session.mail_root();
    let mut view = session
        .instance_host()
        .open_resource(&root, None)
        .expect("the mail window opens");
    let drafts = find_kind(view.tree(), "ui.NavItem", "label", "Drafts")
        .expect("the sidebar offers Drafts")
        .key
        .clone();
    view.fire(&drafts, "select").expect("Drafts opens");
    view.poll_patches().expect("the place shows");
    count_in(view.tree(), "ui.NavShell")
}

// USER REQUIREMENT: a message being written belongs to the person writing it. Each member's
// Drafts place lists their own drafts and none another member's, so a draft in progress is
// private to its author while the mailbox is the workshop.
#[test]
fn each_member_reads_only_their_own_drafts_in_the_drafts_place() {
    use super::{RouteSpec, spec_of};
    use std::rc::Rc;
    use tilolang_process::testing::Bus;

    let bus = Rc::new(Bus::default());
    bus.member(ui::tilo_actions::MemberId::person("ada"), "Ada");
    bus.member(ui::tilo_actions::MemberId::person("bo"), "Bo");
    let route = RouteSpec::app(APP, uuid::Uuid::new_v4());
    let mut ada = Session::on_bus(&bus, &route, spec_of(&route, "ada"));
    let mut bo = Session::on_bus(&bus, &route, spec_of(&route, "bo"));
    ada.settle();
    bo.settle();

    // Each writes a message: with no account they still wait in Drafts.
    let ada_root = ada.mail_root();
    let bo_root = bo.mail_root();
    let _ada_draft = ada.made("new_draft", &ada_root, &json!({}));
    let _bo_draft = bo.made("new_draft", &bo_root, &json!({}));
    // One draft each stands in the workspace, and each member's place lists the one they wrote.
    assert_eq!(
        drafts_count(&ada),
        Some(1),
        "Ada's Drafts place lists only the draft she wrote"
    );
    assert_eq!(
        drafts_count(&bo),
        Some(1),
        "Bo's Drafts place lists only the draft he wrote"
    );
}

/// The buttons the window's bar offers, in the order it offers them: the choice's bar stands in
/// the part the window's own controls stand in, which is its overview's when it lists items.
fn bar_buttons(tree: &UiNode) -> Vec<&UiNode> {
    let bar = tree
        .of_kind("ui.NavShell")
        .first()
        .copied()
        .expect("a window has one bar");
    bar.children
        .iter()
        .filter(|child| {
            child.kind == "part"
                && matches!(
                    child.props.get("name").and_then(RtValue::as_text),
                    Some("actions" | "overview_actions")
                )
        })
        .flat_map(|part| part.children.iter())
        .filter(|node| node.kind == "ui.Button")
        .collect()
}

/// More than one conversation is chosen at a time, and one command over the choice reaches every
/// one of them as a single step: choosing the two in the inbox and archiving takes them both out
/// of it, here and on the server, and one undo brings them back together.
#[test]
fn chosen_conversations_are_archived_with_one_command() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    let root = session.mail_root();
    let plan = session.thread("Launch plan");
    let dinner = session.thread("Dinner on Friday?");
    let mut window = session
        .instance_host()
        .open_resource(&root, None)
        .expect("the mail window opens");
    let list = window
        .tree()
        .of_kind("ui.VirtualList")
        .first()
        .expect("the window lists the conversations")
        .key
        .clone();
    let chosen = RtValue::List(vec![RtValue::resource(&plan), RtValue::resource(&dinner)]);
    window
        .fire_with(&list, "selected", Some(&chosen))
        .expect("the rows are chosen");
    window.poll_patches().expect("the window shows the choice");
    // The bar says how many are chosen, and offers what every one of them can take.
    let tree = window.tree();
    let said = tree
        .of_kind("ui.Badge")
        .into_iter()
        .filter_map(|node| node.props.get("text").and_then(RtValue::as_text))
        .collect::<Vec<_>>();
    assert!(
        said.contains(&"2 selected"),
        "the bar says what is chosen: {said:?}"
    );
    let archive = bar_buttons(tree)
        .into_iter()
        .find(|node| node.props.get("label").and_then(RtValue::as_text) == Some("Archive"))
        .unwrap_or_else(|| {
            let offered = bar_buttons(tree)
                .into_iter()
                .filter_map(|node| node.props.get("label").and_then(RtValue::as_text))
                .collect::<Vec<_>>();
            panic!("the bar offers archiving the choice: {offered:?}")
        })
        .key
        .clone();
    window.fire(&archive, "press").expect("archive runs");
    window.poll_patches().expect("the window shows both gone");
    // The choice is put away once it has acted: the bar is the window's own again.
    assert!(
        window.tree().of_kind("ui.Badge").is_empty(),
        "the choice is put away once it has acted"
    );
    session.until("the server to archive both", |_| {
        servers.imap.messages("INBOX").is_empty()
            && servers.imap.messages("[Gmail]/All Mail").len() == 3
    });
    assert_eq!(
        session.threads().len(),
        2,
        "the conversations stay, out of the inbox"
    );
}

/// A conversation in the trash is deleted for good, from its server too, and the conversation
/// goes with its last message: what Delete permanently does, which the trash cannot take back.
#[test]
fn a_trashed_message_is_deleted_for_good_on_the_server() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    let plan = session.thread("Launch plan");
    session.call("delete", &plan, &json!({}));
    session.until("the server to put it in the trash", |_| {
        servers.imap.messages("INBOX").len() == 1
            && servers.imap.messages("[Gmail]/Trash").len() == 2
    });
    session.call("purge", &plan, &json!({}));
    // The deletion is asked of the server, and the server's trash is emptied of it: the record
    // leaves the workspace only once the server confirmed the expunge.
    session.until("the server to delete it for good", |_| {
        servers.imap.messages("[Gmail]/Trash").is_empty()
    });
    assert!(
        !session
            .threads()
            .iter()
            .any(|thread| session.text(thread, "subject") == "Launch plan"),
        "the conversation goes with its last message"
    );
    // The untouched conversation is still there, in the inbox.
    assert_eq!(session.threads().len(), 1);
    assert_eq!(servers.imap.messages("INBOX").len(), 1);
}

/// A message goes out as the person wrote it: from the identity they chose, with what it says
/// as the formatted alternative a mail program shows beside the words.
#[test]
fn a_message_goes_out_from_its_identity_as_formatted_mail() {
    let servers = servers();
    let config = AccountConfig {
        identities: vec!["Ada at work <ada@work.example>".into()],
        ..account(&servers)
    };
    let (_connector, mut session) = window_of(config);
    synced(&mut session);
    session.hold_for(0);
    // The account says what it sends as, as the settings keep it.
    let account = session.accounts()[0].clone();
    session.call("check_now", &account, &json!({}));
    session.until("the account to say what it sends as", |session| {
        session
            .text(&account, "identities")
            .contains("ada@work.example")
    });

    // An answer to a conversation the person read: its words, its quotation, and the identity it
    // goes from, which the person chose.
    let plan = session.thread("Launch plan");
    let draft = session.made("reply", &plan, &json!({}));
    session.call(
        "send_as",
        &draft,
        &json!({"identity": "Ada at work <ada@work.example>"}),
    );
    assert_eq!(
        session.text(&draft, "from_address"),
        "Ada at work <ada@work.example>"
    );
    session.call("send", &draft, &json!({}));
    session.until("the server to take it", |_| servers.smtp.sent().len() == 1);
    let sent = String::from_utf8_lossy(&servers.smtp.sent()[0]).into_owned();
    // The mail builder writes the name it was given in quotes: what it holds is what counts.
    let from = sent.lines().next().unwrap_or_default();
    assert!(
        from.starts_with("From: ")
            && from.contains("ada@work.example")
            && from.contains("Ada at work"),
        "it goes from the identity the person chose: {from}"
    );
    assert!(
        sent.contains("multipart/alternative") && sent.contains("text/html"),
        "the message carries its formatted alternative: {sent}"
    );
    assert!(
        sent.contains("<blockquote"),
        "the quotation it answers with is formatted: {sent}"
    );
    assert!(
        sent.contains("> On ") || sent.contains("On "),
        "the words it answers stay beside the formatting: {sent}"
    );
    assert!(
        !sent.contains("Bcc:"),
        "a blind recipient is never named in the message: {sent}"
    );
}

/// Whether the conversation `thread` is filed in the mailbox `key`.
fn in_box(session: &Session, thread: &ResourceRef, key: &str) -> bool {
    matches!(
        session.value_of(thread, "boxes"),
        RtValue::List(ref boxes) if boxes.contains(&RtValue::text(key))
    )
}

// USER REQUIREMENT: deleting a conversation moves it to the trash — it leaves the inbox, shows
// under Trash, and reaches the server — and moving it files it in the mailbox the person chose.
#[test]
fn deleting_a_conversation_trashes_it_and_moving_it_files_it_elsewhere() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    let account = session.accounts().remove(0);
    let dinner = session.thread("Dinner on Friday?");

    session.call("delete", &dinner, &json!({}));
    assert!(in_box(&session, &dinner, "trash"), "it is in the trash");
    assert!(!in_box(&session, &dinner, "inbox"), "and left the inbox");
    assert_eq!(
        session.place_count(&account, "Trash"),
        Some(1),
        "the Trash place lists it"
    );
    let shown_titles = session.titles();
    assert!(
        !shown_titles.iter().any(|title| title == "Bo Chen"),
        "the inbox's list no longer shows it: {shown_titles:?}"
    );
    session.until("the server to hold it in the trash", |_| {
        servers.imap.messages("INBOX").len() == 2
            && servers.imap.messages("[Gmail]/Trash").len() == 1
    });

    // Moving the other conversation to Junk takes both its messages out of the inbox, here and
    // on the server.
    let plan = session.thread("Launch plan");
    let junk = session.refs(&account, "mailboxes")[4].clone();
    assert_eq!(session.text(&junk, "key"), "junk");
    session.call(
        "move_to",
        &plan,
        &json!({ "to": {
            "app": APP,
            "kind": format!("tl.{APP}.Mailbox"),
            "id": junk.id_string(),
            "label": "Junk",
        } }),
    );
    assert!(in_box(&session, &plan, "junk"), "it is in Junk");
    assert!(!in_box(&session, &plan, "inbox"), "and left the inbox");
    assert_eq!(
        session.place_count(&account, "Junk"),
        Some(1),
        "the Junk place lists it"
    );
    session.until("the server to hold it in junk", |_| {
        servers.imap.messages("INBOX").is_empty()
            && servers.imap.messages("[Gmail]/Spam").len() == 2
    });
}

// USER REQUIREMENT: forwarding and replying to all make a draft that says what it answers, opens
// in the composer when the person asks to see it, and goes when they throw it away.
#[test]
fn forward_reply_all_open_and_discard_work_on_the_composers_draft() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    let plan = session.thread("Launch plan");

    let forwarded = session.made("forward", &plan, &json!({}));
    assert_eq!(session.text(&forwarded, "subject"), "Fwd: Launch plan");
    assert_eq!(
        session.text(&forwarded, "to"),
        "",
        "a forward is addressed by the person"
    );
    assert!(
        session
            .text(&forwarded, "body")
            .contains("> Agreed on the twelfth."),
        "it carries the message it sends on: {:?}",
        session.text(&forwarded, "body")
    );
    // Its `open` shows the composer in the window: the subject it sends on, where it goes from,
    // and the Send that would carry it, dimmed until it names someone.
    session.open_showing(&forwarded);
    let shown = session.shown();
    for said in [
        "Fwd: Launch plan",
        "sam@rivera.example",
        "It goes to nobody yet",
    ] {
        assert!(
            shown.iter().any(|text| text == said),
            "{said:?} in the composer: {shown:#?}"
        );
    }

    let answered = session.made("reply_all", &plan, &json!({}));
    assert_eq!(session.text(&answered, "to"), "ada@analytical.example");
    assert_eq!(
        session.text(&answered, "cc"),
        ME,
        "everyone the message went to answers"
    );
    assert_eq!(session.text(&answered, "subject"), "Re: Launch plan");

    // Its open shows it, and throwing it away takes it out of the window's drafts.
    session.open_showing(&answered);
    assert!(
        session.shown().iter().any(|text| text == "Re: Launch plan"),
        "the answered draft shows in the window"
    );
    let root = session.mail_root();
    assert_eq!(session.refs(&root, "drafts").len(), 2, "both drafts wait");
    session.call(
        "discard",
        &root,
        &json!({ "d": {
            "app": APP,
            "kind": format!("tl.{APP}.Draft"),
            "id": answered.id_string(),
            "label": "Re: Launch plan",
        } }),
    );
    let waiting = session.refs(&root, "drafts");
    assert_eq!(waiting, vec![forwarded.clone()], "only the forward waits");
}

// USER REQUIREMENT: a mailbox opens as its own page: the conversations filed in it, and nothing
// of another's.
#[test]
fn opening_a_mailbox_shows_the_conversations_filed_in_it() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    let account = session.accounts().remove(0);
    let dinner = session.thread("Dinner on Friday?");

    // The inbox holds both conversations.
    let inbox = session.refs(&account, "mailboxes")[0].clone();
    session.open_showing(&inbox);
    let rows = session.page_rows();
    assert_eq!(
        rows.len(),
        2,
        "the inbox shows its two conversations: {rows:#?}"
    );
    assert!(
        rows.iter()
            .any(|texts| texts.iter().any(|text| text == "Bo Chen")),
        "and names who wrote: {rows:#?}"
    );

    // The archive holds only the conversation moved into it.
    let archive = session.refs(&account, "mailboxes")[1].clone();
    session.call("archive", &dinner, &json!({}));
    session.open_showing(&archive);
    let rows = session.page_rows();
    assert_eq!(
        rows.len(),
        1,
        "the archive shows the one filed in it: {rows:#?}"
    );
    assert!(
        rows[0].iter().any(|text| text == "Bo Chen"),
        "the conversation filed in it: {rows:#?}"
    );
}

// USER REQUIREMENT: a conversation marked as junk leaves the inbox for the junk mailbox, and
// nothing else of the mailbox moves.
#[test]
fn a_conversation_marked_as_junk_goes_to_the_junk_mailbox() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    let account = session.accounts().remove(0);
    let dinner = session.thread("Dinner on Friday?");
    let junk = session.refs(&account, "mailboxes")[4].clone();

    session.call("junk", &dinner, &json!({}));
    session.open_showing(&junk);
    let rows = session.page_rows();
    assert_eq!(rows.len(), 1, "the junk shows the one marked: {rows:#?}");
    assert!(
        rows[0].iter().any(|text| text == "Bo Chen"),
        "the conversation marked as junk: {rows:#?}"
    );

    // The inbox keeps only what was not marked.
    let inbox = session.refs(&account, "mailboxes")[0].clone();
    session.open_showing(&inbox);
    assert_eq!(
        session.page_rows().len(),
        1,
        "the inbox keeps the conversation that was not marked"
    );
}

/// How many messages the account's mailbox `key` says it holds: what Mail counted there after
/// it read the folder from the server.
fn mailbox_total(session: &Session, account: &ResourceRef, key: &str) -> Option<i64> {
    let at = session
        .refs(account, "mailboxes")
        .into_iter()
        .find(|b| session.text(b, "key") == key)?;
    match session.value_of(&at, "total") {
        RtValue::Int(n) => Some(n),
        _ => None,
    }
}

/// What the first node of `kind` whose `label` is `label` says of the prop `name`.
fn labelled_prop<'a>(node: &'a UiNode, kind: &str, label: &str, name: &str) -> Option<&'a RtValue> {
    labelled(node, label)
        .into_iter()
        .find(|found| found.kind == kind)?
        .props
        .get(name)
}

// USER REQUIREMENT: mail the workspace does not hold, and mail in a folder it does not sync, is
// found by asking the account's server, and comes in as the account's own mail once found.
#[test]
fn the_servers_search_finds_mail_the_window_does_not_hold() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    // A message the window never fetched: it came to the archive after the last sync.
    servers.imap.append(
        "[Gmail]/All Mail",
        &mail(
            "needle@analytical.example",
            "Ada Lovelace <ada@analytical.example>",
            "Needle in the archive",
            "The number you asked for is 42.",
            None,
        ),
        &[],
    );
    assert!(
        session
            .threads()
            .iter()
            .all(|t| session.text(t, "subject") != "Needle in the archive"),
        "the window holds no such mail yet"
    );
    let account = session.accounts().remove(0);
    session.call(
        "search_server",
        &account,
        &json!({ "text": "asked for is 42" }),
    );
    session.until("the server's search to find it", |session| {
        session
            .threads()
            .iter()
            .any(|t| session.text(t, "subject") == "Needle in the archive")
    });
    let found = session.thread("Needle in the archive");
    assert!(
        matches!(session.value_of(&found, "boxes"), RtValue::List(ref boxes)
            if boxes.contains(&RtValue::text("archive"))),
        "it comes in where the server holds it"
    );
}

// USER REQUIREMENT: mail that comes while the window is open is announced to the people who
// read it, once, and the first sync of a mailbox is not news.
fn notices_held(session: &Session) -> Vec<ResourceRef> {
    session
        .instance_host()
        .store()
        .resources_of(tilolang::NOTICE_TYPE)
        .unwrap_or_default()
        .into_iter()
        .map(|id| ResourceRef::root(id, tilolang::NOTICE_TYPE.to_owned()))
        .collect()
}

#[test]
fn mail_that_comes_to_an_inbox_the_window_holds_is_announced_once() {
    let servers = servers();
    let connector = MailConnector::of(account(&servers));
    let mut session = connector.attach(Session::member(APP, "ada"));
    synced(&mut session);
    // Who an announcement reaches: the workspace's members.
    assert_eq!(
        session.instance_host().runtime().members().len(),
        1,
        "the workspace has its person as a member"
    );
    let account = session.accounts().remove(0);
    let at = tilolang::calendar::instant("2026-11-23T12:00:00Z").expect("a time");
    // What the window read of the mailbox first is not news: the sync tells nobody.
    assert!(
        notices_held(&session).is_empty(),
        "the first sync of a mailbox is not news"
    );

    // Mail that comes to an inbox the window holds is marked on its conversation with the
    // moment it came, and the app's announcement runs for it: on a live window the trigger runs
    // it, so this is the trigger's own work.
    servers.imap.append(
        "INBOX",
        &mail(
            "lunch@chen.example",
            "Bo Chen <bo@chen.example>",
            "Lunch on Monday?",
            "Shall we meet at noon?",
            None,
        ),
        &[],
    );
    session.call("check_now", &account, &json!({}));
    let lunch = session.wait_for("the conversation to come in", |session| {
        session
            .threads()
            .iter()
            .find(|t| session.text(t, "subject") == "Lunch on Monday?")
            .cloned()
    });
    // The notice is a record of the workspace, addressed to the person: every device of theirs
    // reads it, so the store holds it and it opens the conversation.
    let notices = session.wait_for("the mail to be announced", |session| {
        let held = notices_held(session);
        (held.len() == 1).then_some(held)
    });
    // The announcement cleared the moment it came, so it tells of it once.
    assert!(
        matches!(session.value_of(&lunch, "arrived"), RtValue::Opt(None)),
        "the arrival was announced and cleared"
    );
    assert_eq!(session.instance_host().run_triggers(at), 0, "it rings once");
    let shown = notices[0].clone();
    let opens = session
        .instance_host()
        .runtime()
        .read(&shown, tilolang::NOTICE_OPENS)
        .expect("the notice says what it opens");
    let RtValue::Ref(reference) = opens else {
        panic!("the notice opens the conversation: {opens:?}");
    };
    assert_eq!(
        reference.target.id_string(),
        lunch.id_string(),
        "it opens the conversation it tells of"
    );

    // Mail that comes next is marked and announced the same way.
    servers.imap.append(
        "INBOX",
        &mail(
            "supper@chen.example",
            "Bo Chen <bo@chen.example>",
            "Supper on Saturday?",
            "Are you free at seven?",
            None,
        ),
        &[],
    );
    session.call("check_now", &account, &json!({}));
    let supper = session.wait_for("the mail that comes next to come in", |session| {
        session
            .threads()
            .iter()
            .find(|t| session.text(t, "subject") == "Supper on Saturday?")
            .cloned()
    });
    session.wait_for("the mail that comes next to be announced", |session| {
        (notices_held(session).len() == 2).then_some(())
    });
    assert!(
        matches!(session.value_of(&supper, "arrived"), RtValue::Opt(None)),
        "the next arrival was announced and cleared too"
    );
    // What the window holds is no news again.
    session.call("check_now", &account, &json!({}));
    for _ in 0..10 {
        session.settle();
    }
    assert_eq!(
        notices_held(&session).len(),
        2,
        "each is told once, and what is held again is no news"
    );
    assert_eq!(
        session.instance_host().run_triggers(at),
        0,
        "each rings once"
    );
}

// USER REQUIREMENT: a draft typed here is kept in the account's server Drafts folder, so another
// mail client sees it, and the drafts another client keeps are read here.
#[test]
fn a_draft_is_kept_in_the_servers_drafts_and_theirs_are_read_here() {
    let servers = empty_servers();
    // A draft another mail client of the account kept on the server.
    servers.imap.append(
        "[Gmail]/Drafts",
        &mail(
            "elsewhere@rivera.example",
            "Sam Rivera <sam@rivera.example>",
            "Draft from elsewhere",
            "Half written.",
            None,
        ),
        &["\\Seen", "\\Draft"],
    );
    let (_connector, mut session) = window_of(account(&servers));
    session.until("the server's drafts to come in", |session| {
        session
            .threads()
            .iter()
            .any(|t| session.text(t, "subject") == "Draft from elsewhere")
    });
    let theirs = session.thread("Draft from elsewhere");
    assert!(
        matches!(session.value_of(&theirs, "boxes"), RtValue::List(ref boxes)
            if boxes.contains(&RtValue::text("drafts"))),
        "it is filed in the drafts"
    );
    assert!(
        !session.flag(&theirs, "unread"),
        "a draft is not unread mail"
    );

    // A message written here is kept in the server's Drafts folder too.
    let root = session.mail_root();
    let invitation = "BEGIN:VCALENDAR\nMETHOD:REQUEST\nBEGIN:VEVENT\nUID:plan@rivera.example\n\
END:VEVENT\nEND:VCALENDAR\n";
    let draft = session.made(
        "prepare_calendar_invitation",
        &root,
        &json!({
            "to": "ada@analytical.example",
            "subject": "Launch on the twelfth",
            "body": "Join us on the twelfth.",
            "calendar_ics": invitation,
        }),
    );
    let draft_ref = json!({
        "app": APP,
        "kind": "tl.so.tilo.mail.Draft",
        "id": draft.id_string(),
        "label": "Launch on the twelfth",
    });
    session.call("save_to_drafts", &root, &json!({ "d": draft_ref }));
    session.until("the server to keep the draft", |_| {
        servers.imap.messages("[Gmail]/Drafts").len() == 2
    });
    let kept = servers.imap.messages("[Gmail]/Drafts");
    let written = String::from_utf8_lossy(&kept[1].raw).into_owned();
    assert!(
        written.contains("Subject: Launch on the twelfth"),
        "the server's draft carries what was written: {written}"
    );
    assert!(
        kept[1].flags.iter().any(|flag| flag == "\\Draft"),
        "it is a draft on the server: {:?}",
        kept[1].flags
    );
}

// USER REQUIREMENT: deleting a conversation in the trash permanently takes it out of the
// mailbox and off the server, behind a confirmation, and leaves the rest of the mail.
#[test]
fn a_trashed_conversation_is_deleted_permanently_here_and_on_the_server() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    let dinner = session.thread("Dinner on Friday?");
    session.call("delete", &dinner, &json!({}));
    // The conversation's window offers the deletion behind a confirmation, and opening it
    // deletes nothing.
    let view = session
        .instance_host()
        .open_resource(&dinner, None)
        .expect("the conversation opens");
    let mut shown = Vec::new();
    texts_in(view.tree(), &mut shown);
    let banner = find_kind(view.tree(), "ui.Banner", "title", "It is in the trash")
        .unwrap_or_else(|| panic!("the window says it is in the trash: {shown:#?}"));
    assert_eq!(
        banner.props.get("action"),
        Some(&RtValue::Text("Delete permanently".to_owned())),
        "the offer deletes it permanently"
    );
    // The server puts it in its trash, and Mail read it there: the account counts it in the
    // trash, so the place the server gave it is the one Mail deletes it from.
    let account = session.accounts().remove(0);
    session.until("the server to hold it in the trash", |_| {
        servers.imap.messages("[Gmail]/Trash").len() == 1
    });
    session.until("Mail to read the trash", |session| {
        mailbox_total(session, &account, "trash") == Some(1)
    });

    session.call("purge", &dinner, &json!({}));
    session.until("the conversation to leave the workspace", |session| {
        session
            .threads()
            .iter()
            .all(|t| session.text(t, "subject") != "Dinner on Friday?")
    });
    assert!(
        servers.imap.messages("[Gmail]/Trash").is_empty(),
        "the server's trash no longer holds it: the record leaves only after the server \
         confirmed the expunge"
    );
    assert_eq!(session.threads().len(), 1, "the rest of the mail stays");

    // Emptying the trash offers the same behind a confirmation, and takes every conversation
    // in it.
    let plan = session.thread("Launch plan");
    session.call("delete", &plan, &json!({}));
    session.until("the server to hold the conversation in the trash", |_| {
        servers.imap.messages("[Gmail]/Trash").len() == 2
    });
    session.until("Mail to read the trash again", |session| {
        mailbox_total(session, &account, "trash") == Some(2)
    });
    let mut view = session
        .instance_host()
        .open_resource(&account, None)
        .expect("the account's window opens");
    let trash = find_kind(view.tree(), "ui.NavItem", "label", "Trash")
        .expect("the window offers the trash")
        .key
        .clone();
    view.fire(&trash, "select").expect("the trash opens");
    view.poll_patches().expect("the trash shows");
    let banner = find_kind(view.tree(), "ui.Banner", "title", "Empty the trash")
        .unwrap_or_else(|| panic!("the trash place offers to empty it"));
    assert_eq!(
        banner.props.get("action"),
        Some(&RtValue::Text("Empty trash".to_owned())),
        "the offer empties it"
    );
    session.call("empty_trash", &account, &json!({}));
    session.until("the trash to be empty", |session| {
        session.threads().is_empty()
    });
    assert!(
        servers.imap.messages("[Gmail]/Trash").is_empty(),
        "the server's trash is emptied too, before the records leave"
    );
}

// USER REQUIREMENT: an account Mail has no password for says so in its own words, offers to ask
// again, and leads to the Settings that hold the password.
#[test]
fn an_account_with_no_password_says_so_and_leads_to_settings() {
    let servers = servers();
    let mut config = account(&servers);
    config.password.clear();
    let (_connector, mut session) = window_of(config);
    let account = session.wait_for("the account", |session| {
        session.accounts().into_iter().next()
    });
    let said = session.wait_for("the window to say the password is missing", |session| {
        let said = session.view_of(&account);
        said.iter()
            .any(|text| text.contains("no password"))
            .then_some(said)
    });
    for wanted in [
        "Mail has no password for sam@rivera.example",
        "Nothing was sent to its server. Enter its password in Settings, under Accounts.",
        "Open account settings",
    ] {
        assert!(
            said.iter().any(|text| text == wanted),
            "{wanted:?} in {said:#?}"
        );
    }
    // The notice offers to ask the server again.
    let window = session
        .instance_host()
        .open_resource(&account, None)
        .expect("the account's window opens");
    let banner = find_kind(
        window.tree(),
        "ui.Banner",
        "title",
        "Mail has no password for sam@rivera.example",
    )
    .unwrap_or_else(|| panic!("the window says the password is missing: {said:#?}"));
    assert_eq!(
        banner.props.get("action"),
        Some(&RtValue::Text("Try again".to_owned())),
        "the notice offers to ask again"
    );
    assert_eq!(session.text(&account, "trouble_kind"), "no-password");
    assert!(!session.flag(&account, "online"));
}

// USER REQUIREMENT: what waits to go has a place of its own, showing how each message stands,
// with the press that takes it back or tries a refused one again.
#[test]
fn the_outbox_shows_what_waits_to_go_and_takes_a_message_back() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    session.hold_for(3600);
    let draft = session.sent_reply("Launch plan");
    assert_eq!(session.delivery_of(&draft).unwrap().0, "held");

    // The window offers the outbox, counting the message that waits.
    let root = session.mail_root();
    let mut view = session
        .instance_host()
        .open_resource(&root, None)
        .expect("the mail window opens");
    assert_eq!(
        labelled_prop(view.tree(), "ui.NavItem", "Outbox", "count"),
        Some(&RtValue::Int(1)),
        "it counts what waits",
    );
    // The place lists the message that waits: choosing it in the window lists the draft's row.
    let (x, y) = super::shown_at(&mut session, "Outbox");
    session.window.click_at(x, y);
    session.settle();
    assert!(
        session
            .titles()
            .iter()
            .any(|title| title == "Re: Launch plan"),
        "the outbox lists the waiting message: {:?}",
        session.titles()
    );
    // A message that did not fail is not tried again: the server knows how it went.
    assert!(
        session.try_call("retry", &draft, &json!({})).is_err(),
        "nothing failed, so nothing is tried again"
    );

    // What the row offers takes it back: it never goes, and it is a draft again.
    session.call("undo_send", &draft, &json!({}));
    assert!(!session.flag(&draft, "sent"), "it is a draft again");
    assert!(session.flag(&draft, "recalled"));
    for _ in 0..20 {
        session.settle();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(servers.smtp.sent().is_empty(), "it never went");
    // Nothing waits now: the place says so, and the window offers no outbox to open.
    view.poll_patches().expect("the window shows");
    let mut after = Vec::new();
    texts_in(view.tree(), &mut after);
    assert!(
        after
            .iter()
            .any(|text| text == "Nothing is waiting to be sent"),
        "the outbox is empty: {after:#?}"
    );
}

/// A message the server kept from last winter, read already: what a date narrows conversations by.
fn old_mail() -> String {
    format!(
        "From: Old Sender <old@example.org>\nTo: {ME}\nSubject: Last winter's news\n\
Message-ID: <old@example.org>\nDate: Mon, 1 Dec 2025 09:00:00 +0000\n\nAn old note.\n"
    )
}

// USER REQUIREMENT: search finds mail by who sent it, what it is about and when it came:
// sender and subject as words, a date as a filter, through the same index and query the
// system's search reads.
#[test]
fn search_finds_mail_by_sender_subject_and_date() {
    let servers = servers();
    servers.imap.append("INBOX", &old_mail(), &["\\Seen"]);
    let (_connector, mut session) = window_of(account(&servers));
    session.until("the old message to come in with the rest", |session| {
        session.threads().len() == 3
    });

    // The words a person types: who sent it and what it is about are both in the message's text.
    let host = session.instance_host();
    let hits = tilolang_tilo::search::every_hit(&host);
    let plan = hits
        .iter()
        .find(|hit| hit.type_name == "Message" && hit.title == "Launch plan")
        .unwrap_or_else(|| panic!("the plan message is indexed: {hits:?}"));
    assert!(
        plan.texts.iter().any(|text| text.contains("Grace Hopper")),
        "its sender is searchable: {:?}",
        plan.texts
    );
    assert!(
        plan.texts
            .iter()
            .any(|text| text.contains("Here is the draft plan.")),
        "its words are searchable: {:?}",
        plan.texts
    );

    // A date, as `latest:>2026-06-01` or `latest:-7d` types it: the conversations of that span, by
    // what each one's last message came in. The account's window narrows its list in place, as a
    // query typed in search and scoped to the window does.
    let account = session.accounts().remove(0);
    let mut window = session
        .instance_host()
        .open_resource(&account, None)
        .expect("the account's window opens");
    let list = window
        .tree()
        .of_kind("ui.VirtualList")
        .first()
        .map(|node| node.key.clone())
        .expect("the window lists its conversations");
    assert_eq!(
        window.list_len(&list),
        Some(3),
        "the inbox lists every conversation"
    );
    let package = session
        .instance_host()
        .package(APP)
        .expect("the mail package");
    let query_state = package
        .meta
        .views
        .iter()
        .find(|view| view.name == "mailboxes")
        .and_then(|view| view.state.iter().find(|state| state.name == "query"))
        .expect("the account's window keeps the place it shows")
        .id
        .clone();
    // The conversations that came after the oldest of them: a date, as `latest:-7d` or
    // `latest:>2026-06-01` types it, narrows the list where it stands.
    let mut came: Vec<String> = session
        .threads()
        .iter()
        .map(|thread| session.text(thread, "latest"))
        .collect();
    came.sort();
    let after = format!("latest:>{}", came[0]);
    window.set_view_state(&query_state, RtValue::text(after.as_str()));
    window.poll_patches().expect("the window shows the span");
    assert_eq!(
        window.list_len(&list),
        Some(2),
        "the conversations that came after {} ({after})",
        came[0]
    );
}

// USER REQUIREMENT: an agent drafts a message as a typed action; the person finds it in Drafts
// and sends it, answering what it answers.
#[test]
fn an_agent_drafts_a_message_the_person_sends() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    let root = session.mail_root();
    let resource = tilolang_tilo::publish::resource_id(APP, &root.id_string());
    let (agent, ambient) = super::agent_session();
    let arguments = json!({
        "to": "bo@chen.example",
        "subject": "Re: Dinner on Friday?",
        "body": "Friday works for me.",
        "in_reply_to": "dinner@chen.example",
    });
    let answered = session
        .invoke(
            Some(agent),
            Some(&ambient),
            &resource,
            "draft_message",
            arguments.as_object().expect("an object"),
        )
        .expect("an agent drafts a message");
    let made = answered
        .resources
        .first()
        .unwrap_or_else(|| panic!("it answers the draft it made: {answered:?}"));
    let draft = ResourceRef::parse_id(
        made.strip_prefix(&format!("tl:{APP}:"))
            .expect("a draft of Mail"),
        session.type_id("Draft"),
    )
    .expect("a draft");
    assert_eq!(session.text(&draft, "to"), "bo@chen.example");
    assert_eq!(session.text(&draft, "subject"), "Re: Dinner on Friday?");
    assert_eq!(session.text(&draft, "body"), "Friday works for me.");
    assert_eq!(session.text(&draft, "in_reply_to"), "dinner@chen.example");
    assert!(
        !session.flag(&draft, "sent"),
        "it waits for the person to send it"
    );
    assert!(
        session.refs(&root, "drafts").contains(&draft),
        "the person finds it in Drafts"
    );

    // The person sends it, and it goes out answering the message it names.
    session.hold_for(0);
    session.call("send", &draft, &json!({}));
    session.until("the server to take it", |_| servers.smtp.sent().len() == 1);
    let sent = String::from_utf8_lossy(&servers.smtp.sent()[0]).into_owned();
    assert!(sent.contains("bo@chen.example"), "{sent}");
    assert!(sent.contains("Subject: Re: Dinner on Friday?"), "{sent}");
    assert!(
        sent.contains("In-Reply-To: <dinner@chen.example>"),
        "{sent}"
    );
    assert!(sent.contains("Friday works for me."), "{sent}");
}

// USER REQUIREMENT: a person reads a message before opening its conversation, and that marks
// nothing read: neither here nor on the server.
#[test]
fn a_message_previews_without_being_read() {
    let servers = servers();
    let (_connector, mut session) = window_of(account(&servers));
    synced(&mut session);
    let plan = session.thread("Launch plan");
    assert!(session.flag(&plan, "unread"));
    // The list offers the newest message as a preview while the row is under the pointer.
    let senders = session.text(&plan, "senders");
    let (x, y) = session.wait_for("the conversation's row", |session| {
        let mut found = Vec::new();
        containing(session.window.root(), &senders, &mut found);
        found.last().map(|rect| (rect.center().x, rect.center().y))
    });
    session.window.move_to(x, y);
    session.settle();
    let (preview_x, preview_y) = session.wait_for("its Preview to be offered", |session| {
        let mut found = Vec::new();
        containing(session.window.root(), "Preview", &mut found);
        found.pop().map(|rect| (rect.center().x, rect.center().y))
    });
    session.window.click_at(preview_x, preview_y);
    session.settle();
    let tree = {
        let window = session.hosted();
        window
            .view()
            .expect("the window is connected")
            .detail_tree()
            .expect("the preview opens beside the list")
    };
    let mut texts = Vec::new();
    super::texts(&tree, &mut texts);
    assert!(
        texts.iter().any(|text| text == "Ada Lovelace"),
        "the preview says who wrote the newest message: {texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|text| text.contains("Agreed on the twelfth.")),
        "the preview says what it says: {texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|text| text == "It stays unread until its conversation is opened."),
        "it says it is still unread: {texts:?}"
    );
    assert!(
        session.flag(&plan, "unread"),
        "the preview marks nothing read"
    );
    session.settle();
    assert_eq!(
        servers
            .imap
            .messages("INBOX")
            .iter()
            .filter(|message| message.flags.iter().any(|flag| flag == "\\Seen"))
            .count(),
        1,
        "nothing told the server it was read"
    );
}

/// The session's tools, as a test serves them: it answers every request as accepted and keeps
/// what it was asked, so a test reads the opens an action asked for.
struct Tools {
    asked: std::sync::Arc<std::sync::Mutex<Vec<runtime_portal::tools::ToolIntent>>>,
}

impl Tools {
    /// The session's end of a tools connection, and the stub answering the other end.
    fn serving() -> (std::sync::Arc<runtime_portal::tools::Service>, Self) {
        let stub = Self {
            asked: std::sync::Arc::default(),
        };
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("the tools connection");
        stub.serve(ours);
        let service =
            runtime_portal::tools::Service::connect(theirs, "the window's workspace".to_owned());
        (std::sync::Arc::new(service), stub)
    }

    fn serve(&self, stream: std::os::unix::net::UnixStream) {
        let asked = std::sync::Arc::clone(&self.asked);
        std::thread::spawn(move || {
            use std::io::{BufRead as _, BufReader, Write as _};
            let Ok(mut writing) = stream.try_clone() else {
                return;
            };
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { return };
                let Ok(request) = serde_json::from_str::<runtime_portal::tools::ToolRequest>(&line)
                else {
                    return;
                };
                asked
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(request.intent);
                let reply = runtime_portal::tools::ToolReply {
                    accepted: true,
                    message: "opened".to_owned(),
                    ..runtime_portal::tools::ToolReply::default()
                };
                let Ok(mut encoded) = serde_json::to_vec(&reply) else {
                    return;
                };
                encoded.push(b'\n');
                if writing.write_all(&encoded).is_err() {
                    return;
                }
            }
        });
    }

    /// What it was asked, oldest first.
    fn asked(&self) -> Vec<runtime_portal::tools::ToolIntent> {
        self.asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

// USER REQUIREMENT: a message's links are its own: a person reads every link it holds once, in
// order, opens one in the app that opens links of its scheme, and copies one; an agent asks the
// message what it links to.
#[test]
fn a_messages_links_are_listed_and_open_through_the_shell() {
    let servers = empty_servers();
    servers.imap.append(
        "INBOX",
        &mail_formatted(
            "plan@letter.example",
            "Ada Lovelace <ada@analytical.example>",
            "The launch plan",
            "The plan is at https://tilo.sh/plan, and the notes at https://tilo.sh/notes.",
            "<p>The <a href=\"https://tilo.sh/plan\">plan</a> is ready, and the \
             <a href=\"https://tilo.sh/plan\">plan</a> again.</p>\
             <p>Write to <a href=\"mailto:ada@analytical.example\">Ada</a>.</p>\
             <p>Nothing from <a href=\"javascript:alert(1)\">this</a>.</p>",
        ),
        &["\\Seen"],
    );
    let (tools, stub) = Tools::serving();
    Session::opening_files_through(&tools);
    let (_connector, mut session) = window_of(account(&servers));
    session.until("the message to arrive", |session| {
        session.threads().len() == 1
            && matches!(session.value_of(&session.threads()[0], "boxes"), RtValue::List(boxes)
                if boxes.contains(&RtValue::text("inbox")))
    });
    let plan = session.thread("The launch plan");
    let message = session.refs(&plan, "messages")[0].clone();

    // What the message links to: each address once, in the order it says them, and only those a
    // mail client opens.
    let answered = session.call("links", &message, &json!({}));
    assert_eq!(
        answered.value,
        Some(json!([
            "https://tilo.sh/plan",
            "mailto:ada@analytical.example",
        ])),
        "the message says its links"
    );

    // The conversation shows them, each a link to press and a press that copies it.
    let texts = session.conversation(&plan);
    assert!(
        texts.iter().any(|text| text == "2 links in this message"),
        "the message counts its links: {texts:?}"
    );
    assert!(
        texts.iter().any(|text| text == "https://tilo.sh/plan"),
        "the address it names stands as a link: {texts:?}"
    );
    assert!(
        texts.iter().any(|text| text == "Copy link"),
        "and is copied on its own: {texts:?}"
    );

    // Opening one asks the shell for the app that opens links of its scheme.
    session.call(
        "open_link",
        &message,
        &json!({ "url": "https://tilo.sh/plan" }),
    );
    assert_eq!(
        stub.asked(),
        [runtime_portal::tools::ToolIntent::OpenLink {
            url: "https://tilo.sh/plan".to_owned(),
        }],
        "the shell opens the address"
    );

    // A link the message does not say is not one it opens.
    let refused = session.try_call(
        "open_link",
        &message,
        &json!({ "url": "https://evil.example/" }),
    );
    assert!(
        refused
            .err()
            .is_some_and(|why| why.contains("no such link")),
        "a message opens only a link it says"
    );
}

// USER REQUIREMENT: a sender whose pictures are always shown says so in their message, the
// window lists the trusted senders, and asking again is a press away: what a person allowed
// once stays theirs to take back.
#[test]
fn a_trusted_sender_is_listed_and_can_be_asked_again() {
    let servers = empty_servers();
    servers.imap.append(
        "INBOX",
        &mail_formatted(
            "postcard@letter.example",
            "Ada Lovelace <ada@analytical.example>",
            "A postcard",
            "The lighthouse, as promised.",
            "<p>The lighthouse, as promised.</p>\
             <p><img src=\"https://pictures.example/lighthouse.png\" alt=\"the lighthouse\"></p>",
        ),
        &["\\Seen"],
    );
    let (_connector, mut session) = window_of(account(&servers));
    session.until("the message to arrive", |session| {
        session.threads().len() == 1
            && matches!(session.value_of(&session.threads()[0], "boxes"), RtValue::List(boxes)
                if boxes.contains(&RtValue::text("inbox")))
    });
    let postcard = session.thread("A postcard");
    let root = session.mail_root();
    session.call(
        "trust_sender",
        &root,
        &json!({ "address": "Ada@Analytical.example" }),
    );
    let trusted = session.refs(&root, "trusted");
    assert_eq!(trusted.len(), 1, "the sender is trusted once");
    assert_eq!(
        session.text(&trusted[0], "address"),
        "ada@analytical.example",
        "by their address, however it was written"
    );

    // The message says its sender's pictures are always shown, and offers to ask again.
    let texts = session.conversation(&postcard);
    assert!(
        texts
            .iter()
            .any(|text| text == "Pictures from ada@analytical.example are always shown."),
        "the message says so: {texts:?}"
    );
    assert!(
        texts.iter().any(|text| text == "Ask again"),
        "and offers the press that takes it back: {texts:?}"
    );

    // The window lists the trusted senders, each with that press.
    let mut view = session
        .instance_host()
        .open_resource(&root, None)
        .expect("the mail window opens");
    let place = find_kind(view.tree(), "ui.NavItem", "label", "Trusted senders")
        .expect("the sidebar offers the trusted senders")
        .key
        .clone();
    view.fire(&place, "select").expect("the place opens");
    view.poll_patches().expect("the window shows the place");
    let mut listed = Vec::new();
    texts_in(view.tree(), &mut listed);
    assert!(
        listed.iter().any(|text| text == "ada@analytical.example"),
        "the sender is listed: {listed:?}"
    );
    assert!(
        listed.iter().any(|text| text == "Ask again"),
        "with the press that asks again: {listed:?}"
    );
    assert_eq!(count_in(view.tree(), "ui.NavShell"), Some(1), "one sender");

    // Asking again shows nothing until the person asks for it.
    session.call(
        "distrust_sender",
        &root,
        &json!({ "address": "ada@analytical.example" }),
    );
    assert!(
        session.refs(&root, "trusted").is_empty(),
        "the sender is no longer trusted"
    );
    let texts = session.conversation(&postcard);
    assert!(
        !texts.iter().any(|text| text.contains("are always shown")),
        "and the message no longer says it: {texts:?}"
    );
}
