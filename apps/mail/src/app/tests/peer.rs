//! A window that reaches no mail server (a browser page) beside a window on a
//! device that holds the account: the page asks for new mail and sends, and
//! the device carries both out against real IMAP and SMTP servers, through
//! the mailbox they share.

use std::sync::Arc;

use ui::framework::testing::TestApp;
use ui::time::testing::SharedWall;

use crate::fake_servers::{FakeImap, FakeSmtp, Layout, accepting_client_config, server_tls_config};
use crate::resource::{MailShell, read_mail};
use settings::panes::accounts::AccountsEffect;

const ME: &str = "office@company.example";

type Window = TestApp<MailShell>;

fn mail(window: &mut Window) -> &mut crate::app::MailApp {
    window.surface_mut::<crate::app::MailApp>()
}

/// The content of the person's copies record as its route keeps it: what
/// one device keeps there, the person's other devices read, and no device
/// collects it from under them.
struct Route(tilo_content::MemoryBackend);

impl tilo_content::ContentBackend for Route {
    fn put(
        &self,
        reader: &mut dyn std::io::Read,
        mime: &str,
    ) -> Result<tilo_content::Stored, tilo_content::ContentError> {
        self.0.put(reader, mime)
    }

    fn open(
        &self,
        hash: &str,
    ) -> Result<Option<tilo_content::ContentReader>, tilo_content::ContentError> {
        self.0.open(hash)
    }

    fn has(&self, hash: &str) -> bool {
        self.0.has(hash)
    }

    fn shared(&self) -> bool {
        true
    }
}

/// Both windows take in what the other changed in the mailbox, and settle.
fn exchange(page: &mut Window, device: &mut Window) {
    page.settle();
    page.deliver_changes_to(device).unwrap();
    device.settle();
    device.deliver_changes_to(page).unwrap();
    page.settle();
}

/// What a window holds, for a failure to say: its messages, its sync
/// failure, and how the mailbox's checks and sends stand.
fn holds(window: &mut Window) -> String {
    let outboxes = window.read_document(|doc| {
        read_mail(doc).map(|mail| {
            mail.checks
                .entries()
                .map(|(_, entry)| format!("check {}: {:?}", entry.value.account, entry.settled))
                .chain(mail.outbox.entries().map(|(_, entry)| {
                    format!("send {:?}: {:?}", entry.value.draft.subject, entry.settled)
                }))
                .collect::<Vec<_>>()
        })
    });
    let app = mail(window);
    let st = app.shared.borrow();
    format!(
        "{} messages, failure {:?}, {outboxes:?}",
        st.mailbox.messages.len(),
        st.sync_error
    )
}

