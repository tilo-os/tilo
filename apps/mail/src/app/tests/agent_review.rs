//! The agent's review of the day's applications, against a test IMAP and
//! SMTP server: eight applications with a PDF CV each are fetched into
//! Mail; a Mail opened afresh, with no server reached, finds them by search
//! and reads each from what this device keeps; a reply drafted to each is a
//! draft of its own; each CV becomes workspace content the PDF viewer reads,
//! and opens through the desktop's `open` as a tab of the window the agent
//! names; each message and each draft opens in a Mail window of its own,
//! the draft's composer editing that draft; and nothing reaches the SMTP
//! server.

use std::sync::Arc;

use ui::framework::testing::TestApp;

use crate::actions::MailEffect;
use crate::resource::MailShell;
use crate::review_fixture::{CANDIDATES, ME, cv_line, cv_pdf};

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
    // A window reaches the server once, and keeps what it fetched; Mail
    // opened afresh reaches no server, and answers from what this device
    // keeps.
    let profile = tempfile::tempdir().unwrap();
    let (imap, smtp) = crate::review_fixture::fetched_into(profile.path());
    let mut app = crate::review_fixture::offline_window(profile.path());

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

    // Each CV opens through the desktop's `open`, as content, as a tab of
    // the window the agent names; each draft and each message opens in a
    // Mail window of its own, the draft's composer editing that draft.
    let opened = cvs_open_through_the_desktop(&mut app, &messages, &handed_over);
    assert_eq!(opened, CANDIDATES.len());
    drop(app);
    for (listed, draft) in messages.iter().zip(&drafted) {
        let message = listed["id"].as_str().unwrap();
        each_opens_in_a_window_of_its_own(profile.path(), message, draft);
    }
    drafts_reach_the_server_and_come_back_as_nothing(profile.path(), &imap);
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

/// Open every application's CV through its attachment's `open`, as the
/// agent, as a tab of the window group `4242`: Mail hands it over as
/// content and asks the desktop's `open` for it, which a stand-in desktop
/// answers. How many opened.
fn cvs_open_through_the_desktop(
    app: &mut TestApp<MailShell>,
    messages: &[serde_json::Value],
    handed_over: &[(&str, Vec<u8>)],
) -> usize {
    use ui::desktop::{OPEN, PROVIDER, RESOURCE};
    let desktop = Arc::new(tilo_actions::remote::TestHost::new().completing(|_| {
        tilo_actions::ActionOutput {
            value: Some(serde_json::json!({
                "window": "11", "group": "4242", "strip": "7", "app": "so.tilo.pdf-viewer",
            })),
            resources: Vec::new(),
        }
    }));
    let _desktop = ui::remote_actions::RemoteActions::enter(
        Arc::clone(&desktop) as Arc<dyn tilo_actions::remote::ActionHost + Send + Sync>,
        tilo_actions::remote::Caller::local_agent("planner"),
    );
    let mut opened = 0;
    for (listed, (_, bytes)) in messages.iter().zip(handed_over) {
        let id = listed["id"].as_str().unwrap();
        let answer = app
            .call_by(
                tilo_actions::ActionInvoker::actor("agent:planner"),
                &format!("mail.attachment:{id}:0"),
                "open-attachment",
                serde_json::json!({ "group": "4242" }),
            )
            .expect("the CV opens through the desktop");
        let asked = desktop.take_asked();
        assert_eq!(asked.len(), 1, "one desktop open per CV: {asked:?}");
        let request = &asked[0];
        assert_eq!(request.target.provider, PROVIDER);
        assert_eq!(request.target.address.resource.as_str(), RESOURCE);
        assert_eq!(request.target.address.action, OPEN);
        let tilo_actions::remote::InvokeArgument::Json(arguments) = &request.argument else {
            panic!("the desktop's open takes typed arguments: {request:?}");
        };
        let hash = tilo_content::hash_bytes(bytes);
        assert_eq!(
            arguments["target"],
            tilo_actions::content_resource(&hash).unwrap(),
            "the CV's own content: {arguments:?}"
        );
        assert_eq!(arguments["place"], "tab-of");
        assert_eq!(arguments["group"], "4242");
        let value = answer.value.unwrap();
        assert_eq!(value["window"], "11", "the desktop's answer: {value}");
        assert_eq!(value["media_type"], "application/pdf", "{value}");
        opened += 1;
    }
    opened
}

/// A Mail window opened afresh at `message` shows it; one opened at
/// `draft` edits that draft; a draft that is not there is refused.
fn each_opens_in_a_window_of_its_own(profile: &std::path::Path, message: &str, draft: &str) {
    let mut window = crate::review_fixture::offline_window(profile);
    window
        .call(
            "open",
            serde_json::json!({ "anchor": format!("mail.message:{message}") }),
        )
        .expect("the message opens");
    window.settle();
    let shown = window
        .with_surface(|mail: &mut super::MailApp| mail.shown_message())
        .unwrap();
    assert_eq!(shown.as_deref(), Some(message));

    let mut window = crate::review_fixture::offline_window(profile);
    window
        .call(
            "open",
            serde_json::json!({ "anchor": format!("mail.draft:{draft}") }),
        )
        .expect("the draft opens");
    window.settle();
    let (editing, subject) = window
        .with_surface(|mail: &mut super::MailApp| {
            (mail.composing_draft(), mail.compose.subject_text())
        })
        .unwrap();
    assert_eq!(
        editing.as_deref(),
        Some(draft),
        "its composer edits that draft"
    );
    let kept = window
        .state()
        .draft_at(Some(draft))
        .unwrap()
        .subject
        .clone();
    assert_eq!(subject, kept);
    assert!(
        window
            .call(
                "open",
                serde_json::json!({ "anchor": "mail.draft:no-such-draft" })
            )
            .is_err(),
        "a draft that is not there is refused"
    );
}

