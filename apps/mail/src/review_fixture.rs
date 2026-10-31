//! The day's applications, as a test reviews them: eight candidates who
//! each sent an application with a one-page PDF CV, and an invoice, on a
//! test IMAP server; fetched once into a Mail profile and kept on the
//! device, so a Mail window opened afresh answers from what it keeps.
//!
//! For Mail's own tests and other crates' (the desktop's), which drive the
//! review through the action door.

use std::path::Path;

use ui::framework::testing::TestApp;

use crate::app::MailApp;
use crate::fake_servers::{FakeImap, FakeSmtp, Layout, server_tls_config};
use crate::model::{AccountConfig, AuthMethod};
use crate::resource::MailShell;

/// The account the applications came to.
pub const ME: &str = "hiring@studio.example";

/// The candidates: name and address.
pub const CANDIDATES: [(&str, &str); 8] = [
    ("Ada Lovelace", "ada@lovelace.example"),
    ("Grace Hopper", "grace@hopper.example"),
    ("Alan Turing", "alan@turing.example"),
    ("Edsger Dijkstra", "edsger@dijkstra.example"),
    ("Barbara Liskov", "barbara@liskov.example"),
    ("Donald Knuth", "don@knuth.example"),
    ("Frances Allen", "fran@allen.example"),
    ("Ken Thompson", "ken@thompson.example"),
];

/// A one-page PDF whose text is `line`.
#[must_use]
pub fn cv_pdf(line: &str) -> Vec<u8> {
    use std::fmt::Write as _;
    let content = format!("BT /F1 14 Tf 56 740 Td ({line}) Tj ET\n");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [4 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 3 0 R >> >> /Contents 5 0 R >>"
            .to_owned(),
        format!(
            "<< /Length {} >>\nstream\n{content}endstream",
            content.len()
        ),
    ];
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        let _ = write!(out, "{} 0 obj\n{object}\nendobj\n", i + 1);
    }
    let xref = out.len();
    let _ = write!(out, "xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
    for offset in offsets {
        let _ = writeln!(out, "{offset:010} 00000 n ");
    }
    let _ = write!(
        out,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    );
    out.into_bytes()
}

/// The line `name`'s CV says.
#[must_use]
pub fn cv_line(name: &str) -> String {
    format!("Curriculum vitae of {name}")
}

/// `name`'s application, as the server holds it: a text part with links,
/// and the CV attached.
#[must_use]
pub fn application(index: usize, name: &str, address: &str) -> String {
    let handle = address.split('@').next().unwrap_or(address);
    mail_builder::MessageBuilder::new()
        .from((name, address))
        .to(ME)
        .subject(format!("Application: product engineer ({name})"))
        .message_id(format!("application-{index}@{handle}.example"))
        .date(1_793_260_800_i64 + i64::try_from(index).unwrap_or_default() * 600)
        .text_body(format!(
            "Hello,\n\nI'd like to apply for the product engineer role. My CV is attached.\n\
             GitHub: https://github.com/{handle}\nPortfolio: https://{handle}.example/\n\n{name}\n"
        ))
        .attachment(
            "application/pdf",
            format!("{handle}-cv.pdf"),
            cv_pdf(&cv_line(name)),
        )
        .write_to_string()
        .unwrap_or_default()
}

/// The test servers, holding the applications and an invoice, and the
/// account that reaches them.
///
/// # Panics
/// A test server could not start.
#[must_use]
pub fn servers() -> (FakeImap, FakeSmtp, AccountConfig) {
    let rt = ui::rt::app();
    let imap = rt
        .block_on(FakeImap::start(Layout::Gmail, server_tls_config()))
        .unwrap();
    let smtp = rt.block_on(FakeSmtp::start(server_tls_config())).unwrap();
    for (index, (name, address)) in CANDIDATES.iter().enumerate() {
        imap.append("INBOX", &application(index, name, address), &[]);
    }
    imap.append(
        "INBOX",
        "From: Billing <billing@vendor.example>\r\nTo: hiring@studio.example\r\n\
Subject: Invoice 4411\r\nMessage-ID: <invoice-4411@vendor.example>\r\n\
Date: Thu, 29 Oct 2026 07:00:00 +0000\r\n\r\nYour invoice.\r\n",
        &[],
    );
    let account = AccountConfig {
        display_name: "Studio hiring".into(),
        email: ME.into(),
        username: ME.into(),
        password: "app-password".into(),
        imap_host: "127.0.0.1".into(),
        imap_port: imap.port,
        smtp_host: "127.0.0.1".into(),
        smtp_port: smtp.port,
        auth: AuthMethod::Password,
        ..Default::default()
    };
    (imap, smtp, account)
}

/// Start the servers, keep their account in `profile`, and fetch the inbox
/// into a Mail window there once, until this device keeps all of it.
///
/// # Panics
/// A server, the profile or the fetch failed.
#[must_use]
pub fn fetched_into(profile: &Path) -> (FakeImap, FakeSmtp) {
    let (imap, smtp, account) = servers();
    crate::account::AccountStore::single(account)
        .persist_in(profile)
        .unwrap();
    let mut app = TestApp::<MailShell>::open_data_dir(profile).unwrap();
    app.with_surface(MailApp::reach_test_servers).unwrap();
    let held = CANDIDATES.len() + 1;
    let deadline = ui::testing::hang_deadline();
    // The window may be busy with the server already (connecting, or
    // keeping its drafts there): it is asked again until it takes the ask.
    while app.with_surface(|mail: &mut MailApp| mail.held()).unwrap() < held {
        assert!(ui::time::now() < deadline, "every message is fetched");
        match app.effect(&crate::actions::MailEffect::Refresh) {
            Ok(_) => {}
            Err(tilo_actions::DoorError::Failed(why)) if why.contains("already running") => {}
            Err(error) => panic!("the inbox is fetched: {error:?}"),
        }
        app.settle();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    while app
        .with_surface(|mail: &mut MailApp| mail.kept_on_device(ME))
        .unwrap()
        < held
    {
        assert!(ui::time::now() < deadline, "the fetched mail is kept");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    (imap, smtp)
}

/// A Mail window opened afresh on `profile`, reaching no server.
///
/// # Panics
/// The window could not open.
#[must_use]
pub fn offline_window(profile: &Path) -> TestApp<MailShell> {
    let mut app = TestApp::<MailShell>::open_data_dir(profile).unwrap();
    app.with_surface(MailApp::reach_no_server).unwrap();
    app.settle();
    app
}
