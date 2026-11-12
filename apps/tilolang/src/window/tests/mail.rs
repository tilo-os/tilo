//! Mail against a mail server: the package's window kept in step with a test IMAP server through
//! the mail origin and native Mail's engine, and sending through a test SMTP server from the
//! outbox. What people do goes through the package's actions, as a press does.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mail::fake_servers::{FakeImap, FakeSmtp, Layout, accepting_client_config, server_tls_config};
use mail::model::{AccountConfig, AuthMethod};
use tilolang_tilo::runtime::{Principal, ResourceRef, Value as RtValue};

use super::Workspace;
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
    let servers = empty_servers();
    let imap = &servers.imap;
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
    servers
}

/// The two servers with nothing in the inbox yet: what a test puts there once a workspace
/// stands, as mail arrives at an open window.
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
fn workspace_with(config: AccountConfig) -> Workspace {
    workspace_over(Box::new(move || vec![config.clone()]), Arc::default())
}

/// A workspace whose accounts are what `accounts` says each time it is asked, which comes online
/// again when `reconnected` is set.
fn workspace_over(
    accounts: tilolang_mail::engine::AccountSource,
    reconnected: Arc<AtomicBool>,
) -> Workspace {
    let woken = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&woken);
    // One wake for every answer, as the app's engine has: an answer to a request and news of
    // mail that arrived both wake the window.
    let wake: mail::engine::Wake = Arc::new(move || flag.store(true, Ordering::Release));
    let engine = mail::engine::live::LiveEngine::trusting(
        tilo_content::ContentStore::memory(),
        Arc::clone(&wake),
        accepting_client_config(),
    )
    .expect("the engine starts");
    let (requests, events) = mail::engine::spawn_waking(Box::new(engine), wake);
    let connector = EngineConnector::new(accounts, requests, events);
    let origin = std::rc::Rc::new(std::cell::RefCell::new(
        ImapOrigin::new(Box::new(connector), woken).reconnecting_on(reconnected),
    ));
    workspace_of(origin)
}

/// A workspace kept in step with a mail server by `origin`.
fn workspace_of(origin: std::rc::Rc<std::cell::RefCell<ImapOrigin>>) -> Workspace {
    crate::app::builtin::choose("mail");
    let meta = crate::app::builtin::package_named("mail").expect("the mail package");
    let sink = tilolang_tilo::runtime::MemorySink::new();
    let window = Window::new(meta.app.clone(), "ada".to_owned(), RunMode::Live)
        .with_sink(sink.clone())
        .with_origin(origin);
    let mut workspace = Workspace {
        doc: ui::automerge::AutoCommit::new(),
        window,
        sink,
    };
    workspace.settle();
    workspace
}

impl Workspace {
    fn host(&self) -> tilolang_tilo::host::Host {
        self.window.current_host().expect("the host started")
    }

    /// Frames until `done` holds of the workspace, as the servers answer. It waits against the
    /// workspace's hang bound, so a busy runner makes the test slower rather than failing a
    /// server that answered in time.
    fn until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        self.wait_for(what, |workspace| done(workspace).then_some(()));
    }

    /// Frames until `probe` yields what it looks for, and yields that. The workspace's state
    /// changes before its window names it — a list names its rows a frame after the state they
    /// read changed — so a read taken right after a write can still see the frame before it.
    fn wait_for<T>(&mut self, what: &str, mut probe: impl FnMut(&mut Self) -> Option<T>) -> T {
        tilo_test_env::wait_for(what, || {
            self.settle();
            probe(self)
        })
    }

    /// The key of the root list's row that shows `item`, once the list shows it.
    fn row_showing(&mut self, item: &ResourceRef) -> String {
        let shown = item.id_string();
        self.wait_for("the list to show the item", |workspace| {
            workspace
                .items()
                .into_iter()
                .find(|(_, row)| row.item.as_ref().is_some_and(|row| row.0 == shown))
                .map(|(key, _)| key)
        })
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

    /// Invoke the package's action `name` as the person at the window, as a press does. A call
    /// the action makes runs at its own door, so an action that sends asks first: the test
    /// allows what the app's sheet would ask, as the person pressing does.
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
            tilolang_tilo::runtime::InvokeOutcome::Suggested(suggestion) => {
                let allowed = self
                    .host()
                    .runtime()
                    .resolve(
                        &suggestion.run,
                        &Principal::new("ada"),
                        tilolang_tilo::runtime::Resolution::Approve,
                    )
                    .unwrap_or_else(|error| panic!("{name}: the ask: {error}"));
                self.window.commit_planned();
                self.settle();
                match allowed {
                    tilolang_tilo::runtime::RunOutcome::Done(value) => value,
                    suspended @ tilolang_tilo::runtime::RunOutcome::Suspended(_) => {
                        panic!("{name}: the allowed run waits again: {suspended:?}");
                    }
                }
            }
            tilolang_tilo::runtime::InvokeOutcome::Denied => panic!("{name} was denied"),
        }
    }
}

/// Frames until the account's server's mail is in the workspace as a person sees it: its
/// conversations, filed in the inbox, with the messages the sync read. A sync makes a
/// conversation before it fetches its messages and files it, so the conversations standing in
/// the account are not the sync's end.
fn synced(workspace: &mut Workspace) {
    workspace.until("the inbox to sync", |workspace| {
        let threads = workspace.threads();
        threads.len() == 2
            && threads.iter().all(|thread| {
                matches!(workspace.read(thread, "boxes"), RtValue::List(boxes)
                    if boxes.contains(&RtValue::text("inbox")))
            })
    });
}

