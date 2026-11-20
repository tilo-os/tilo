//! Mail held in the workspace, as an agent uses it: delivered, read, searched,
//! drafted to and exported, without an account and never sent.

use serde_json::json;
use ui::framework::testing::TestApp;

use crate::resource::MailShell;

/// The address the mailbox's messages are published at, beside it.
fn messages(app: &mut TestApp<MailShell>) -> String {
    crate::resource::TestMail::messages_resource(app)
        .id
        .as_str()
        .to_owned()
}

fn deliver(app: &mut TestApp<MailShell>, message: serde_json::Value) -> serde_json::Value {
    let at = messages(app);
    let answer = app
        .call_on(&at, "deliver-message", message)
        .expect("delivered")
        .expect("an answer");
    app.settle();
    answer
}

/// The one message the scenarios below deliver, as a delivery takes it.
fn quarterly() -> serde_json::Value {
    json!({
        "from": "Sam Lee <sam@example.test>",
        "to": "me@example.test",
        "subject": "Quarterly numbers",
        "body": "They are up.\nSee the attached summary.",
        "date": "2026-10-12T09:30:00Z",
    })
}

fn sample(app: &mut TestApp<MailShell>) -> String {
    deliver(app, quarterly())["id"].as_str().unwrap().to_owned()
}

fn exported(app: &TestApp<MailShell>, answer: &serde_json::Value) -> Vec<u8> {
    let hash = ui::tilo_actions::content_hash(answer["content"].as_str().unwrap()).unwrap();
    app.content().read(hash).unwrap()
}

tilo_test_env::scenario! {
    /// Mail held in the workspace, as an agent delivers, reads and searches it and the window shows it.
    fn held_mail_is_delivered_read_searched_and_shown;

    phase a_delivered_message_is_held_in_the_inbox_unread_with_a_deterministic_id;
    {
        let mut app = TestApp::<MailShell>::new().unwrap();
        let id = sample(&mut app);
        let state = crate::resource::TestMail::held_by(&app);
        let message = state.message(&id).expect("its state");
        assert_eq!(message.folder, crate::model::Folder::Inbox);
        assert!(!message.read);
        assert!(message.server.is_none(), "nothing is queued for a server");
        assert!(crate::resource::TestMail::owed_by(&app).is_empty());
        let at = messages(&mut app);
        let again = app.call_on(&at, "deliver-message", quarterly());
        assert!(
            again.unwrap_err().to_string().contains(&id),
            "the same message is recognised, by the same id"
        );
    }

    phase read_lists_held_mail_and_shows_one_message_with_its_conversation;
    {
        let mut app = TestApp::<MailShell>::new().unwrap();
        let first = sample(&mut app);
        let reply = deliver(
            &mut app,
            json!({
                "from": "me@example.test",
                "to": "Sam Lee <sam@example.test>",
                "subject": "Re: Quarterly numbers",
                "body": "Great, thanks.",
                "date": "2026-10-12T10:00:00Z",
                "folder": "sent",
                "read": true,
            }),
        );
        let at = messages(&mut app);
        let listing = app.call_on(&at, "read", json!({})).unwrap().unwrap();
        let held = listing["held"].as_array().unwrap();
        assert_eq!(held.len(), 2);
        assert_eq!(
            held[0]["value"]["subject"], "Re: Quarterly numbers",
            "newest first"
        );
        let item = held[1]["id"].as_str().unwrap().to_owned();
        assert_eq!(item, first);
        let one = app
            .call_on(&at, "read", json!({ "item": item }))
            .unwrap()
            .unwrap();
        assert_eq!(
            one["message"]["body"],
            "They are up.\nSee the attached summary."
        );
        assert_eq!(
            reply["conversation"], one["message"]["conversation"],
            "a reply about the same subject joins the conversation"
        );
        assert_eq!(one["conversation"].as_array().unwrap().len(), 2);
    }

    phase search_messages_finds_held_mail_by_every_word;
    {
        let mut app = TestApp::<MailShell>::new().unwrap();
        let id = sample(&mut app);
        deliver(
            &mut app,
            json!({ "from": "ops@example.test", "subject": "Outage", "body": "The server is down." }),
        );
        let found = app
            .call("search-messages", json!({ "query": "numbers SAM" }))
            .unwrap()
            .unwrap();
        let found = found["messages"].as_array().unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0]["id"], id.as_str());
        let none = app
            .call(
                "search-messages",
                json!({ "query": "numbers", "folder": "archive" }),
            )
            .unwrap()
            .unwrap();
        assert!(none["messages"].as_array().unwrap().is_empty());
    }

    phase held_mail_shows_in_the_window_and_moves_with_the_mailbox_verbs;
    {
        let mut app = TestApp::<MailShell>::new().unwrap();
        let id = sample(&mut app);
        let at = messages(&mut app);
        app.call_on(&at, "archive", json!({ "messages": [&id] })).unwrap();
        app.settle();
        let message_id: crate::model::MessageId = id.parse().unwrap();
        let shown = app
            .with_surface(|mail: &mut crate::app::MailApp| {
                mail.shared
                    .borrow()
                    .mailbox
                    .get(message_id)
                    .map(|message| (message.subject.clone(), message.folder))
            })
            .flatten();
        assert_eq!(
            shown,
            Some((
                "Quarterly numbers".to_owned(),
                crate::model::Folder::Archive
            ))
        );
    }

    // A message's history is its part's: the delivery and the move are
    // steps there, beside the mailbox itself.
    phase a_messages_history_is_kept_in_its_part;
    {
        let mut app = TestApp::<MailShell>::new().unwrap();
        let id = deliver(
            &mut app,
            json!({
                "from": "Sam Lee <sam@example.test>",
                "to": "me@example.test",
                "subject": "Marker kumquat parade",
                "body": "Bring the kumquats.",
                "date": "2026-10-12T09:30:00Z",
            }),
        )["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let at = messages(&mut app);
        app.call_on(&at, "archive", json!({ "messages": [&id] })).unwrap();
        app.settle();
        let history = app.chain_history::<crate::message_states::MessageStates>();
        assert!(history.len() >= 2, "{history:?}");
        let held = crate::resource::TestMail::held_by(&app);
        assert_eq!(
            held.message(&id).map(|message| message.folder),
            Some(crate::model::Folder::Archive)
        );
    }
}