/// Each draft is published as the item its verbs answer
/// (`mail.draft:<id>`, of kind `mail.draft`), so the desktop finds the app
/// that opens it; it goes when the draft does.
#[test]
fn each_draft_is_published_as_the_item_its_verbs_name() {
    let profile = tempfile::tempdir().unwrap();
    let mut app = TestApp::<MailShell>::open_data_dir(profile.path()).unwrap();
    app.settle();
    let made = app
        .call(
            "draft-message",
            serde_json::json!({ "to": "ada@lovelace.example", "subject": "Hi", "body": "Hello" }),
        )
        .unwrap()
        .unwrap();
    let item = made["draft"]["item"].as_str().unwrap().to_owned();
    app.settle();
    let published = |app: &mut TestApp<MailShell>| {
        app.actions()
            .unwrap()
            .into_iter()
            .filter(|action| action.address.resource.as_str() == item)
            .map(|action| (action.resource_kind, action.address.action))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        published(&mut app),
        [("mail.draft".to_owned(), "open".to_owned())],
        "{item}"
    );
    app.call_confirmed("discard-draft", serde_json::json!({ "draft": item }))
        .expect("the draft is discarded");
    app.settle();
    assert!(
        published(&mut app).is_empty(),
        "a discarded draft is not published"
    );
}

/// The drafts made in a window that reached no server are kept in the
/// server's Drafts folder by the first window that reaches it, marked as
/// drafts and without their notes. Fetched back with Drafts, each copy is
/// the draft itself: it becomes no message, no entry in the record and
/// nothing kept on the device. And the window fetches when asked once its
/// first sync is over.
fn drafts_reach_the_server_and_come_back_as_nothing(
    profile: &std::path::Path,
    imap: &crate::fake_servers::FakeImap,
) {
    const DRAFTS: &str = "[Gmail]/Drafts";
    let mut app = TestApp::<MailShell>::open_data_dir(profile).unwrap();
    app.with_surface(super::MailApp::reach_test_servers)
        .unwrap();
    let deadline = ui::testing::hang_deadline();
    while imap.messages(DRAFTS).len() < CANDIDATES.len() {
        assert!(ui::time::now() < deadline, "the drafts reach the server");
        app.settle();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let copies = imap.messages(DRAFTS);
    assert_eq!(copies.len(), CANDIDATES.len(), "one copy of each draft");
    for copy in &copies {
        let raw = String::from_utf8_lossy(&copy.raw).to_string();
        assert!(
            copy.flags.iter().any(|flag| flag == "\\Draft"),
            "{:?}",
            copy.flags
        );
        assert!(raw.contains("Re: Application"), "{raw}");
        assert!(
            !raw.contains("Review of"),
            "the notes are never in the copy: {raw}"
        );
    }
    while app
        .with_surface(|mail: &mut super::MailApp| mail.awaiting_server())
        .unwrap()
    {
        assert!(ui::time::now() < deadline, "the window's first sync ends");
        app.settle();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let counts = |app: &mut TestApp<MailShell>| {
        app.with_surface(|mail: &mut super::MailApp| (mail.held(), mail.kept_on_device(ME)))
            .unwrap()
    };
    let before = counts(&mut app);
    app.call(
        "open",
        serde_json::json!({ "anchor": crate::resource::folder_resource("drafts") }),
    )
    .expect("Drafts opens");
    app.settle();
    while app
        .with_surface(|mail: &mut super::MailApp| mail.awaiting_server())
        .unwrap()
    {
        assert!(ui::time::now() < deadline, "opening Drafts ends");
        app.settle();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    app.effect(&MailEffect::Refresh)
        .expect("a window that reaches its server fetches when asked");
    app.settle();
    assert!(
        app.with_surface(|mail: &mut super::MailApp| {
            mail.shared
                .borrow()
                .passed_over
                .contains_key(&(ME.to_owned(), crate::model::Folder::Drafts))
        })
        .unwrap(),
        "Drafts was fetched with the copies in it"
    );
    assert_eq!(
        counts(&mut app),
        before,
        "the copies fetched back are no messages, and nothing is kept of them"
    );
    let mail = app.state();
    let ids = mail.draft_copy_ids();
    assert!(ids.len() >= CANDIDATES.len(), "{ids:?}");
    for copy in ids {
        assert!(
            mail.messages
                .get(&ui::tilo_resource::Id::from_raw(copy.as_str()))
                .is_none(),
            "no entry in the record for the copy {copy}"
        );
    }
}