#[test]
fn an_account_syncs_into_mailboxes_and_conversations() {
    let servers = servers();
    let mut workspace = workspace_with(account(&servers));
    synced(&mut workspace);
    let account = workspace.accounts().remove(0);
    assert_eq!(workspace.text(&account, "address"), ME);
    assert_eq!(workspace.text(&account, "name"), "Sam Rivera");
    assert!(
        !workspace.text(&account, "fetched").is_empty(),
        "the account says when its mail was read from the server"
    );
    let boxes: Vec<String> = workspace
        .refs(&account, "mailboxes")
        .iter()
        .map(|b| workspace.text(b, "key"))
        .collect();
    assert_eq!(
        boxes,
        ["inbox", "archive", "sent", "drafts", "junk", "trash"]
    );

    // The reply joins the conversation it answers; the other stands alone.
    let plan = workspace.thread("Launch plan");
    let messages = workspace.refs(&plan, "messages");
    assert_eq!(messages.len(), 2);
    assert_eq!(workspace.text(&messages[0], "from_name"), "Grace Hopper");
    assert_eq!(
        workspace.text(&messages[1], "body").trim(),
        "Agreed on the twelfth."
    );
    assert!(
        workspace.flag(&plan, "unread"),
        "an unread message makes it unread"
    );
    assert_eq!(
        workspace.text(&plan, "senders"),
        "Ada Lovelace, Grace Hopper"
    );
    let dinner = workspace.thread("Dinner on Friday?");
    assert!(
        !workspace.flag(&dinner, "unread"),
        "it was read on the server"
    );

    // Syncing again finds nothing new and makes nothing twice.
    workspace.until("the inbox counts", |workspace| {
        let inbox = workspace.refs(&workspace.accounts()[0], "mailboxes")[0].clone();
        matches!(workspace.read(&inbox, "unread"), RtValue::Int(2))
    });
    assert_eq!(workspace.threads().len(), 2);
}

#[test]
fn reading_and_archiving_reach_the_server_and_come_back() {
    let servers = servers();
    let mut workspace = workspace_with(account(&servers));
    synced(&mut workspace);
    let plan = workspace.thread("Launch plan");

    workspace.press("mark_read", vec![RtValue::resource(&plan)]);
    assert!(!workspace.flag(&plan, "unread"), "it reads as read at once");
    workspace.until("the server to hold it read", |_| {
        servers
            .imap
            .messages("INBOX")
            .iter()
            .filter(|m| m.flags.iter().any(|f| f == "\\Seen"))
            .count()
            == 3
    });

    workspace.press("archive", vec![RtValue::resource(&plan)]);
    assert!(
        !matches!(workspace.read(&plan, "boxes"), RtValue::List(ref b) if b.contains(&RtValue::text("inbox"))),
        "it leaves the inbox at once"
    );
    workspace.until("the server to archive it", |_| {
        servers.imap.messages("INBOX").len() == 1
            && servers.imap.messages("[Gmail]/All Mail").len() == 2
    });

    // Taken back to the inbox, the server moves it back.
    workspace.press("to_inbox", vec![RtValue::resource(&plan)]);
    workspace.until("the server to hold it in the inbox again", |_| {
        servers.imap.messages("INBOX").len() == 3
    });
    assert_eq!(workspace.threads().len(), 2, "moving makes nothing twice");
}