fn draft(app: &mut TestApp<MailShell>, subject: &str, body: &str) -> Result<(), String> {
    let drafted = app.call(
        "draft-message",
        json!({ "to": "Ana <ana@example.test>", "subject": subject, "body": body }),
    );
    app.settle();
    drafted.map(drop).map_err(|refused| refused.to_string())
}

fn save_as_a_person(app: &mut TestApp<MailShell>, subject: &str, body: &str) {
    app.call(
        "save-draft",
        json!({ "message": {
            "to": "Ana <ana@example.test>", "cc": "", "bcc": "", "subject": subject, "body": body,
        } }),
    )
    .unwrap();
    app.settle();
}

fn drafts(app: &TestApp<MailShell>) -> Vec<(String, crate::resource::DraftState)> {
    app.state()
        .drafts
        .iter()
        .map(|(id, draft)| (id.as_str().to_owned(), draft.clone()))
        .collect()
}

tilo_test_env::scenario! {
    /// Drafts an agent starts beside held mail: never sent, never over a draft a person is writing.
    fn drafting_to_held_mail_makes_drafts_of_their_own;

    phase draft_reply_starts_a_draft_of_its_own_quoting_the_original_and_sends_nothing;
    {
        let mut app = TestApp::<MailShell>::new().unwrap();
        let id = sample(&mut app);
        let answer = app
            .call(
                "draft-reply",
                json!({ "message": id, "body": "Thanks Sam, looks good.", "notes": "Numbers check out." }),
            )
            .unwrap()
            .expect("the draft is answered");
        app.settle();
        let [(draft_id, draft)]: [(String, crate::resource::DraftState); 1] =
            drafts(&app).try_into().expect("one draft");
        assert_eq!(answer["draft"]["id"], draft_id.as_str(), "{answer}");
        assert_eq!(answer["draft"]["item"], format!("mail.draft:{draft_id}"));
        assert_eq!(draft.to, "Sam Lee <sam@example.test>");
        assert_eq!(draft.subject, "Re: Quarterly numbers");
        assert!(draft.body.starts_with("Thanks Sam, looks good."));
        assert!(draft.body.contains("> They are up."));
        assert_eq!(draft.in_reply_to.as_deref(), Some(id.as_str()));
        assert_eq!(draft.notes, "Numbers check out.");
        assert!(draft.rich_body.is_some(), "the composer opens on the text");
        let state = app.state();
        assert!(
            state.draft.is_empty(),
            "the composer's own draft is untouched"
        );
        assert!(state.outgoing().is_empty(), "nothing is sent");
    }

    phase every_reply_and_message_drafted_is_a_draft_of_its_own;
    {
        let mut app = TestApp::<MailShell>::new().unwrap();
        let id = sample(&mut app);
        for body in ["Thanks Sam.", "Thanks Sam, looks good."] {
            app.call("draft-reply", json!({ "message": id, "body": body }))
                .unwrap();
            app.settle();
        }
        let answer = app
            .call(
                "draft-message",
                json!({ "to": "Ana <ana@example.test>", "subject": "Lunch", "body": "Friday?", "cc": "bo@example.test", "notes": "Ask about Monday too." }),
            )
            .unwrap()
            .expect("the draft is answered");
        app.settle();
        assert_eq!(answer["draft"]["subject"], "Lunch", "{answer}");
        let all = drafts(&app);
        assert_eq!(all.len(), 3, "nothing is written over");
        let lunch = &all
            .iter()
            .find(|(id, _)| answer["draft"]["id"] == id.as_str())
            .expect("the answered draft")
            .1;
        assert_eq!(
            (
                lunch.to.as_str(),
                lunch.cc.as_str(),
                lunch.body.as_str(),
                lunch.notes.as_str()
            ),
            (
                "Ana <ana@example.test>",
                "bo@example.test",
                "Friday?",
                "Ask about Monday too."
            )
        );
    }

    phase a_draft_a_person_is_writing_is_left_alone_beside_new_ones;
    {
        let mut app = TestApp::<MailShell>::new().unwrap();
        save_as_a_person(&mut app, "Lunch", "Friday? Or Monday.");
        draft(&mut app, "Lunch", "Monday?").unwrap();
        assert_eq!(app.state().draft.body, "Friday? Or Monday.");
        let [(id, _)]: [(String, crate::resource::DraftState); 1] =
            drafts(&app).try_into().expect("one draft");
        app.call_confirmed("discard-draft", json!({ "draft": id }))
            .unwrap();
        app.settle();
        assert!(drafts(&app).is_empty());
        assert_eq!(app.state().draft.body, "Friday? Or Monday.");
        app.call_confirmed("discard-draft", json!({})).unwrap();
        assert!(app.state().draft.is_empty());
    }
}

