//! An idle Mail window over a connected account: what it does while
//! nothing arrives.

use automerge::transaction::Transactable;
use ui::framework::testing::TestApp;
use ui::widget::Widget as _;

use crate::fake_servers::{FakeImap, FakeSmtp, Layout, server_tls_config};
use crate::resource::MailShell;

const ME: &str = "hiring@studio.example";

fn connected_window() -> (TestApp<MailShell>, FakeImap, tempfile::TempDir) {
    let rt = ui::rt::app();
    let imap = rt
        .block_on(FakeImap::start(Layout::Gmail, server_tls_config()))
        .unwrap();
    let _smtp = rt.block_on(FakeSmtp::start(server_tls_config())).unwrap();
    imap.append(
        "INBOX",
        "From: Ada <ada@example.test>\r\nTo: hiring@studio.example\r\n\
Subject: Application\r\nMessage-ID: <application@example.test>\r\n\
Date: Thu, 29 Oct 2026 07:00:00 +0000\r\n\r\nHello.\r\n",
        &[],
    );
    let kept = tempfile::tempdir().unwrap();
    crate::account::AccountStore::single(crate::model::AccountConfig {
        display_name: "Studio hiring".into(),
        email: ME.into(),
        username: ME.into(),
        password: "app-password".into(),
        imap_host: "127.0.0.1".into(),
        imap_port: imap.port,
        auth: crate::model::AuthMethod::Password,
        ..Default::default()
    })
    .persist_in(kept.path())
    .unwrap();

    let mut app = TestApp::<MailShell>::open_data_dir(kept.path()).unwrap();
    app.with_surface(super::MailApp::reach_test_servers)
        .unwrap();
    let deadline = ui::testing::hang_deadline();
    // The window's first sync connects with the engine it was built with;
    app.with_surface(|mail: &mut super::MailApp| mail.reach_test_servers())
        .unwrap();
    // A window that reaches its server fetches when asked; it is asked
    // until the whole inbox is here and the sync that brought it is over.
    let mut asked = 0;
    while app
        .with_surface(|mail: &mut super::MailApp| mail.held())
        .unwrap()
        < 1
    {
        assert!(ui::time::now() < deadline, "the inbox is fetched");
        if asked % 20 == 0 {
            match app.effect(&crate::actions::MailEffect::Refresh) {
                Ok(_) => {}
                Err(tilo_actions::DoorError::Failed(why)) if why.contains("already running") => {}
                Err(error) => panic!("the inbox is fetched: {error:?}"),
            }
        }
        asked += 1;
        app.settle();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    while app
        .with_surface(|mail: &mut super::MailApp| mail.awaiting_server())
        .unwrap()
    {
        assert!(ui::time::now() < deadline, "the first sync ends");
        app.settle();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    app.settle();
    (app, imap, kept)
}

fn mark(app: &TestApp<MailShell>) -> (Vec<automerge::ChangeHash>, usize) {
    app.read_document(|doc| {
        let mut doc = doc.clone();
        (doc.get_heads(), doc.pending_ops())
    })
}

#[test]
fn an_idle_connected_window_asks_for_no_pass() {
    let (mut app, imap, _kept) = connected_window();
    let needs_pass = |app: &mut TestApp<MailShell>| {
        app.with_surface(|mail: &mut super::MailApp| mail.needs_idle_sync())
            .unwrap()
    };
    // The profile's account was connected by the window launch builds, which
    // cannot trust the test server: the notice saying so is that attempt's,
    // not this test's subject, and goes before the idle interval begins.
    app.with_surface(|mail: &mut super::MailApp| mail.nav.dismiss_toast())
        .unwrap();
    app.settle();
    // Whatever the first sync left (the notice going, a fade's last frames)
    let deadline = ui::testing::hang_deadline();
    let mut quiet = 0;
    while quiet < 50 {
        assert!(
            ui::time::now() < deadline,
            "the window stops asking for frames after its first sync"
        );
        if app.frame() {
            quiet = 0;
        } else {
            quiet += 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // Two seconds of an idle window, drawn as a shown one is: nothing
    // arrives, so nothing is drawn and no pass runs.
    let before = mark(&app);
    let mut frames = 0;
    for _ in 0..200 {
        if app.frame() {
            frames += 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(frames, 0, "an idle window draws no frame");
    assert!(!needs_pass(&mut app), "an idle window asks for no pass");
    assert_eq!(
        before,
        mark(&app),
        "an idle window writes nothing to its document"
    );
    drop(imap);
}

#[test]
fn a_connected_window_asks_for_the_pages_its_server_still_holds() {
    let (mut app, imap, _kept) = connected_window();
    let account = app
        .with_surface(|mail: &mut super::MailApp| {
            mail.shared.borrow().accounts.active_config().email
        })
        .unwrap();
    // What the server says once the window knows of pages behind the one
    // it holds. Asking that same pass what it owes consumes the want, so
    // the answer and the questions it was made of are read together.
    let asked = app
        .with_surface(|mail: &mut super::MailApp| {
            mail.older_pending.clear();
            mail.has_older
                .insert((account.clone(), crate::model::Folder::Inbox), true);
            mail.shared.borrow().wants_older.set(true);
            let wanted = mail.older_pages_wanted().len();
            (
                wanted,
                mail.needs_idle_sync(),
                mail.state().engine.is_some(),
            )
        })
        .unwrap();
    assert_eq!(
        asked,
        (1, true, true),
        "the page before is still to be asked for"
    );
    drop(imap);
}

#[test]
fn a_refused_account_save_is_said_once_and_the_window_goes_idle() {
    let (mut app, imap, kept) = connected_window();
    std::fs::write(
        accounts::vault_dir_in(kept.path()).join("vault.enc"),
        b"damaged fixture vault",
    )
    .unwrap();
    app.with_surface(|mail: &mut super::MailApp| {
        mail.nav.dismiss_toast();
        mail.persisted_active_account = None;
    })
    .unwrap();
    app.settle();
    let said = app
        .with_surface(|mail: &mut super::MailApp| mail.nav.toast_text().map(str::to_owned))
        .unwrap();
    assert!(
        said.as_deref()
            .is_some_and(|text| text.contains("could not save that preference")),
        "the refusal is said: {said:?}"
    );
    app.with_surface(|mail: &mut super::MailApp| mail.nav.dismiss_toast())
        .unwrap();
    let deadline = ui::testing::hang_deadline();
    let mut quiet = 0;
    while quiet < 50 {
        assert!(
            ui::time::now() < deadline,
            "the window stops asking for frames once the refusal is said"
        );
        if app.frame() {
            quiet = 0;
        } else {
            quiet += 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let (pass, notice) = app
        .with_surface(|mail: &mut super::MailApp| {
            (
                mail.needs_idle_sync(),
                mail.nav.toast_text().map(str::to_owned),
            )
        })
        .unwrap();
    assert!(!pass, "a said refusal asks for no pass");
    assert_eq!(notice, None, "and is not put back up");
    drop(imap);
}
