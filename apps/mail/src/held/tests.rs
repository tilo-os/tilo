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

#[test]
fn draft_reply_writes_the_draft_quoting_the_original_and_sends_nothing() {
    let mut app = TestApp::<MailShell>::new().unwrap();
    let id = sample(&mut app);
    app.call(
        "draft-reply",
        json!({ "message": id, "body": "Thanks Sam, looks good." }),
    )
    .unwrap();
    let state = app.state();
    assert_eq!(state.draft.to, "Sam Lee <sam@example.test>");
    assert_eq!(state.draft.subject, "Re: Quarterly numbers");
    assert!(state.draft.body.starts_with("Thanks Sam, looks good."));
    assert!(state.draft.body.contains("> They are up."));
    assert!(
        state.draft.rich_body.is_some(),
        "the composer opens on the text"
    );
    assert!(state.outgoing().is_empty(), "nothing is sent");
}

#[test]
fn draft_message_writes_a_new_draft() {
    let mut app = TestApp::<MailShell>::new().unwrap();
    app.call(
        "draft-message",
        json!({ "to": "Ana <ana@example.test>", "subject": "Lunch", "body": "Friday?", "cc": "bo@example.test" }),
    )
    .unwrap();
    let draft = app.state().draft;
    assert_eq!(
        (
            draft.to.as_str(),
            draft.cc.as_str(),
            draft.subject.as_str(),
            draft.body.as_str()
        ),
        (
            "Ana <ana@example.test>",
            "bo@example.test",
            "Lunch",
            "Friday?"
        )
    );
}

/// What refuses drafting over a person's draft: no verb an agent may not
/// take.
const PERSONS_DRAFT: &str = "a person is writing a draft here";

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

fn refused_over_a_persons_draft(refused: Result<(), String>) {
    let refused = refused.expect_err("a person's draft is left alone");
    assert!(refused.contains(PERSONS_DRAFT), "{refused}");
    assert!(
        !refused.contains("save-draft") && !refused.contains("discard-draft"),
        "it names no verb an agent may not take: {refused}"
    );
}

#[test]
fn drafting_again_over_its_own_untouched_draft_replaces_it() {
    let mut app = TestApp::<MailShell>::new().unwrap();
    draft(&mut app, "Claim pending", "We still need your ID.").unwrap();
    let answer = app
        .call(
            "draft-message",
            json!({ "to": "Ana <ana@example.test>", "subject": "Claim approved", "body": "We pay EUR 239." }),
        )
        .expect("its own draft is written over")
        .expect("an answer");
    app.settle();
    assert_eq!(answer["draft"]["subject"], "Claim approved", "{answer}");
    let state = app.state();
    assert_eq!(
        (state.draft.subject.as_str(), state.draft.body.as_str()),
        ("Claim approved", "We pay EUR 239.")
    );
    let drafts = app
        .call(
            "export-content",
            json!({ "format": "json", "folder": "drafts" }),
        )
        .unwrap()
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&exported(&app, &drafts)).unwrap();
    assert_eq!(
        parsed.as_array().map(Vec::len),
        Some(1),
        "one draft: {parsed}"
    );
    assert_eq!(parsed[0]["body"], "We pay EUR 239.");
    // Over and over: each one written is its own again.
    draft(
        &mut app,
        "Claim approved",
        "We pay EUR 239.00 by 28 October.",
    )
    .unwrap();
    assert_eq!(app.state().draft.body, "We pay EUR 239.00 by 28 October.");
}

#[test]
fn a_draft_a_person_changed_after_it_was_drafted_is_left_alone() {
    // Saved by the person, as the composer saves it.
    let mut app = TestApp::<MailShell>::new().unwrap();
    draft(&mut app, "Lunch", "Friday?").unwrap();
    save_as_a_person(&mut app, "Lunch", "Friday? Or Monday.");
    refused_over_a_persons_draft(draft(&mut app, "Lunch", "Monday?"));
    assert_eq!(app.state().draft.body, "Friday? Or Monday.");

    // Its subject typed over in the composer.
    let mut app = TestApp::<MailShell>::new().unwrap();
    draft(&mut app, "Lunch", "Friday?").unwrap();
    app.call(
        "field-edit",
        json!({ "edit": crate::resource::Mail::paths().draft.subject.replace("Lunch on Friday") }),
    )
    .unwrap();
    app.settle();
    refused_over_a_persons_draft(draft(&mut app, "Lunch", "Monday?"));

    // Opened in the composer, which starts its body again to type in, the
    // same text as before.
    let mut app = TestApp::<MailShell>::new().unwrap();
    draft(&mut app, "Lunch", "Friday?").unwrap();
    app.call("start-draft-body", json!({ "text": "Friday?" }))
        .unwrap();
    app.settle();
    refused_over_a_persons_draft(draft(&mut app, "Lunch", "Monday?"));
    assert_eq!(app.state().draft.body, "Friday?");
}

#[test]
fn a_draft_a_person_is_writing_is_left_alone() {
    let mut app = TestApp::<MailShell>::new().unwrap();
    save_as_a_person(&mut app, "Lunch", "Friday?");
    refused_over_a_persons_draft(draft(&mut app, "Lunch", "Monday?"));
    // Only begun: a blank message opened in the composer.
    let mut app = TestApp::<MailShell>::new().unwrap();
    app.call("start-draft-body", json!({ "text": "" })).unwrap();
    app.settle();
    refused_over_a_persons_draft(draft(&mut app, "Lunch", "Monday?"));
}

#[test]
fn once_the_draft_is_discarded_drafting_writes_a_new_one() {
    let mut app = TestApp::<MailShell>::new().unwrap();
    save_as_a_person(&mut app, "Lunch", "Friday?");
    refused_over_a_persons_draft(draft(&mut app, "Lunch", "Monday?"));
    app.call_confirmed("discard-draft", json!({})).unwrap();
    app.settle();
    draft(&mut app, "Lunch", "Monday?").unwrap();
    assert_eq!(app.state().draft.body, "Monday?");
    assert!(app.state().draft.drafted.is_some());
    app.call_confirmed("discard-draft", json!({})).unwrap();
    assert_eq!(app.state().draft.drafted, None, "discarding forgets it");
}

#[test]
fn draft_reply_writes_over_its_own_untouched_reply_and_leaves_a_persons_alone() {
    let mut app = TestApp::<MailShell>::new().unwrap();
    let id = sample(&mut app);
    let reply = |app: &mut TestApp<MailShell>, body: &str| {
        let replied = app.call("draft-reply", json!({ "message": id, "body": body }));
        app.settle();
        replied.map(drop).map_err(|refused| refused.to_string())
    };
    reply(&mut app, "Thanks Sam.").unwrap();
    reply(&mut app, "Thanks Sam, looks good.").expect("its own reply is written over");
    let state = app.state();
    assert!(state.draft.body.starts_with("Thanks Sam, looks good.\n"));
    assert_eq!(state.draft.subject, "Re: Quarterly numbers");
    // A reply drafted over a message drafted, and the other way round.
    draft(&mut app, "Lunch", "Friday?").unwrap();
    reply(&mut app, "Thanks Sam.").unwrap();
    assert_eq!(app.state().draft.subject, "Re: Quarterly numbers");

    save_as_a_person(&mut app, "Re: Quarterly numbers", "Thanks Sam. More soon.");
    refused_over_a_persons_draft(reply(&mut app, "Thanks again."));
    assert_eq!(app.state().draft.body, "Thanks Sam. More soon.");
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