tilo_test_env::scenario! {
    /// A message delivered twice, as a migration that resumed asks again.
    fn a_message_delivered_twice_answers_with_its_code;

    phase delivering_the_same_message_again_answers_message_already_held;
    {
        let mut app = TestApp::<MailShell>::new().unwrap();
        let id = sample(&mut app);
        let at = messages(&mut app);
        let refusal = app
            .call_on(&at, "deliver-message", quarterly())
            .expect_err("the mailbox holds it already");
        assert_eq!(
            refusal.code(),
            ui::tilo_actions::codes::MESSAGE_ALREADY_HELD,
            "a caller deciding on the answer reads its code, not its words: {refusal}"
        );
        assert!(refusal.to_string().contains(&id), "{refusal}");
    }
}

tilo_test_env::scenario! {
    /// Exporting held mail and the draft being written.
    fn export_content_keeps_held_mail_and_the_draft_being_written;

    phase export_content_carries_the_draft_being_written_marked_a_draft;
    {
        let mut app = TestApp::<MailShell>::new().unwrap();
        sample(&mut app);
        app.call(
            "draft-message",
            json!({ "to": "Ana <ana@example.test>", "subject": "Lunch", "body": "Friday?" }),
        )
        .unwrap();
        let md = app.call("export-content", json!({})).unwrap().unwrap();
        assert_eq!(
            (md["messages"].as_u64(), md["drafts"].as_u64()),
            (Some(2), Some(1))
        );
        let text = String::from_utf8(exported(&app, &md)).unwrap();
        assert!(
            text.contains("## Lunch (draft)") && text.contains("- To: Ana <ana@example.test>"),
            "{text}"
        );
        assert!(
            text.contains("Friday?") && text.contains("## Quarterly numbers"),
            "{text}"
        );
        let drafts = app
            .call(
                "export-content",
                json!({ "format": "json", "folder": "drafts" }),
            )
            .unwrap()
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&exported(&app, &drafts)).unwrap();
        assert_eq!(parsed.as_array().map(Vec::len), Some(1), "{parsed}");
        assert_eq!(parsed[0]["draft"], true);
        assert_eq!(parsed[0]["subject"], "Lunch");
        let inbox = app
            .call("export-content", json!({ "folder": "inbox" }))
            .unwrap()
            .unwrap();
        assert_eq!(inbox["drafts"], 0, "the inbox holds no draft");
    }

    phase export_content_keeps_held_mail_as_markdown_json_or_eml;
    {
        let mut app = TestApp::<MailShell>::new().unwrap();
        let id = sample(&mut app);
        let md = app.call("export-content", json!({})).unwrap().unwrap();
        assert_eq!(md["messages"], 1);
        let text = String::from_utf8(exported(&app, &md)).unwrap();
        assert!(text.contains("## Quarterly numbers") && text.contains("They are up."));
        let json = app
            .call("export-content", json!({ "format": "json" }))
            .unwrap()
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&exported(&app, &json)).unwrap();
        assert_eq!(parsed[0]["from"], "Sam Lee <sam@example.test>");
        let eml = app
            .call("export-content", json!({ "format": "eml", "message": id }))
            .unwrap()
            .unwrap();
        let bytes = exported(&app, &eml);
        let parsed = mail_parser::MessageParser::default().parse(&bytes).unwrap();
        assert_eq!(parsed.subject(), Some("Quarterly numbers"));
        deliver(
            &mut app,
            json!({ "from": "ops@example.test", "subject": "Outage", "body": "Down." }),
        );
        assert!(
            app.call("export-content", json!({ "format": "eml" }))
                .is_err(),
            "an .eml holds one message: several are refused"
        );
    }
}

