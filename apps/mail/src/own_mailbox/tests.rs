//! A person's own mailbox, as the build before the workspace kept one mailbox
//! stored it on their device, brought into the workspace's.
//!
//! The bytes beside this file are what that build's Mail wrote for the person
//! `a1…a1` in the test workspace: the mailbox (a folder, the composer's draft,
//! another draft, a message sent while no device held its account, a check
//! for new mail) and the part of its messages (three fetched from a server,
//! one of them since moved to the Trash, and one delivered without a server).

use ui::framework::Chained as _;
use ui::framework::testing::TestApp;
use ui::permissions::Role;
use ui::resource::DocumentId;
use ui::tilo_resource::Id;

use crate::message_states::MessageStates;
use crate::model::Folder;
use crate::resource::{MailShell, TestMail};

const PERSON: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
const UNREAD: &str = "0b8f6c1e-3d2a-4f5b-9c7e-1a2b3c4d5e60";
const TRASHED: &str = "0b8f6c1e-3d2a-4f5b-9c7e-1a2b3c4d5e62";
const DRAFT: &str = "6f1d2c3b-4a59-4e7f-8a1b-2c3d4e5f6a7b";

fn own() -> DocumentId {
    DocumentId::of_member(PERSON, "mail.mailbox").unwrap()
}

fn stored(hex: &str) -> Vec<u8> {
    hex::decode(hex.trim()).expect("hex")
}

/// The person's Mail on a device that kept their own mailbox, opened after
/// updating.
fn updated(role: Role) -> TestApp<MailShell> {
    let app = TestApp::<MailShell>::for_person_with_role(PERSON, role).unwrap();
    app.keep_stored(&own(), stored(include_str!("mailbox.hex")))
        .unwrap();
    app.keep_stored(&own().part(0), stored(include_str!("part-0.hex")))
        .unwrap();
    app.reopen().unwrap()
}

fn messages(app: &TestApp<MailShell>) -> MessageStates {
    TestMail::held_by(app)
}

/// Everything the workspace's mailbox holds, to compare two openings by.
fn everything(app: &TestApp<MailShell>) -> (crate::resource::Mail, MessageStates) {
    (app.state(), messages(app))
}

fn assert_brought_in(app: &TestApp<MailShell>) {
    let held = messages(app);
    assert_eq!(
        held.messages.len(),
        5,
        "every message, the Drafts one showing the draft among them: {:?}",
        held.messages
    );
    let unread = held.message(UNREAD).expect("a fetched message");
    assert!(!unread.read);
    assert_eq!(unread.folder, Folder::Inbox);
    let server = unread.server.as_ref().expect("what its server holds");
    assert_eq!(server.account, "ada@example.test");
    assert_eq!(server.locator, "invoice-1@shop.test");
    let trashed = held.message(TRASHED).expect("the trashed message");
    assert_eq!(trashed.folder, Folder::Trash, "what the person did is kept");
    assert_eq!(held.held.len(), 1, "the delivered message is held whole");
    let (delivered, _) = held.held.iter().next().unwrap();
    assert!(held.message(delivered.as_str()).is_some());

    let mail = app.state();
    assert_eq!(
        mail.folders
            .get(&Id::from_raw("f-receipts"))
            .map(|f| f.name.as_str()),
        Some("Receipts")
    );
    let plan = mail
        .drafts
        .get(&Id::from_raw(DRAFT))
        .expect("the other draft");
    assert_eq!(plan.subject, "Quarter plan");
    assert_eq!(plan.notes, "Check the numbers");
    let composer = mail
        .drafts
        .iter()
        .find(|(_, draft)| draft.subject == "Lunch on Friday?")
        .expect("the composer's draft, as a draft of its own");
    assert_eq!(composer.1.to, "grace@example.test");
    assert!(
        !mail.draft.subject.contains("Lunch"),
        "the workspace's composer draft is not written over"
    );
    let sends: Vec<_> = mail.outbox.pending().collect();
    assert_eq!(sends.len(), 1, "the message waiting to be sent");
    assert_eq!(sends[0].1.draft.subject, "Sent while offline");
    assert_eq!(mail.checks.pending().count(), 1);
    assert!(mail.taken_in.contains(&own().as_str().to_owned()));
}

tilo_test_env::scenario! {
    /// The mailbox a person kept as their own joins the workspace's once.
    fn a_persons_own_mailbox_joins_the_workspaces;

    phase everything_it_held_is_in_the_workspaces_mailbox_once;
    {
        let app = updated(Role::Owner);
        assert_brought_in(&app);
        let before = everything(&app);
        let app = app.reopen().unwrap();
        assert_eq!(everything(&app), before, "opening again brings nothing in twice");
    }

    phase two_devices_bringing_it_in_make_one_of_each;
    {
        let mut one = updated(Role::Owner);
        let mut other = updated(Role::Owner);
        let part = MessageStates::chain().part(0);
        one.deliver_changes_to(&mut other).unwrap();
        one.deliver_resource_to(&mut other, &part).unwrap();
        other.deliver_changes_to(&mut one).unwrap();
        other.deliver_resource_to(&mut one, &part).unwrap();
        one.settle();
        other.settle();
        for app in [&one, &other] {
            assert_brought_in(app);
            assert_eq!(app.state().drafts.len(), 2, "each draft once");
            assert_eq!(app.state().folders.len(), 1);
        }
        assert_eq!(everything(&one), everything(&other));
    }

    phase a_viewer_keeps_theirs_until_they_may_change_the_workspaces;
    {
        let mut app = updated(Role::Viewer);
        assert!(messages(&app).messages.is_empty(), "nothing is written");
        assert!(app.state().taken_in.is_empty());
        app.set_role(Role::Editor);
        let app = app.reopen().unwrap();
        assert_brought_in(&app);
    }
}
