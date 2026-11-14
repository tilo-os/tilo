//! A window that reaches no mail server (a browser page) beside a window on a
//! device that holds the account: the page asks for new mail and sends, and
//! the device carries both out against real IMAP and SMTP servers, through
//! the mailbox they share.

use std::sync::Arc;

use ui::framework::testing::TestApp;

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

#[test]
#[allow(clippy::too_many_lines, reason = "one scenario, read top to bottom")]
fn a_page_reads_checks_and_sends_mail_through_a_device_that_holds_the_account() {
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

    // Mail arrives at the server after the device's first sync.
    until(
        &mut page,
        &mut device,
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

    // The page sends: the message waits in the outbox, and the device that
    // holds the account delivers it.
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
    let queued =
        page.read_document(|doc| read_mail(doc).map_or(0, |mail| mail.outbox.entries().len()));
    assert_eq!(queued, 1, "the message waits in the outbox");
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
    drop(imap);
}