fn until(
    page: &mut Window,
    device: &mut Window,
    what: &str,
    done: impl Fn(&mut Window, &mut Window) -> bool,
) {
    let deadline = tilo_test_env::hang_deadline();
    while !done(page, device) {
        assert!(
            ui::time::now() < deadline,
            "{what}: the page holds {}; the device {}",
            holds(page),
            holds(device)
        );
        exchange(page, device);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// A page and a device that holds the account, beside the real IMAP and SMTP
/// servers they reach, once the device's first sync has ended.
struct Peers {
    imap: FakeImap,
    smtp: FakeSmtp,
    /// The vault the account's secret lives in: the device releases it from
    /// here, so it must outlive both windows.
    _vault: tempfile::TempDir,
    /// The Settings window that made the account: the person's accounts
    /// record lives through it, so it outlives both windows too.
    _settings: TestApp<settings::resource::SettingsShell>,
    page: Window,
    device: Window,
}

fn peers() -> Peers {
    let rt = ui::rt::app();
    let imap = rt
        .block_on(FakeImap::start(Layout::Gmail, server_tls_config()))
        .unwrap();
    let smtp = rt.block_on(FakeSmtp::start(server_tls_config())).unwrap();

    // The account, as a migration or Settings keeps it: its settings in the
    // person's accounts record, its password in the vault of the device that
    // holds it.
    let vault = tempfile::tempdir().unwrap();
    let mut settings =
        TestApp::<settings::resource::SettingsShell>::open_data_dir(vault.path()).unwrap();
    settings
        .effect(&AccountsEffect::AddEmailAccount {
            display_name: "Office".into(),
            email: ME.into(),
            username: None,
            imap_host: "127.0.0.1".into(),
            imap_port: imap.port,
            smtp_host: "127.0.0.1".into(),
            smtp_port: smtp.port,
            password: "app-password".into(),
        })
        .expect("Settings adds the account");
    let accounts = settings
        .shared_person::<accounts::person::PersonAccounts>()
        .unwrap();

    // The device: custody releases it the secret, and it reaches the servers.
    let mut device = Window::on_device("laptop").unwrap();
    {
        let app = mail(&mut device);
        app.custody = Some(crate::custody::SecretRelease::kept_under(
            vault.path().to_path_buf(),
        ));
        let engine = crate::engine::live::LiveEngine::trusting(
            tilo_content::ContentStore::memory(),
            Arc::new(|| {}),
            accepting_client_config(),
        )
        .unwrap();
        let (tx, rx) = crate::engine::spawn_with(Box::new(engine));
        app.state().engine = Some(crate::app::EngineHandle { tx, rx });
        app.follow_person_accounts(accounts.clone());
    }
    // What the device fetches it keeps for the person's other devices: the
    // person's copies record, its content carried beside it (a test window's
    // record keeps none of its own).
    let copies = device
        .person::<crate::copies::MailCopies>()
        .expect("the device keeps copies");
    let carried = tilo_content::ContentStore::new(Route(tilo_content::MemoryBackend::default()));
    mail(&mut device).copies = Some(crate::copies::Copies::new(copies.clone(), carried.clone()));

    // The page: the same accounts record, no secret and no server.
    let mut page = Window::on_device("page").unwrap();
    {
        let app = mail(&mut page);
        app.reach_no_server();
        app.custody = None;
        app.state().checks.asks = true;
        app.follow_person_accounts(accounts);
        app.copies = Some(crate::copies::Copies::new(copies, carried));
    }
    page.settle();

    let mut peers = Peers {
        imap,
        smtp,
        _vault: vault,
        _settings: settings,
        page,
        device,
    };
    until(
        &mut peers.page,
        &mut peers.device,
        "the device's first sync ends",
        |_, device| {
            !mail(device).awaiting_server()
                && mail(device)
                    .shared
                    .borrow()
                    .accounts
                    .active_account()
                    .is_some()
        },
    );
    peers
}

/// The page sends one message, as its Send button does.
fn send_from_page(page: &mut Window) {
    page.invoke_confirmed(crate::resource::Mail::SEND_MESSAGE.with(
        crate::resource::mailbox::SendMessage {
            account: ME.into(),
            message: crate::actions::DraftData {
                to: "grace@navy.example".into(),
                subject: "Re: Quarterly invoices".into(),
                body: "Confirmed.".into(),
                ..Default::default()
            },
            in_reply_to: None,
            draft: None,
        },
    ))
    .expect("the page sends");
}

/// How many messages wait in a window's outbox, not yet delivered.
fn pending_outbox(window: &Window) -> usize {
    window.read_document(|doc| read_mail(doc).map_or(0, |mail| mail.outbox.pending().count()))
}

/// The messages a window's outbox holds, settled or not.
fn outbox(window: &Window) -> Vec<crate::actions::Outgoing> {
    window.read_document(|doc| {
        read_mail(doc).map_or_else(Vec::new, |mail| {
            mail.outbox
                .entries()
                .map(|(_, entry)| entry.value.clone())
                .collect()
        })
    })
}

/// Undo on the window that pressed Send: it takes back what it sent, wherever
/// the account's device is.
fn undo_here(window: &mut Window) {
    let sent = outbox(window);
    mail(window).sent_here = sent;
    mail(window).unsend = true;
    window.settle();
}

/// The undo window is over: the sending window's held message is due, and its
/// delivering device may carry it out.
fn window_ends(device: &mut Window) {
    for held in &mut mail(device).held_sends {
        held.due = ui::time::now();
    }
}

fn sent_with(smtp: &FakeSmtp, subject: &str) -> usize {
    smtp.sent()
        .iter()
        .filter(|raw| String::from_utf8_lossy(raw).contains(subject))
        .count()
}

#[test]
#[allow(clippy::too_many_lines, reason = "one scenario, read top to bottom")]
fn a_page_reads_checks_and_sends_mail_through_a_device_that_holds_the_account() {
    let Peers {
        imap,
        smtp,
        _vault,
        _settings,
        mut page,
        mut device,
    } = peers();

    // Mail arrives at the server after the device's first sync.
    imap.append(
        "INBOX",
        "From: Grace <grace@navy.example>\r\nTo: office@company.example\r\n\
Subject: Quarterly invoices\r\nMessage-ID: <invoices@navy.example>\r\n\
Date: Thu, 12 Nov 2026 09:30:00 +0000\r\n\r\nPlease confirm by Friday.\r\n",
        &[],
    );

    // The page asks: a check waits in the mailbox until the device takes it.
    let _ = mail(&mut page).state().refresh_active_folder();
    page.settle();
    let asked =
        page.read_document(|doc| read_mail(doc).map_or(0, |mail| mail.checks.pending().count()));
    assert!(asked >= 1, "the page asked a device to check");

    until(
        &mut page,
        &mut device,
        "the page shows the mail the device fetched",
        |page, _| {
            mail(page)
                .shared
                .borrow()
                .mailbox
                .messages
                .iter()
                .any(|(_, message)| message.subject == "Quarterly invoices")
        },
    );
    until(
        &mut page,
        &mut device,
        "the device settles the page's checks",
        |page, _| {
            page.read_document(|doc| {
                read_mail(doc).is_some_and(|mail| mail.checks.pending().count() == 0)
            })
        },
    );
    assert_eq!(
        mail(&mut page).shared.borrow().sync_error,
        None,
        "a device took the check, so the page says nothing is wrong"
    );
    assert!(
        mail(&mut page)
            .shared
            .borrow()
            .accounts
            .accounts
            .iter()
            .all(|account| account.config.password.is_empty()),
        "the page never holds the account's secret"
    );

    // The page sends: the message waits in the outbox for its undo window,
    // and the device that holds the account delivers it after that.
    let start = ui::time::wall();
    let wall = SharedWall::default();
    wall.follow();
    wall.pin(start);
    send_from_page(&mut page);
    let queued =
        page.read_document(|doc| read_mail(doc).map_or(0, |mail| mail.outbox.entries().len()));
    assert_eq!(queued, 1, "the message waits in the outbox");
    exchange(&mut page, &mut device);
    assert_eq!(
        sent_with(&smtp, "Re: Quarterly invoices"),
        0,
        "it waits out the undo window"
    );
    wall.pin(start + ui::time::Duration::from_secs(11));
    window_ends(&mut device);
    until(
        &mut page,
        &mut device,
        "the device delivers what the page sent",
        |_, _| {
            smtp.sent()
                .iter()
                .any(|raw| String::from_utf8_lossy(raw).contains("Re: Quarterly invoices"))
        },
    );
    until(
        &mut page,
        &mut device,
        "the page reads that it was sent",
        |page, _| {
            page.read_document(|doc| {
                read_mail(doc).is_some_and(|mail| {
                    mail.outbox.pending().count() == 0 && mail.outbox.entries().len() > 0
                })
            })
        },
    );
    wall.release();
    drop(imap);
}

#[test]
fn a_page_takes_back_a_message_before_the_window_ends_and_nothing_goes_out() {
    let mut peers = peers();
    let start = ui::time::wall();
    let wall = SharedWall::default();
    wall.follow();
    wall.pin(start);
    send_from_page(&mut peers.page);
    exchange(&mut peers.page, &mut peers.device);
    assert_eq!(
        sent_with(&peers.smtp, "Re: Quarterly invoices"),
        0,
        "the message waits in the outbox"
    );

    // Undo, pressed on the page, takes the message back before any device
    // delivers it: it leaves the outbox, wherever it was pressed.
    undo_here(&mut peers.page);
    exchange(&mut peers.page, &mut peers.device);
    assert_eq!(
        pending_outbox(&peers.page),
        0,
        "taken out of the outbox unsent"
    );
    assert!(
        mail(&mut peers.page).compose.open,
        "the message is back in the composer"
    );

    // Even past the window nothing goes out: the outbox no longer holds it.
    wall.pin(start + ui::time::Duration::from_secs(11));
    window_ends(&mut peers.device);
    for _ in 0..3 {
        exchange(&mut peers.page, &mut peers.device);
    }
    assert_eq!(
        sent_with(&peers.smtp, "Re: Quarterly invoices"),
        0,
        "nothing went out"
    );
    wall.release();
}

#[test]
fn a_page_message_goes_out_only_after_the_undo_window() {
    let mut peers = peers();
    let start = ui::time::wall();
    let wall = SharedWall::default();
    wall.follow();
    wall.pin(start);
    send_from_page(&mut peers.page);
    assert_eq!(
        pending_outbox(&peers.page),
        1,
        "the message waits in the outbox"
    );
    let not_before = outbox(&peers.page)
        .into_iter()
        .next()
        .and_then(|outgoing| outgoing.not_before)
        .expect("the send carries the moment it may leave");
    assert_eq!(
        not_before,
        ui::time::unix_now_ms_i64() + crate::actions::SEND_UNDO_MS,
        "the device pressing Send sets the undo window"
    );

    exchange(&mut peers.page, &mut peers.device);
    assert_eq!(
        sent_with(&peers.smtp, "Re: Quarterly invoices"),
        0,
        "it waits out the window"
    );

    // The window closes: now the account's device delivers it.
    wall.pin(start + ui::time::Duration::from_secs(11));
    window_ends(&mut peers.device);
    {
        let Peers {
            page, device, smtp, ..
        } = &mut peers;
        until(
            page,
            device,
            "the device delivers it once the window is over",
            |_, _| sent_with(smtp, "Re: Quarterly invoices") == 1,
        );
    }
    wall.release();
}

#[test]
fn undo_after_the_message_went_out_says_it_was_already_sent() {
    let mut peers = peers();
    let start = ui::time::wall();
    let wall = SharedWall::default();
    wall.follow();
    wall.pin(start);
    send_from_page(&mut peers.page);
    exchange(&mut peers.page, &mut peers.device);

    // The device that holds the account runs ahead of the page's clock: it
    // delivers while the page's own undo window still looks open.
    wall.pin(start + ui::time::Duration::from_secs(11));
    window_ends(&mut peers.device);
    {
        let Peers {
            page, device, smtp, ..
        } = &mut peers;
        until(page, device, "the device delivers the message", |_, _| {
            sent_with(smtp, "Re: Quarterly invoices") == 1
        });
    }
    wall.pin(start + ui::time::Duration::from_secs(5));
    exchange(&mut peers.page, &mut peers.device);

    // Undo reaches the outbox too late: it says so instead of failing
    // silently.
    undo_here(&mut peers.page);
    assert_eq!(
        mail(&mut peers.page)
            .nav
            .toast_text()
            .map(str::to_owned)
            .as_deref(),
        Some("Already sent"),
        "undo says the message already left"
    );
    assert_eq!(
        sent_with(&peers.smtp, "Re: Quarterly invoices"),
        1,
        "only the one message went out"
    );
    wall.release();
}