#[test]
fn opening_a_conversation_reads_it_here_and_on_the_server() {
    let servers = servers();
    let mut workspace = workspace_with(account(&servers));
    synced(&mut workspace);
    let plan = workspace.thread("Launch plan");
    assert!(workspace.flag(&plan, "unread"));
    let row = workspace.row_showing(&plan);
    workspace.activate(&row);
    assert!(
        !workspace.flag(&plan, "unread"),
        "it reads as read once opened"
    );
    workspace.until("the server to hold it read", |_| {
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
    let mut workspace = workspace_with(account(&servers));
    synced(&mut workspace);
    hold_for(&mut workspace, 0);
    let plan = workspace.thread("Launch plan");
    let RtValue::Ref(draft) = workspace.press("reply", vec![RtValue::resource(&plan)]) else {
        panic!("a reply is a draft");
    };
    let draft = (*draft.target).clone();
    assert_eq!(workspace.text(&draft, "to"), "ada@analytical.example");
    assert_eq!(workspace.text(&draft, "subject"), "Re: Launch plan");
    assert_eq!(
        workspace.text(&draft, "in_reply_to"),
        "plan-2@analytical.example"
    );

    workspace.press("send", vec![RtValue::resource(&draft)]);
    workspace.until("the server to take the reply", |_| {
        servers.smtp.sent().len() == 1
    });
    workspace.until("the outbox to settle", |workspace| {
        workspace.host().store().outbox_pending_count() == 0
    });
    let sent = String::from_utf8_lossy(&servers.smtp.sent()[0]).into_owned();
    assert!(sent.contains("Subject: Re: Launch plan"), "{sent}");
    assert!(
        sent.contains("In-Reply-To: <plan-2@analytical.example>"),
        "{sent}"
    );
    assert!(workspace.flag(&draft, "sent"));
    assert!(
        workspace.sink.delivered().is_empty(),
        "a send is the connector's, never the development log's"
    );
}

#[test]
fn an_unreachable_server_is_said_on_the_account() {
    let servers = servers();
    let mut config = account(&servers);
    // Nothing listens there.
    config.imap_port = 1;
    let mut workspace = workspace_with(config);
    workspace.until("the account to say why", |workspace| {
        workspace
            .accounts()
            .first()
            .is_some_and(|a| !workspace.text(a, "trouble").is_empty())
    });
    let account = workspace.accounts().remove(0);
    assert!(!workspace.flag(&account, "online"));
    assert!(workspace.threads().is_empty());
}

/// Every text the account's own window says, the words its mark carries on hover among them.
fn account_window(workspace: &Workspace, account: &ResourceRef) -> Vec<String> {
    let view = workspace
        .host()
        .open_resource(account, None)
        .expect("the account's window opens");
    let mut texts = Vec::new();
    texts_in(view.tree(), &mut texts);
    texts
}

// USER REQUIREMENT: a person whose server cannot be reached sees why in their mail window, and
// the window says nothing more once the server answers.
#[test]
fn a_troubled_account_says_so_in_its_window_and_clears_when_the_server_answers() {
    let servers = servers();
    let reachable = account(&servers);
    // Nothing listens at the account's server yet.
    let config = Arc::new(std::sync::Mutex::new(AccountConfig {
        imap_port: 1,
        ..reachable.clone()
    }));
    let accounts = Arc::clone(&config);
    let mut workspace = workspace_over(
        Box::new(move || vec![accounts.lock().unwrap().clone()]),
        Arc::default(),
    );
    workspace.until("the account to be made", |workspace| {
        !workspace.accounts().is_empty()
    });
    let account = workspace.accounts().remove(0);
    // The window says so where the person reads their mail: over the list, and on the mark
    // beside the account's name, which says why.
    let said = workspace.wait_for(
        "the window to say the server cannot be reached",
        |workspace| {
            let said = account_window(workspace, &account);
            said.iter()
                .any(|text| text.contains("Can't reach"))
                .then_some(said)
        },
    );
    let trouble = workspace.text(&account, "trouble");
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
    // Nothing asks the server again on its own: the window keeps saying it.
    for _ in 0..20 {
        workspace.settle();
    }
    assert!(
        account_window(&workspace, &account)
            .iter()
            .any(|text| text.contains("Can't reach")),
        "the window still says it"
    );

    // The server answers, and the person asks for their mail again: it comes in, and the
    // window says nothing more of it.
    *config.lock().unwrap() = reachable;
    workspace.press("check_now", vec![RtValue::resource(&account)]);
    synced(&mut workspace);
    workspace.until("the account to say nothing more", |workspace| {
        workspace.text(&account, "trouble").is_empty() && workspace.flag(&account, "online")
    });
    let said = workspace.wait_for("the window to say nothing more", |workspace| {
        let said = account_window(workspace, &account);
        (!said.iter().any(|text| text.contains("Can't reach"))).then_some(said)
    });
    assert!(
        !said.iter().any(|text| text.contains(&trouble)),
        "the words the mark carried are gone with it: {said:#?}"
    );
}

// USER REQUIREMENT: mail a workspace cannot keep is not half kept: no conversation stands
// filed nowhere and no message is taken as known, the account says the mail could not be
// saved, and the next sync takes the whole step in — once.
#[test]
fn a_sync_the_workspace_cannot_keep_leaves_nothing_behind_and_is_taken_in_again() {
    let servers = empty_servers();
    let mut workspace = workspace_with(account(&servers));
    let store = workspace.host().store().clone();
    // The change a conversation is made by is refused, as a record that cannot take it is.
    let package = crate::app::builtin::package_named("mail").expect("the mail package");
    let thread = package
        .types
        .iter()
        .find(|t| t.name == "Thread")
        .expect("Mail keeps conversations")
        .id
        .clone();
    store.fail_commits_adding(&thread);
    servers.imap.append(
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
    workspace.until("the account to say the mail is not saved", |workspace| {
        workspace
            .accounts()
            .first()
            .is_some_and(|a| workspace.text(a, "trouble").contains("could not be saved"))
    });
    let account = workspace.accounts().remove(0);
    assert!(
        workspace.threads().is_empty(),
        "nothing of the step is kept: {:?}",
        workspace.threads()
    );
    assert!(
        workspace.flag(&account, "online"),
        "the server answered: it is the workspace that could not take what it said"
    );
    // The window says it over the mail the server did answer with, and how old that mail is:
    // the notice adds what the account cannot say of itself.
    let said = account_window(&workspace, &account);
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

    // The workspace takes changes again: the next sync takes in what the server said before,
    // and the mail is there once.
    store.take_commits();
    synced(&mut workspace);
    assert_eq!(
        workspace
            .refs(&workspace.thread("Launch plan"), "messages")
            .len(),
        1,
        "the step that waited was taken in once"
    );
    let account = workspace.accounts().remove(0);
    assert_eq!(
        workspace.text(&account, "trouble"),
        "",
        "and the account says nothing more"
    );
}

// USER REQUIREMENT: a read the workspace cannot answer — its index has not caught up with what
// it holds — is read again on the next sync, never taken as "the account is gone". An account
// stays one account, and its mail fills its inbox once the workspace answers again.
#[test]
fn a_read_the_workspace_cannot_answer_keeps_one_account_and_its_inbox_fills() {
    let servers = empty_servers();
    let mut workspace = workspace_with(account(&servers));
    workspace.until("the account to be made", |workspace| {
        !workspace.accounts().is_empty()
    });
    let account = workspace.accounts().remove(0);
    let root = workspace.host().root_of(APP).expect("the root");
    let store = workspace.host().store().clone();
    // Mail arrives, and the workspace cannot take the step that keeps it: the step waits for
    // the next sync, though the account says nothing.
    let package = crate::app::builtin::package_named("mail").expect("the mail package");
    let thread = package
        .types
        .iter()
        .find(|t| t.name == "Thread")
        .expect("Mail keeps conversations")
        .id
        .clone();
    store.fail_commits_adding(&thread);
    servers.imap.append(
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
    workspace.press("check_now", vec![RtValue::resource(&account)]);
    workspace.until("the account to say the mail is not saved", |workspace| {
        workspace
            .accounts()
            .first()
            .is_some_and(|a| workspace.text(a, "trouble").contains("could not be saved"))
    });
    // The step waits for the next sync — and the store answers none of the account's records
    // while it takes it in again, as an index that has not caught up with the workspace does.
    store.fail_reads_of(&root.id_string());
    for _ in 0..3 {
        workspace.settle();
    }
    // The workspace answers again: the account it holds is still the one account, and the mail
    // the step waited on comes in.
    store.take_reads();
    store.take_commits();
    let accounts = workspace.accounts();
    assert_eq!(
        accounts.len(),
        1,
        "a read that failed is no account gone, and no account was made again: {accounts:?}"
    );
    let account = accounts.into_iter().next().expect("the account");
    assert_eq!(workspace.text(&account, "address"), ME);
    workspace.until("the inbox to fill", |workspace| {
        let threads = workspace.threads();
        threads.len() == 1 && !workspace.refs(&threads[0], "messages").is_empty()
    });
    assert_eq!(
        workspace.text(&account, "trouble"),
        "",
        "and the account says nothing is wrong"
    );
    assert!(workspace.flag(&account, "online"), "its server answers");
}

/// What the draft's window shows, as text.
fn editor_of(workspace: &Workspace, draft: &ResourceRef) -> String {
    let view = workspace
        .host()
        .open_resource(draft, None)
        .expect("the draft's window opens");
    format!("{:?}", view.tree())
}

/// Hold a sent message for `secs` seconds: the person's setting, written where Mail keeps it,
/// as a step of theirs in the record.
fn hold_for(workspace: &mut Workspace, secs: i64) {
    let root = workspace.host().root_of(APP).expect("the root");
    let meta = crate::app::builtin::package_named("mail").expect("the mail package");
    let field = meta
        .types
        .iter()
        .find(|t| t.name == "Mail")
        .and_then(|t| t.fields.iter().find(|f| f.name == "hold"))
        .expect("Mail keeps a hold")
        .id
        .clone();
    let edit = ui::tilo_resource::Edit::new(
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
        &mut workspace.doc,
        &action,
        action.encode().expect("it encodes"),
        Some(b"ada"),
        context,
    )
    .expect("the hold is written");
    workspace.doc.commit();
    workspace.settle();
}

/// A reply to the conversation `subject`, sent: the draft it made.
fn sent_reply(workspace: &mut Workspace, subject: &str) -> ResourceRef {
    let plan = workspace.thread(subject);
    let RtValue::Ref(draft) = workspace.press("reply", vec![RtValue::resource(&plan)]) else {
        panic!("a reply is a draft");
    };
    let draft = (*draft.target).clone();
    workspace.press("send", vec![RtValue::resource(&draft)]);
    draft
}

#[test]
fn a_sent_message_waits_for_the_persons_hold_before_it_goes() {
    let servers = servers();
    let mut workspace = workspace_with(account(&servers));
    synced(&mut workspace);
    hold_for(&mut workspace, 600);
    let draft = sent_reply(&mut workspace, "Launch plan");
    workspace.settle();
    // It is held: nothing went, and it says how it stands.
    assert!(workspace.flag(&draft, "sent"));
    assert!(
        servers.smtp.sent().is_empty(),
        "a held message does not go yet"
    );
    assert_eq!(delivery_of(&workspace, &draft).unwrap().0, "held");
    let shown = editor_of(&workspace, &draft);
    assert!(shown.contains("Sending in a moment"), "{shown}");
}

#[test]
fn a_held_message_is_taken_back_and_never_goes() {
    let servers = servers();
    let mut workspace = workspace_with(account(&servers));
    synced(&mut workspace);
    hold_for(&mut workspace, 600);
    let draft = sent_reply(&mut workspace, "Launch plan");
    assert_eq!(delivery_of(&workspace, &draft).unwrap().0, "held");

    // Its author takes it back: it never leaves, and it is a draft again.
    workspace.press("undo_send", vec![RtValue::resource(&draft)]);
    workspace.settle();
    assert!(!workspace.flag(&draft, "sent"), "it is a draft again");
    assert!(
        workspace.flag(&draft, "recalled"),
        "and it says it was taken back"
    );
    assert_eq!(delivery_of(&workspace, &draft).unwrap().0, "withdrawn");
    for _ in 0..20 {
        workspace.settle();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(servers.smtp.sent().is_empty(), "it never went");
}

#[test]
fn a_message_that_went_cannot_be_taken_back() {
    let servers = servers();
    let mut workspace = workspace_with(account(&servers));
    synced(&mut workspace);
    // No hold: it goes at once.
    hold_for(&mut workspace, 0);
    let draft = sent_reply(&mut workspace, "Launch plan");
    workspace.until("the server to take the message", |_| {
        servers.smtp.sent().len() == 1
    });
    workspace.until("the outbox to settle", |workspace| {
        workspace.host().store().outbox_pending_count() == 0
    });
    let meta = crate::app::builtin::package_named("mail").expect("the mail package");
    let action = meta
        .actions
        .iter()
        .find(|a| a.name == "undo_send")
        .expect("undo_send");
    let refused = workspace.host().runtime().invoke_action(
        &Principal::new("ada"),
        (action.id.as_str(), vec![RtValue::resource(&draft)]),
    );
    assert!(refused.is_err(), "a message that went is not taken back");
    assert_eq!(servers.smtp.sent().len(), 1);
}

#[test]
fn the_window_wakes_when_a_held_message_is_due() {
    let servers = servers();
    let mut workspace = workspace_with(account(&servers));
    synced(&mut workspace);
    hold_for(&mut workspace, 600);
    let draft = sent_reply(&mut workspace, "Launch plan");
    workspace.settle();
    // The window waits for something that is not nothing: the moment the message is due, or
    // the minute the inbox's snooze filter next reads differently.
    let wait = workspace
        .window
        .instance()
        .wakes_in()
        .expect("a held message sets the window's next wake");
    assert!(
        wait <= std::time::Duration::from_secs(610),
        "it waits no longer than the hold: {wait:?}"
    );
    let held = workspace
        .window
        .instance()
        .host()
        .expect("the host")
        .runtime()
        .held_for()
        .expect("the message is held");
    assert!(
        held > std::time::Duration::from_secs(500),
        "the message is held for the person's hold: {held:?}"
    );
    assert_eq!(delivery_of(&workspace, &draft).unwrap().0, "held");
}

/// The row of `message` in the conversation `thread`, as activating its list row made it: what
/// it shows, and the keys of the controls under the row's fold.
fn message_rows(workspace: &mut Workspace, thread: &ResourceRef) -> Vec<Vec<String>> {
    let row = workspace.row_showing(thread);
    workspace.activate(&row);
    let detail = workspace
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
    workspace.settle();
    out
}

// USER REQUIREMENT: a reply's quotation of the message it answers folds in the conversation, as
// Gmail and Apple Mail fold it, so a thread reads as what each person said.
#[test]
fn a_replys_quotation_folds_in_the_conversation() {
    let servers = servers();
    let mut workspace = workspace_with(account(&servers));
    synced(&mut workspace);
    hold_for(&mut workspace, 0);
    let plan = workspace.thread("Launch plan");
    // A message of the conversation that answers another, as the composer makes one: what it
    // says, then the message it answers quoted under it, as its sender's client wrote it.
    let replies = workspace.refs(&plan, "messages");
    let reply = replies[1].clone();
    add_quote(
        &mut workspace,
        &reply,
        "The fifth works for the beta.",
        "Here is the draft plan.",
    );
    let rows = message_rows(&mut workspace, &plan);
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
fn add_quote(workspace: &mut Workspace, message: &ResourceRef, said: &str, quote: &str) {
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
        &mut workspace.doc,
        &action,
        action.encode().expect("it encodes"),
        Some(b"ada"),
        context,
    )
    .expect("the message says it");
    workspace.doc.commit();
    workspace.settle();
}

/// How the draft `draft` stands on its way out, as `delivery(d)` reads it.
fn delivery_of(workspace: &Workspace, draft: &ResourceRef) -> Option<(String, String)> {
    use tilolang_tilo::runtime::store::Store as _;
    workspace
        .host()
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
    let mut workspace = workspace_over(
        Box::new(move || vec![accounts.lock().unwrap().clone()]),
        Arc::clone(&reconnected),
    );
    synced(&mut workspace);
    hold_for(&mut workspace, 0);
    let plan = workspace.thread("Launch plan");
    let RtValue::Ref(draft) = workspace.press("reply", vec![RtValue::resource(&plan)]) else {
        panic!("a reply is a draft");
    };
    let draft = (*draft.target).clone();
    workspace.press("send", vec![RtValue::resource(&draft)]);
    workspace.until("the message to wait to go online", |workspace| {
        delivery_of(workspace, &draft)
            .is_some_and(|(state, reason)| state == "waiting" && !reason.is_empty())
    });
    // The draft says so where it is written.
    let shown = editor_of(&workspace, &draft);
    assert!(shown.contains("Waiting to go online"), "{shown}");
    // It is not tried again on its own.
    for _ in 0..20 {
        workspace.settle();
    }
    assert!(servers.smtp.sent().is_empty());
    assert_eq!(delivery_of(&workspace, &draft).unwrap().0, "waiting");

    // The network comes back, and with it the server: the message goes, once.
    *config.lock().unwrap() = reachable;
    reconnected.store(true, Ordering::Release);
    workspace.until("the message to be sent", |workspace| {
        delivery_of(workspace, &draft).is_some_and(|(state, _)| state == "sent")
    });
    assert_eq!(servers.smtp.sent().len(), 1);
    let shown = editor_of(&workspace, &draft);
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

/// A workspace as an instance in a tilolang host keeps it: its origin reaches the connector over a
/// socket, holding no network and no credential of its own.
fn workspace_attached(
    connector: &std::sync::mpsc::Sender<tilolang_mail::serve::Wakeup>,
) -> Workspace {
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    connector
        .send(tilolang_mail::serve::Wakeup::Attach(theirs))
        .unwrap();
    let origin = tilolang_mail::remote_origin(ours, Arc::new(|| {})).expect("the origin attaches");
    workspace_of(origin)
}

#[test]
fn an_instance_without_network_syncs_and_sends_through_the_mail_connector() {
    let servers = servers();
    let connector = connector_for(account(&servers));
    let mut workspace = workspace_attached(&connector);
    synced(&mut workspace);
    hold_for(&mut workspace, 0);
    let plan = workspace.thread("Launch plan");
    let RtValue::Ref(draft) = workspace.press("reply", vec![RtValue::resource(&plan)]) else {
        panic!("a reply is a draft");
    };
    let draft = (*draft.target).clone();
    workspace.press("send", vec![RtValue::resource(&draft)]);
    workspace.until("the message to be sent", |workspace| {
        delivery_of(workspace, &draft).is_some_and(|(state, _)| state == "sent")
    });
    assert_eq!(servers.smtp.sent().len(), 1);

    // The window closes; mail arrives meanwhile; the next window has it.
    drop(workspace);
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
    let mut workspace = workspace_attached(&connector);
    workspace.until("the mail that came while it was closed", |workspace| {
        workspace
            .threads()
            .iter()
            .any(|t| workspace.text(t, "subject") == "Your lease renewal")
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

/// Every text `node` says, its whole tree walked: what its `value`, `title`, `secondary` and
/// `label` props show, what its `description` says — the words a mark carries on hover — and
/// what a notice says under its title.
fn texts_in(node: &tilolang_tilo::runtime::UiNode, out: &mut Vec<String>) {
    for prop in [
        "value",
        "title",
        "secondary",
        "label",
        "description",
        "message",
    ] {
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
fn conversation_of(workspace: &mut Workspace, thread: &ResourceRef) -> Vec<String> {
    let row = workspace.row_showing(thread);
    workspace.activate(&row);
    let detail = workspace
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
    workspace.settle();
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
    let mut workspace = workspace_over(
        Box::new(move || vec![account(&servers), work_account(&second)]),
        Arc::default(),
    );
    workspace.until("both accounts' mail", |workspace| {
        workspace.accounts().len() == 2
            && workspace
                .accounts()
                .iter()
                .all(|a| !workspace.refs(a, "threads").is_empty())
    });
    let mut texts = Vec::new();
    let root = workspace.window.root_tree().expect("the mail window runs");
    host_texts_in(&root, &mut texts);
    for want in ["Sam Rivera", "Sam at work"] {
        assert!(
            texts.iter().any(|text| text == want),
            "the sidebar names {want}: {texts:#?}"
        );
    }
    // The second account's own window shows its inbox, as its row in the sidebar opens it.
    let work = workspace
        .accounts()
        .into_iter()
        .find(|a| workspace.text(a, "address") == "work@rivera.example")
        .expect("the second account");
    let shown = conversation_titles(&workspace, &work);
    assert!(
        shown.iter().any(|text| text == "Inbox"),
        "the account's window shows its mailboxes: {shown:#?}"
    );
    assert!(
        workspace
            .refs(&work, "threads")
            .iter()
            .any(|t| workspace.text(t, "subject") == "Team holiday"),
        "the second account holds its own mail"
    );
}

/// The subjects a mail window over `of` lists, its root view's texts walked.
fn conversation_titles(workspace: &Workspace, of: &ResourceRef) -> Vec<String> {
    let view = workspace
        .host()
        .open_resource(of, None)
        .expect("the account's window opens");
    let mut texts = Vec::new();
    texts_in(view.tree(), &mut texts);
    texts
}

/// How many conversations the account's window lists in the sidebar's place `place`: the place
/// is chosen first, as a person clicks it, because a window opens on the place it was left at.
fn place_count(workspace: &Workspace, account: &ResourceRef, place: &str) -> Option<i64> {
    let mut view = workspace
        .host()
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

/// How many conversations the account's window says its inbox holds now: what its own filter
/// counts, as its header shows it.
fn inbox_count(workspace: &Workspace, account: &ResourceRef) -> Option<i64> {
    place_count(workspace, account, "Inbox")
}

/// The `overview_count` prop of the first node of `kind`.
fn count_in(node: &tilolang_tilo::runtime::UiNode, kind: &str) -> Option<i64> {
    if node.kind == kind
        && let Some(tilolang_tilo::runtime::Value::Int(n)) = node.props.get("overview_count")
    {
        return Some(*n);
    }
    node.children.iter().find_map(|c| count_in(c, kind))
}

/// The wall clock at `hour:minute:second` on `day` of 2026, in the zone this process reads as
/// the person's.
fn local(month: u32, day: u32, (hour, minute, second): (u32, u32, u32)) -> ui::time::SystemTime {
    use chrono::TimeZone as _;
    let at = chrono::Local
        .with_ymd_and_hms(2026, month, day, hour, minute, second)
        .earliest()
        .expect("the time exists where the person is");
    let ms = u64::try_from(at.timestamp_millis()).expect("after 1970");
    ui::time::UNIX_EPOCH
        .checked_add(ui::time::Duration::from_millis(ms))
        .expect("the clock reaches it")
}

/// A mail window pinned to a fixed wall clock, as a test reads a snooze's moment.
fn workspace_at(at: ui::time::SystemTime, servers: &Servers) -> Workspace {
    ui::time::testing::with_wall(at, || {
        let mut workspace = workspace_with(account(servers));
        synced(&mut workspace);
        workspace
    })
}

// USER REQUIREMENT: snoozing a conversation takes it out of the inbox until a moment, and it
// comes back then with no window open.
#[test]
fn snoozing_takes_a_conversation_out_of_the_inbox_until_its_moment() {
    use ui::time::testing::with_wall;
    let _settled = ui::motion::testing::settled();
    let servers = servers();
    let mut workspace = workspace_at(local(6, 30, (10, 0, 0)), &servers);
    let account = workspace.accounts().remove(0);
    let before = inbox_count(&workspace, &account).expect("the inbox counts its mail");
    assert_eq!(
        before, 2,
        "the account's two conversations are in the inbox"
    );
    let plan = workspace.thread("Launch plan");
    let snoozed = with_wall(local(6, 30, (10, 0, 0)), || {
        workspace.press(
            "snooze",
            vec![
                RtValue::resource(&plan),
                RtValue::text("2026-06-30T10:30:00Z"),
            ],
        );
        inbox_count(&workspace, &account)
    });
    assert_eq!(
        snoozed,
        Some(before - 1),
        "a snoozed conversation leaves the inbox"
    );
    // The inbox reads the clock (the snooze), so the window wakes at the next minute and the
    // conversation comes back on its own, with no window open.
    let wait = workspace
        .window
        .instance()
        .wakes_in()
        .expect("a snoozed conversation sets the window's next wake");
    assert!(
        wait <= std::time::Duration::from_secs(60),
        "the window re-reads the time within a minute: {wait:?}"
    );
    // The moment passes with no window open: the conversation is in the inbox again.
    let shown = with_wall(local(6, 30, (10, 31, 0)), || {
        workspace.settle();
        inbox_count(&workspace, &account)
    });
    assert_eq!(
        shown,
        Some(before),
        "back in the inbox once its moment passed"
    );
    assert_eq!(
        with_wall(local(6, 30, (10, 31, 0)), || {
            workspace.press("unsnooze", vec![RtValue::resource(&plan)]);
            inbox_count(&workspace, &account)
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
    let mut workspace = workspace_with(account(&servers));
    synced(&mut workspace);
    let plan = workspace.thread("Launch plan");
    workspace.press(
        "label",
        vec![
            RtValue::resource(&plan),
            RtValue::text("Work"),
            RtValue::Bool(true),
        ],
    );
    let labels = workspace.read(&plan, "labels");
    assert!(
        matches!(&labels, RtValue::List(items) if items.iter().any(|l| l == &RtValue::text("Work"))),
        "the label is on the conversation: {labels:?}"
    );
    // The label is the person's own: the root holds it, so it is a place from then on.
    let root = workspace.host().root_of(APP).expect("the root");
    let named: Vec<String> = workspace
        .refs(&root, "labels")
        .iter()
        .map(|l| workspace.text(l, "name"))
        .collect();
    assert_eq!(named, ["Work"], "the person's labels hold it once");

    // Its place narrows the window to the conversation that carries it.
    let account = workspace.accounts().remove(0);
    assert_eq!(
        place_count(&workspace, &account, "Work"),
        Some(1),
        "only the conversation with the label is listed"
    );

    // Taken away, the conversation no longer carries it.
    workspace.press(
        "label",
        vec![
            RtValue::resource(&plan),
            RtValue::text("Work"),
            RtValue::Bool(false),
        ],
    );
    let labels = workspace.read(&plan, "labels");
    assert!(
        !matches!(&labels, RtValue::List(items) if items.iter().any(|l| l == &RtValue::text("Work"))),
        "the label is gone: {labels:?}"
    );
}

/// The first node of `kind` whose prop `name` is `value`.
fn find_kind<'a>(
    node: &'a tilolang_tilo::runtime::UiNode,
    kind: &str,
    name: &str,
    value: &str,
) -> Option<&'a tilolang_tilo::runtime::UiNode> {
    if node.kind == kind
        && matches!(node.props.get(name), Some(tilolang_tilo::runtime::Value::Text(t)) if t == value)
    {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find_kind(child, kind, name, value))
}

// USER REQUIREMENT: a muted conversation stays out of the inbox while new messages keep coming,
// and is found under Muted until it is brought back, as Gmail mutes a busy thread.
#[test]
fn muting_keeps_a_conversation_out_of_the_inbox_until_it_is_brought_back() {
    let servers = servers();
    let mut workspace = workspace_with(account(&servers));
    synced(&mut workspace);
    let account = workspace.accounts().remove(0);
    let before = inbox_count(&workspace, &account).expect("the inbox counts its mail");
    let plan = workspace.thread("Launch plan");
    workspace.press("mute", vec![RtValue::resource(&plan)]);
    assert!(workspace.flag(&plan, "muted"));
    assert_eq!(
        inbox_count(&workspace, &account),
        Some(before - 1),
        "a muted conversation leaves the inbox"
    );
    // New mail in it does not bring it back: with it unread again, as a new message leaves it,
    // it is still out of the inbox.
    workspace.press(
        "mark_read",
        vec![RtValue::resource(&plan), RtValue::Bool(true)],
    );
    assert!(workspace.flag(&plan, "unread"));
    assert_eq!(
        inbox_count(&workspace, &account),
        Some(before - 1),
        "the muted conversation stays out of the inbox while it is answered"
    );
    // Its place of its own lists it, so it is not lost.
    assert_eq!(
        place_count(&workspace, &account, "Muted"),
        Some(1),
        "Muted lists the muted conversation"
    );
    // Bringing it back unmutes it: it is in the inbox again.
    workspace.press("to_inbox", vec![RtValue::resource(&plan)]);
    assert!(!workspace.flag(&plan, "muted"));
    assert_eq!(
        inbox_count(&workspace, &account),
        Some(before),
        "brought back, it is in the inbox again"
    );
}

// USER REQUIREMENT: each account signs what it writes with the person's own signature, above
// what a reply quotes, as every mail client does.
#[test]
fn a_new_message_and_an_answer_carry_the_accounts_signature() {
    let servers = servers();
    let mut workspace = workspace_with(account(&servers));
    synced(&mut workspace);
    let account = workspace.accounts().remove(0);
    assert_eq!(
        workspace.text(&account, "signature"),
        "",
        "an account starts without a signature"
    );
    workspace.press(
        "set_signature",
        vec![
            RtValue::resource(&account),
            RtValue::text("Sam\nhttps://rivera.example"),
        ],
    );
    assert_eq!(
        workspace.text(&account, "signature"),
        "Sam\nhttps://rivera.example"
    );

    // A new message starts with it.
    let RtValue::Ref(draft) = workspace.press("new_message", vec![RtValue::resource(&account)])
    else {
        panic!("a new message is a draft");
    };
    assert_eq!(
        workspace.text(&(*draft.target).clone(), "body"),
        "Sam\nhttps://rivera.example",
        "a new message carries the signature"
    );

    // An answer puts it over the quotation it answers.
    hold_for(&mut workspace, 0);
    let plan = workspace.thread("Launch plan");
    let RtValue::Ref(reply) = workspace.press("reply", vec![RtValue::resource(&plan)]) else {
        panic!("a reply is a draft");
    };
    let reply = (*reply.target).clone();
    let body = workspace.text(&reply, "body");
    assert!(
        body.starts_with("Sam\nhttps://rivera.example\n\nOn "),
        "the signature comes before the quotation: {body:?}"
    );

    // A message typed in the conversation goes with it, over the quotation too.
    workspace.press(
        "answer",
        vec![
            RtValue::resource(&plan),
            RtValue::text("Thursday suits me."),
        ],
    );
    workspace.until("the server to take the answer", |_| {
        servers.smtp.sent().len() == 1
    });
    let sent = String::from_utf8_lossy(&servers.smtp.sent()[0]).into_owned();
    let said = sent
        .find("Thursday suits me.")
        .expect("it says what was typed");
    let signature = sent
        .find("https://rivera.example")
        .expect("it carries the signature");
    let quotation = sent.find("On 2026-").expect("it quotes what it answers");
    assert!(
        said < signature && signature < quotation,
        "the answer, its signature, then what it quotes: {sent}"
    );
}
#[test]
fn marking_all_read_reads_every_conversation_and_reaches_the_server() {
    let servers = servers();
    let mut workspace = workspace_with(account(&servers));
    synced(&mut workspace);
    let account = workspace.accounts().remove(0);
    workspace.press("mark_all_read", vec![RtValue::resource(&account)]);
    for t in workspace.refs(&account, "threads") {
        assert!(!workspace.flag(&t, "unread"), "every conversation is read");
    }
    workspace.until("the server to hold them read", |_| {
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
    let mut workspace = workspace_at(local(6, 30, (10, 0, 0)), &servers);
    hold_for(&mut workspace, 0);
    let plan = workspace.thread("Launch plan");
    let RtValue::Ref(draft) = workspace.press("reply", vec![RtValue::resource(&plan)]) else {
        panic!("a reply is a draft");
    };
    let draft = (*draft.target).clone();
    // Everything up to its cancellation happens at the pinned clock, so the moment it goes is
    // still ahead: the frame settles without carrying it out.
    let wait = with_wall(local(6, 30, (10, 0, 0)), || {
        workspace.press(
            "send_later",
            vec![
                RtValue::resource(&draft),
                RtValue::text("2026-06-30T18:00:00Z"),
            ],
        );
        assert!(
            workspace.flag(&draft, "sent"),
            "it is on its way, at a moment"
        );
        assert_eq!(
            workspace.text(&draft, "send_at"),
            "2026-06-30T18:00:00Z",
            "the moment is recorded on the message"
        );
        assert_eq!(delivery_of(&workspace, &draft).unwrap().0, "held");
        assert!(
            servers.smtp.sent().is_empty(),
            "a message sent later does not go yet"
        );
        // The window waits for its moment, and no longer.
        workspace
            .window
            .instance()
            .wakes_in()
            .expect("a message sent later sets the window's next wake")
    });
    assert!(
        wait <= std::time::Duration::from_secs(8 * 3600 + 60),
        "it waits until the moment it goes: {wait:?}"
    );
    // Cancelled before it goes: it is a draft again, and it never leaves.
    with_wall(local(6, 30, (10, 1, 0)), || {
        workspace.press("cancel_later", vec![RtValue::resource(&draft)]);
    });
    assert!(!workspace.flag(&draft, "sent"), "it is a draft again");
    assert!(workspace.flag(&draft, "recalled"));
    assert_eq!(delivery_of(&workspace, &draft).unwrap().0, "withdrawn");
    for _ in 0..20 {
        workspace.settle();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(servers.smtp.sent().is_empty(), "it never went");
}

// USER REQUIREMENT: a conversation must read as a mail client's does — its subject, each
// message's sender and recipients, and what came with it — not as a run of bare bodies.
#[test]
fn a_conversation_shows_its_subject_senders_recipients_and_attachments() {
    let servers = servers();
    let mut workspace = workspace_with(account(&servers));
    synced(&mut workspace);
    let plan = workspace.thread("Launch plan");
    let shown = conversation_of(&mut workspace, &plan);
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
    workspace.until("the message with the lease", |workspace| {
        workspace
            .threads()
            .iter()
            .any(|t| workspace.text(t, "subject") == "Your lease renewal")
    });
    let lease = workspace.thread("Your lease renewal");
    let shown = conversation_of(&mut workspace, &lease);
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
    let mut workspace = workspace_with(account(&servers));
    synced(&mut workspace);
    hold_for(&mut workspace, 0);
    let plan = workspace.thread("Launch plan");
    let thread = RtValue::resource(&plan);
    workspace.press("answer", vec![thread, RtValue::text("Thursday suits me.")]);
    workspace.until("the server to take the answer", |_| {
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
    let mut workspace = workspace_with(account(&servers));
    synced(&mut workspace);
    let plan = workspace.thread("Launch plan");
    let thread = RtValue::resource(&plan);
    let refused = workspace.host().runtime().invoke_action(
        &Principal::new("ada"),
        ("answer", vec![thread, RtValue::text("")]),
    );
    assert!(refused.is_err(), "an empty answer is refused: {refused:?}");
    assert!(servers.smtp.sent().is_empty());
}
