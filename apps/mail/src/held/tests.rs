//! Mail held in the room, as an agent uses it: delivered, read, searched,
//! drafted to and exported, without an account and never sent.

use serde_json::json;
use ui::framework::testing::TestApp;

use crate::resource::MailShell;

fn deliver(app: &mut TestApp<MailShell>, message: serde_json::Value) -> serde_json::Value {
    let answer = app
        .call("deliver-message", message)
        .expect("delivered")
        .expect("an answer");
    app.settle();
    answer
}

fn sample(app: &mut TestApp<MailShell>) -> String {
    deliver(
        app,
        json!({
            "from": "Sam Lee <sam@example.test>",
            "to": "me@example.test",
            "subject": "Quarterly numbers",
            "body": "They are up.\nSee the attached summary.",
            "date": "2026-10-12T09:30:00Z",
        }),
    )["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn exported(app: &TestApp<MailShell>, answer: &serde_json::Value) -> Vec<u8> {
    let hash = ui::tilo_actions::content_hash(answer["content"].as_str().unwrap()).unwrap();
    app.content().read(hash).unwrap()
}

#[test]
fn a_delivered_message_is_held_in_the_inbox_unread_with_a_deterministic_id() {
    let mut app = TestApp::<MailShell>::new().unwrap();
    let id = sample(&mut app);
    let state = app.state();
    let message = state.message(&id).expect("its state");
    assert_eq!(message.folder, crate::model::Folder::Inbox);
    assert!(!message.read);
    assert!(message.server.is_none(), "nothing is queued for a server");
    assert!(state.server_updates().is_empty());
    let again = app.call(
        "deliver-message",
        json!({
            "from": "Sam Lee <sam@example.test>",
            "to": "me@example.test",
            "subject": "Quarterly numbers",
            "body": "They are up.\nSee the attached summary.",
            "date": "2026-10-12T09:30:00Z",
        }),
    );
    assert!(
        again.unwrap_err().to_string().contains(&id),
        "the same message is recognised, by the same id"
    );
}

#[test]
fn read_lists_held_mail_and_shows_one_message_with_its_conversation() {
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
    let listing = app.call("read", json!({})).unwrap().unwrap();
    let messages = listing["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[0]["subject"], "Re: Quarterly numbers",
        "newest first"
    );
    assert_eq!(
        reply["conversation"], messages[1]["conversation"],
        "a reply about the same subject joins the conversation"
    );
    let one = app
        .call("read", json!({ "item": format!("mail.message:{first}") }))
        .unwrap()
        .unwrap();
    assert_eq!(
        one["message"]["body"],
        "They are up.\nSee the attached summary."
    );
    assert_eq!(one["message"]["date"], "2026-10-12T09:30:00Z");
    assert_eq!(one["conversation"].as_array().unwrap().len(), 2);
}

#[test]
fn search_messages_finds_held_mail_by_every_word() {
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

fn draft(app: &mut TestApp<MailShell>, subject: &str, body: &str) -> Result<(), String> {
    let drafted = app.call(
        "draft-message",
        json!({ "to": "Ana <ana@example.test>", "subject": subject, "body": body }),
    );
    app.settle();
    drafted.map(drop).map_err(|refused| refused.to_string())
}

/// A person saving the draft as the composer does when it closes.
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

/// The drafts of their own, by id, oldest first.
fn drafts(app: &TestApp<MailShell>) -> Vec<(String, crate::resource::DraftState)> {
    app.state()
        .drafts
        .iter()
        .map(|(id, draft)| (id.as_str().to_owned(), draft.clone()))
        .collect()
}

#[test]
fn draft_reply_starts_a_draft_of_its_own_quoting_the_original_and_sends_nothing() {
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

#[test]
fn every_reply_and_message_drafted_is_a_draft_of_its_own() {
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

#[test]
fn a_draft_a_person_is_writing_is_left_alone_beside_new_ones() {
    let mut app = TestApp::<MailShell>::new().unwrap();
    save_as_a_person(&mut app, "Lunch", "Friday? Or Monday.");
    draft(&mut app, "Lunch", "Monday?").unwrap();
    assert_eq!(app.state().draft.body, "Friday? Or Monday.");
    let [(id, _)]: [(String, crate::resource::DraftState); 1] =
        drafts(&app).try_into().expect("one draft");
    // Each is discarded on its own.
    app.call_confirmed("discard-draft", json!({ "draft": id }))
        .unwrap();
    app.settle();
    assert!(drafts(&app).is_empty());
    assert_eq!(app.state().draft.body, "Friday? Or Monday.");
    app.call_confirmed("discard-draft", json!({})).unwrap();
    assert!(app.state().draft.is_empty());
}

#[test]
fn export_content_carries_the_draft_being_written_marked_a_draft() {
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

#[test]
fn export_content_keeps_held_mail_as_markdown_json_or_eml() {
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

#[test]
fn held_mail_shows_in_the_window_and_moves_with_the_mailbox_verbs() {
    let mut app = TestApp::<MailShell>::new().unwrap();
    let id = sample(&mut app);
    app.call("archive", json!({ "messages": [&id] })).unwrap();
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

/// The window bar's sidebar button flips the sidebar on every click, in a
/// window wide enough to dock it and in a narrow one.
#[test]
fn the_window_bars_sidebar_button_flips_the_sidebar() {
    ui::framework::testing::check_window_sidebar_button(|| {
        ui::framework::testing::TestApp::<MailShell>::drive("wide")
    })
    .unwrap_or_else(|error| panic!("{error}"));
}

/// A past version reads through the `read` door with the mailbox as it stood
/// then, and draws in the app's own view.
#[test]
fn a_past_version_reads_and_shows_its_mail() {
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
    let past = app.versions().pop().unwrap().hash;
    app.call("archive", json!({ "messages": [&id] })).unwrap();
    app.settle();

    let read = app
        .call("read", json!({ "version": past.to_string() }))
        .unwrap()
        .unwrap();
    let content = &read["content"];
    assert_eq!(
        content["messages"][0]["subject"], "Marker kumquat parade",
        "{read}"
    );
    assert_eq!(content["messages"][0]["folder"], "inbox", "{read}");

    // The past view is the app's own mailbox, read-only, holding the
    // message as it was, though a past version reads no accounts.
    let shown = app.past_version(&[past]).unwrap().texts();
    assert!(
        shown
            .iter()
            .any(|text| text.contains("Marker kumquat parade")),
        "{shown:?}"
    );
}