#[test]
fn the_window_bars_sidebar_button_flips_the_sidebar() {
    ui::framework::testing::check_window_sidebar_button(|| {
        ui::framework::testing::TestApp::<MailShell>::drive("wide")
    })
    .unwrap_or_else(|error| panic!("{error}"));
}

/// `message` delivered as an importer delivers one: what remains of it and
/// each large part's pieces kept as content, named by reference.
fn deliver_apart(app: &mut TestApp<MailShell>, message: &[u8]) -> serde_json::Value {
    let split = tilo_mail_parts::split(message);
    assert!(!split.apart.is_empty(), "the message has large parts");
    let mut keep = |bytes: &[u8]| format!("tilo.content:{}", app.content().put(bytes).unwrap());
    let original = keep(&split.rest);
    let parts: Vec<serde_json::Value> = split
        .apart
        .iter()
        .map(|part| {
            json!({
                "at": part.at,
                "size": part.size,
                "pieces": part.pieces().map(&mut keep).collect::<Vec<_>>(),
            })
        })
        .collect();
    deliver(
        app,
        json!({
            "from": "Grace Hopper <grace@example.net>",
            "subject": "Plans",
            "body": "The files are attached.",
            "date": "2026-11-03T10:00:00Z",
            "message_id": "plans@example.net",
            "original": original,
            "parts": parts,
        }),
    )
}

