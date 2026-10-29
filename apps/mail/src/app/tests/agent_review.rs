//! The agent's review of the day's applications, against a test IMAP and
//! SMTP server: eight applications with a PDF CV each are fetched into
//! Mail; a Mail opened afresh, with no server reached, finds them by search
//! and reads each from what this device keeps; a reply drafted to each is a
//! draft of its own; each CV becomes workspace content the PDF viewer reads;
//! and nothing reaches the SMTP server.

use std::sync::Arc;

use ui::framework::testing::TestApp;

use crate::actions::MailEffect;
use crate::fake_servers::{FakeImap, FakeSmtp, Layout, accepting_client_config, server_tls_config};
use crate::model::{AccountConfig, AuthMethod};
use crate::resource::MailShell;

const ME: &str = "hiring@studio.example";

/// The candidates: name, address, and the line their CV opens with.
const CANDIDATES: [(&str, &str); 8] = [
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
fn cv_pdf(line: &str) -> Vec<u8> {
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

/// The CV line of `name`.
fn cv_line(name: &str) -> String {
    format!("Curriculum vitae of {name}")
}

/// `name`'s application, as the server holds it: a text part with links,
/// and the CV attached.
fn application(index: usize, name: &str, address: &str) -> String {
    let handle = address.split('@').next().unwrap_or(address);
    mail_builder::MessageBuilder::new()
        .from((name, address))
        .to(ME)
        .subject(format!("Application: product engineer ({name})"))
        .message_id(format!("application-{index}@{handle}.example"))
        .date(1_793_260_800_i64 + i64::try_from(index).unwrap() * 600)
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
        .unwrap()
}

/// The test servers and the account that reaches them.
fn servers() -> (FakeImap, FakeSmtp, AccountConfig) {
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

/// The value an answer carries.
fn value(output: tilo_actions::ActionOutput) -> serde_json::Value {
    output.value.expect("the effect answers")
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one workflow, each step building on the last"
)]
fn eight_applications_are_found_read_drafted_and_handed_to_the_pdf_viewer() {
    let (imap, smtp, account) = servers();
    let profile = tempfile::tempdir().unwrap();
    crate::account::AccountStore::single(account)
        .persist_in(profile.path())
        .unwrap();

    // A window reaches the server once, and keeps what it fetched.
    let mut app = TestApp::<MailShell>::open_data_dir(profile.path()).unwrap();
    app.with_surface(|mail: &mut super::MailApp| {
        let engine = crate::engine::live::LiveEngine::trusting(
            tilo_content::ContentStore::memory(),
            Arc::new(|| {}),
            accepting_client_config(),
        )
        .unwrap();
        let (tx, rx) = crate::engine::spawn_with(Box::new(engine));
        mail.state().engine = Some(super::EngineHandle { tx, rx });
    })
    .unwrap();
    app.effect(&MailEffect::Refresh)
        .expect("the inbox is fetched");
    app.settle();
    let kept = app
        .with_surface(|mail: &mut super::MailApp| mail.shared.borrow().mailbox.messages.len())
        .unwrap();
    assert_eq!(kept, CANDIDATES.len() + 1, "every message is fetched");
    let deadline = tilo_test_env::hang_deadline();
    while app
        .with_surface(|mail: &mut super::MailApp| mail.state().store.load(ME).len())
        .unwrap()
        < kept
    {
        assert!(ui::time::now() < deadline, "the fetched mail is kept");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    drop(app);

    // Mail opened afresh reaches no server: what it answers comes from the
    // mail this device keeps.
    let mut app = TestApp::<MailShell>::open_data_dir(profile.path()).unwrap();
    app.with_surface(|mail: &mut super::MailApp| mail.state().engine = None)
        .unwrap();
    app.settle();

    let found = value(
        app.effect(&MailEffect::SearchMessages {
            query: None,
            folder: None,
            from: None,
            subject: Some("application".into()),
            // The test server dates what it holds from 2026-01-01.
            since: Some("2026-01-01".into()),
            until: Some("2026-01-02".into()),
            has_attachment: Some(true),
        })
        .expect("the search answers"),
    );
    assert_eq!(found["found"], CANDIDATES.len(), "{found}");
    let messages = found["messages"].as_array().unwrap().clone();
    let none = value(
        app.effect(&MailEffect::SearchMessages {
            query: Some("invoice".into()),
            folder: None,
            from: None,
            subject: None,
            since: None,
            until: None,
            has_attachment: Some(true),
        })
        .unwrap(),
    );
    assert_eq!(none["found"], 0, "the invoice has no attachment: {none}");

    let mut handed_over = Vec::new();
    let mut drafted = Vec::new();
    for listed in &messages {
        let id = listed["id"].as_str().unwrap().to_owned();
        let label = listed["label"].as_str().unwrap();
        assert!(
            label.starts_with("Application: product engineer"),
            "{label}"
        );
        assert!(
            label.contains(", 2026-01-01"),
            "subject, sender and date: {label}"
        );

        let read = value(
            app.effect_on(
                &format!("mail.message:{id}"),
                &MailEffect::ReadMessage {
                    message: id.clone(),
                },
            )
            .expect("the kept message reads"),
        );
        let (name, address) = CANDIDATES
            .iter()
            .find(|(_, address)| read["from"].as_str().unwrap().contains(address))
            .copied()
            .unwrap_or_else(|| panic!("a candidate: {read}"));
        assert!(
            read["text"].as_str().unwrap().contains("My CV is attached"),
            "{read}"
        );
        assert!(
            read["links"]
                .as_array()
                .unwrap()
                .iter()
                .any(|link| link.as_str().unwrap().starts_with("https://github.com/")),
            "{read}"
        );
        let attachment = &read["attachments"][0];
        assert_eq!(attachment["type"], "application/pdf", "{read}");

        // The CV becomes workspace content, which the PDF viewer reads.
        let index = usize::try_from(attachment["index"].as_u64().unwrap()).unwrap();
        let handed = app
            .effect_on(
                attachment["item"].as_str().unwrap(),
                &MailEffect::Content {
                    message: id.clone(),
                    index,
                },
            )
            .expect("the attachment becomes content");
        let answer = handed.value.clone().unwrap();
        assert_eq!(answer["media_type"], "application/pdf", "{answer}");
        let address_of = answer["content"].as_str().unwrap();
        assert_eq!(handed.resources, [address_of.to_owned()]);
        let hash = tilo_actions::content_hash(address_of).unwrap();
        let bytes = app.content().read(hash).expect("kept in the content store");
        assert_eq!(bytes, cv_pdf(&cv_line(name)));
        handed_over.push((name, bytes));

        let reply = value(
            app.effect(&MailEffect::DraftReply {
                message: id.clone(),
                body: format!("Thank you, {name}. We'd like to talk."),
                all: None,
                notes: Some(format!("Review of {name}: strong CV.")),
            })
            .expect("a reply is drafted"),
        );
        assert_eq!(
            reply["draft"]["to"],
            format!("{name} <{address}>"),
            "{reply}"
        );
        drafted.push(reply["draft"]["id"].as_str().unwrap().to_owned());
    }
    app.settle();

    // Eight drafts of their own, each answering its message, with notes.
    let mail = app.state();
    assert_eq!(mail.drafts.len(), CANDIDATES.len());
    for id in &drafted {
        let draft = mail.draft_at(Some(id)).expect("each draft is kept");
        assert!(draft.in_reply_to.is_some());
        assert!(draft.notes.starts_with("Review of"), "{}", draft.notes);
        assert!(
            draft.subject.starts_with("Re: Application"),
            "{}",
            draft.subject
        );
        assert_eq!(
            draft.sender.as_ref().map(|sender| sender.email.as_str()),
            Some(ME),
            "a reply goes from the account the application came to"
        );
    }
    assert!(
        mail.outbox.entries().next().is_none(),
        "nothing is queued to send"
    );
    assert!(smtp.sent().is_empty(), "nothing reached the SMTP server");
    drop(imap);

    // The PDF viewer reads each CV from the content Mail handed over, as
    // the workspace's content store holds it for both.
    assert_eq!(handed_over.len(), CANDIDATES.len());
    for (name, bytes) in handed_over {
        let mut viewer = TestApp::<pdf_viewer::PdfViewerShell>::new().unwrap();
        let hash = viewer.content().put(&bytes).unwrap();
        let address = tilo_actions::content_resource(&hash).unwrap();
        value(
            viewer
                .effect(&pdf_viewer::actions::PdfEffect::ImportPdfContent {
                    content: address,
                    name: Some(format!("{name}.pdf")),
                })
                .expect("the CV opens"),
        );
        viewer.settle();
        let text = value(
            viewer
                .effect(&pdf_viewer::actions::PdfEffect::ReadText {
                    from_page: None,
                    to_page: Some(1),
                })
                .expect("the CV reads"),
        );
        assert!(text.to_string().contains(&cv_line(name)), "{text}");
    }
}
