//! A mention in a chat message, as the workspace reads it: posted through the part's own verb,
//! synced, and found in the record by the framework (`ui::mentions`), with no chat window open.

use ui::automerge::AutoCommit;
use ui::record::RecordId;
use ui::widget::BlockContent;
use ui::widget::marks::{Mark, MarkSpan};

use super::tests::{ADA, Talk, id};
use crate::messages::{NewMessage, chat_part};
use crate::parts::ChatPart;

const BO: &str = "u_bo";

#[test]
fn a_mention_in_a_message_is_found_in_the_record_the_workspace_sent() {
    let mut ada = Talk::new();
    let mut bo = ada.fork_as(BO);
    let message_text = "@Bo the deploy is green";
    let mut paragraph = BlockContent::paragraph("b1", message_text);
    paragraph.marks = vec![MarkSpan::new(0..3, Mark::Mention(BO.into()))];
    let body = vec![tilo_rich_text::clip_of_content(&paragraph)];
    ada.apply(ChatPart::POST.with(chat_part::Post {
        message: NewMessage {
            channel: id("c_chat"),
            text: message_text.into(),
            parent: None,
            body: Some(body),
            attachments: Vec::new(),
            mentions: None,
            written: None,
        },
    }))
    .unwrap();
    let message = ada.said(message_text);
    // Bo's device receives the workspace's records; no chat window is open.
    bo.sync_with(&mut ada);
    let part = AutoCommit::load(&bo.part.save()).unwrap();
    let route = RecordId::package_primary().part(0).as_str().to_owned();

    let found = ui::mentions::in_record(&part);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].person, BO);
    assert_eq!(found[0].by.as_deref(), Some(ADA), "{found:#?}");
    assert_eq!(found[0].channel.as_deref(), Some(id("c_chat").as_str()));
    assert_eq!(
        found[0].item,
        Some(("messages".to_owned(), message.as_str().to_owned()))
    );
    assert_eq!(found[0].excerpt, message_text);
    // Where the chat's record is: the message's item in the chat's own record.
    assert_eq!(ui::mentions::chain_of(&route), Some(""));
}