#[test]
fn a_message_with_large_attachments_is_kept_with_its_parts_apart_and_opens_whole() {
    use tilo_mail_parts::fixture::{attachment_bytes, message_with_attachments};
    let files = [
        ("plans.pdf", 3 * 1024 * 1024),
        ("photo.jpg", 700 * 1024),
        ("note.txt", 200),
    ];
    let message = message_with_attachments("plans@example.net", "Plans", &files);
    let mut app = TestApp::<MailShell>::new().unwrap();
    let id = deliver_apart(&mut app, &message)["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // The held message names its parts; no one piece of content is the message.
    let held = crate::resource::TestMail::held_by(&app);
    let (_, kept) = held.held_message(&id).unwrap();
    assert_eq!(
        kept.parts.len(),
        2,
        "the two large attachments are kept apart"
    );
    assert!(kept.original.as_ref().unwrap().len() < 16 * 1024);

    // An agent reading it sees every attachment by name and size.
    let at = messages(&mut app);
    let read = app
        .call_on(&at, "read", json!({ "item": &id }))
        .unwrap()
        .unwrap();
    let listed: Vec<(String, u64)> = read["message"]["attachments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| {
            (
                file["name"].as_str().unwrap().to_owned(),
                file["size"].as_u64().unwrap(),
            )
        })
        .collect();
    let expected: Vec<(String, u64)> = files
        .iter()
        .map(|(name, size)| ((*name).to_owned(), *size as u64))
        .collect();
    assert_eq!(listed, expected);

    // The window shows it whole, and each attachment opens with its bytes.
    let message_id: crate::model::MessageId = id.parse().unwrap();
    app.with_surface(|mail: &mut crate::app::MailApp| {
        let shown = mail
            .shared
            .borrow()
            .mailbox
            .get(message_id)
            .cloned()
            .unwrap();
        assert!(shown.body.contains("attached"), "{}", shown.body);
        assert!(
            shown
                .attachments
                .iter()
                .all(crate::model::Attachment::is_downloadable)
        );
    })
    .unwrap();
    for (index, (name, size)) in files.iter().enumerate() {
        let handed = app
            .effect_on(
                &format!("mail.attachment:{id}:{index}"),
                &crate::actions::MailEffect::Content {
                    message: id.clone(),
                    index,
                },
            )
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let answer = handed.value.unwrap();
        let hash = ui::tilo_actions::content_hash(answer["content"].as_str().unwrap()).unwrap();
        let bytes = app.content().read(hash).unwrap();
        assert!(
            bytes == attachment_bytes(name, *size),
            "{name}: {} bytes",
            bytes.len()
        );
    }

    // Exported, it is the message that was sent, byte for byte.
    let eml = app
        .call("export-content", json!({ "format": "eml", "message": &id }))
        .unwrap()
        .unwrap();
    let eml = exported(&app, &eml);
    assert!(eml == message, "{} of {} bytes", eml.len(), message.len());
}

