//! A mention in a chat message, as the workspace tells it: posted through
//! the part's own verb, synced to the person named, and read from the
//! record by the framework (`ui::mentions`), with no chat window open.

use ui::automerge::AutoCommit;
use ui::mentions::{Look, MentionInbox, Place};
use ui::record::RecordId;
use ui::widget::BlockContent;
use ui::widget::marks::{Mark, MarkSpan};

use super::tests::{Talk, id};
use crate::messages::{NewMessage, chat_part};
use crate::parts::ChatPart;

const ADA: &str = "u_ada";
const BO: &str = "u_bo";
const CY: &str = "u_cy";

fn name(user: &str) -> String {
    match user {
        ADA => "Ada".into(),
        BO => "Bo".into(),
        other => other.into(),
    }
}

#[test]
fn a_mention_in_a_message_tells_the_person_named_once_and_opens_the_message() {
    let mut ada = Talk::new();
    let mut bo = ada.fork_as(BO);
    let mut paragraph = BlockContent::paragraph("b1", "@Bo the deploy is green");
    paragraph.marks = vec![MarkSpan::new(0..3, Mark::Mention(BO.into()))];
    let body = vec![tilo_rich_text::clip_of_content(&paragraph)];
    ada.apply(ChatPart::POST.with(chat_part::Post {
        message: NewMessage {
            channel: id("c_chat"),
            text: "@Bo the deploy is green".into(),
            parent: None,
            body: Some(body),
            attachment: None,
            mentions: None,
            written: None,
        },
    }))
    .unwrap();
    let message = ada.said("@Bo the deploy is green");
    // Bo's device receives the room's records; no chat window is open.
    bo.sync_with(&mut ada);
    // As the room sends them: the chat's own record names no identity.
    let chat = AutoCommit::load(&bo.chat.save()).unwrap();
    let part = AutoCommit::load(&bo.part.save()).unwrap();
    let route = RecordId::package_primary().part(0).as_str().to_owned();
    let look = Look {
        app: "so.tilo.chat",
        record: &route,
        kind: "chat.workspace",
        doc: &part,
        tree: Some(&chat),
        quiet: false,
    };

    let mut inbox = MentionInbox::new([BO.to_owned()]);
    let told = inbox.look(&look, &name);
    assert_eq!(told.show.len(), 1, "{told:?}");
    let notice = &told.show[0];
    assert_eq!(notice.title, "Ada mentioned you in #chat");
    assert_eq!(notice.body, "@Bo the deploy is green");
    assert_eq!(
        Place::from_activation(&notice.place.activation()),
        Some(Place {
            app: "so.tilo.chat".into(),
            record: String::new(),
            resource: crate::testing::item("messages", message.as_str()),
        }),
        "choosing it opens the message, in the chat's own record"
    );
    // Opening the chat again, or the device reloading, tells nothing more.
    assert!(inbox.look(&look, &name).show.is_empty());
    let mut reloaded = MentionInbox::new([BO.to_owned()]);
    reloaded.restore(&inbox.told_state());
    assert!(reloaded.look(&look, &name).show.is_empty());

    // Someone else in the room, and Ada herself, are told nothing.
    for other in [CY, ADA] {
        let mut inbox = MentionInbox::new([other.to_owned()]);
        assert!(inbox.look(&look, &name).show.is_empty(), "{other}");
    }
}