#[test]
fn an_attachment_and_an_export_larger_than_one_piece_of_content_open_whole() {
    use tilo_mail_parts::fixture::{attachment_bytes, message_with_attachments};
    // Past what one attachment opened (32 MiB) and what one export (64 MiB) used to be.
    let size = 70 * 1024 * 1024;
    let message = message_with_attachments("scan@example.net", "Scan", &[("scan.pdf", size)]);
    let mut app = TestApp::<MailShell>::new().unwrap();
    let id = deliver_apart(&mut app, &message)["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let handed = app
        .effect_on(
            &format!("mail.attachment:{id}:0"),
            &crate::actions::MailEffect::Content {
                message: id.clone(),
                index: 0,
            },
        )
        .unwrap()
        .value
        .unwrap();
    assert_eq!(handed["size"].as_u64(), Some(size as u64), "{handed}");
    let hash = ui::tilo_actions::content_hash(handed["content"].as_str().unwrap()).unwrap();
    assert_eq!(app.content().size(hash), Some(size as u64));
    let bytes = app.content().read(hash).unwrap();
    assert!(
        bytes == attachment_bytes("scan.pdf", size),
        "{} bytes",
        bytes.len()
    );

    let eml = app
        .call("export-content", json!({ "format": "eml", "message": &id }))
        .unwrap()
        .unwrap();
    assert_eq!(eml["bytes"].as_u64(), Some(message.len() as u64), "{eml}");
    let eml = exported(&app, &eml);
    assert!(eml == message, "{} of {} bytes", eml.len(), message.len());
}

#[test]
fn a_message_whose_part_is_not_here_exports_once_it_is_never_without_it() {
    use tilo_mail_parts::fixture::message_with_attachments;
    let message =
        message_with_attachments("plans@example.net", "Plans", &[("plans.pdf", 600 * 1024)]);
    let mut app = TestApp::<MailShell>::new().unwrap();
    let id = deliver_apart(&mut app, &message)["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let held = crate::resource::TestMail::held_by(&app);
    let (_, kept) = held.held_message(&id).unwrap();
    let piece = kept.parts[0].pieces[0].reference().unwrap();
    let bytes = app.content().read(&piece).unwrap();
    app.content().release(&piece).unwrap();

    // Without its attachment's bytes, the export fails rather than answering a
    // copy of the message that has lost the attachment.
    let lacking = app.call("export-content", json!({ "format": "eml", "message": &id }));
    assert!(lacking.is_err(), "{lacking:?}");

    // Once it is here again, the export is the message that was sent.
    app.content().put_as(&piece, &bytes).unwrap();
    let eml = app
        .call("export-content", json!({ "format": "eml", "message": &id }))
        .unwrap()
        .unwrap();
    let eml = exported(&app, &eml);
    assert!(eml == message, "{} of {} bytes", eml.len(), message.len());
}

#[test]
fn a_part_that_does_not_go_back_into_its_message_is_refused() {
    let mut app = TestApp::<MailShell>::new().unwrap();
    let original = format!("tilo.content:{}", app.content().put(&[b'x'; 4096]).unwrap());
    let piece = format!("tilo.content:{}", app.content().put(&[b'y'; 4096]).unwrap());
    let at = messages(&mut app);
    let refused = app.call_on(
        &at,
        "deliver-message",
        json!({
            "from": "a@example.net",
            "subject": "x",
            "body": "x",
            "original": original,
            "parts": [{ "at": 5000, "size": 1, "pieces": [piece] }],
        }),
    );
    assert!(refused.is_err(), "{refused:?}");
}

#[test]
fn a_message_lists_its_attachments_before_its_original_is_here() {
    use tilo_mail_parts::fixture::message_with_attachments;
    let message =
        message_with_attachments("plans@example.net", "Plans", &[("plans.pdf", 600 * 1024)]);
    let split = tilo_mail_parts::split(&message);
    let mut app = TestApp::<MailShell>::new().unwrap();
    let keep = |bytes: &[u8]| app.content().put(bytes).unwrap();
    let original = keep(&split.rest);
    let parts: Vec<serde_json::Value> = split
        .apart
        .iter()
        .map(|part| {
            json!({
                "at": part.at,
                "size": part.size,
                "pieces": part
                    .pieces()
                    .map(|piece| format!("tilo.content:{}", keep(piece)))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    let id = deliver(
        &mut app,
        json!({
            "from": "Grace Hopper <grace@example.net>",
            "subject": "Plans",
            "body": "The files are attached.",
            "date": "2026-11-03T10:00:00Z",
            "message_id": "plans@example.net",
            "original": format!("tilo.content:{original}"),
            "parts": parts,
            "attachments": [{"filename": "plans.pdf", "size": 600 * 1024}],
        }),
    )["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // A device that does not hold the original yet still lists the attachment, by the name and
    // size the delivery gave, with the type its name says.
    app.content().release(&original).unwrap();
    let at = messages(&mut app);
    let read = app
        .call_on(&at, "read", json!({ "item": &id }))
        .unwrap()
        .unwrap();
    assert_eq!(
        read["message"]["attachments"],
        json!([{"name": "plans.pdf", "media_type": "application/pdf", "size": 600 * 1024}]),
        "{read}"
    );
}

/// Where the page carries files out, saving an attachment kept apart names
/// the pieces it came in and their encoding, never its bytes: decoded in
/// order, they are the file.
#[test]
fn saving_an_attachment_kept_apart_names_the_pieces_it_came_in() {
    use tilo_mail_parts::fixture::{attachment_bytes, message_with_attachments};
    let size = 3 * 1024 * 1024 + 7;
    let message = message_with_attachments("link@example.net", "Scan", &[("scan.pdf", size)]);
    let mut app = TestApp::<MailShell>::new().unwrap();
    let id = deliver_apart(&mut app, &message)["id"]
        .as_str()
        .unwrap()
        .to_owned();
    app.settle();
    let store = app.content();
    let ask = app
        .with_surface(|mail: &mut crate::app::MailApp| mail.state().attachment_link(&id, 0, &store))
        .unwrap()
        .expect("an attachment kept apart is saved from its pieces");
    assert!(ask.check().is_ok(), "{ask:?}");
    assert_eq!(ask.name, "scan.pdf");
    let file = &ask.files[0];
    let encoded = file.encoded.as_ref().unwrap();
    assert_eq!(encoded.size, size as u64);
    let mut decoder = tilo_mail_parts::Decoder::named(&encoded.encoding).unwrap();
    let mut bytes = Vec::new();
    for piece in &encoded.pieces {
        bytes.extend(decoder.push(&app.content().read(piece).unwrap()).unwrap());
    }
    bytes.extend(decoder.finish().unwrap());
    assert!(
        bytes == attachment_bytes("scan.pdf", size),
        "{} bytes",
        bytes.len()
    );
}
