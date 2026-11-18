//! A mention in a chat message, as the workspace reads it: posted through the part's own verb,
//! synced, and found in the resource by the framework (`ui::mentions`), with no chat window open.

use ui::automerge::AutoCommit;
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
    paragraph.marks = vec![MarkSpan::new(
        0..3,
        Mark::Mention(ui::tilo_resource::MemberId::person(BO)),
    )];
    let body = vec![tilo_rich_text::clip_of_content(&paragraph)];
    ada.apply(ChatPart::POST.with(chat_part::Post {
        message: NewMessage {
            channel: id("c_chat"),
            text: message_text.into(),
            parent: None,
            quoted: None,
            body: Some(body),
            attachments: Vec::new(),
            mentions: None,
            written: None,
            origin: None,
        },
    }))
    .unwrap();
    let message = ada.said(message_text);
    // Bo's device receives the workspace's documents; no chat window is open.
    bo.sync_with(&mut ada);
    let part = AutoCommit::load(&bo.part.save()).unwrap();

    let found = ui::mentions::in_resource(&part);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].person, ui::tilo_resource::MemberId::person(BO));
    assert_eq!(
        found[0].by,
        Some(ui::tilo_resource::MemberId::person(ADA)),
        "{found:#?}"
    );
    assert_eq!(found[0].channel.as_deref(), Some(id("c_chat").as_str()));
    assert_eq!(
        found[0].item,
        Some(("messages".to_owned(), message.as_str().to_owned()))
    );
    assert_eq!(found[0].excerpt, message_text);
    // Where the chat's resource is: the message's item in the chat's own record.
    assert_eq!(
        ui::mentions::record_of_part(crate::parts::part(0).as_str()),
        Some("")
    );
}

#[test]
fn a_direct_message_is_news_for_the_person_it_is_addressed_to() {
    let mut ada = Talk::new();
    let mut bo = ada.fork_as(BO);
    let before = AutoCommit::load(&bo.part.save()).unwrap().get_heads();
    let to_bo = super::Chat::direct_id([
        ui::tilo_resource::MemberId::person(ADA),
        ui::tilo_resource::MemberId::person(BO),
    ]);
    ada.apply(ChatPart::POST.with(chat_part::Post {
        message: NewMessage {
            channel: to_bo,
            text: "Have a look at this plan".into(),
            parent: None,
            quoted: None,
            body: None,
            attachments: Vec::new(),
            mentions: None,
            written: None,
            origin: None,
        },
    }))
    .unwrap();
    bo.sync_with(&mut ada);
    let mut part = AutoCommit::load(&bo.part.save()).unwrap();

    // What reached Bo's device names him, so a device following the chat reads it whole.
    let people = [ui::tilo_resource::MemberId::person(BO)].into();
    let touched = ui::mentions::touched_by(&part.save_after(&before), &people).unwrap();
    assert!(touched.names, "{touched:?}");
    let found = ui::mentions::in_resource(&part);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].person, ui::tilo_resource::MemberId::person(BO));
    assert_eq!(found[0].by, Some(ui::tilo_resource::MemberId::person(ADA)));
    assert_eq!(found[0].excerpt, "Have a look at this plan");
}

/// A typed `@Name` binds to the member the workspace's member list calls that, though they never
/// opened Chat: the record keeps no list of people of its own.
#[test]
fn a_typed_name_mentions_the_member_the_workspace_calls_that() {
    let mut ada = Talk::new();
    let carla = ui::tilo_resource::MemberId::person("u_carla");
    let mut members = ui::tilo_resource::members::named();
    members.push((carla.clone(), "Carla".to_owned()));
    ui::tilo_resource::members::know(members);
    let message_text = "@Carla can you look";
    ada.apply(ChatPart::POST.with(chat_part::Post {
        message: NewMessage {
            channel: id("c_chat"),
            text: message_text.into(),
            parent: None,
            quoted: None,
            body: None,
            attachments: Vec::new(),
            mentions: None,
            written: None,
            origin: None,
        },
    }))
    .unwrap();
    let message = ada.said(message_text);
    let state = ada.part.state().unwrap();
    let held = state.messages.get(&message).unwrap();
    assert_eq!(
        held.mentions
            .iter()
            .map(|mention| (mention.user.clone(), mention.slice(message_text)))
            .collect::<Vec<_>>(),
        [(carla, "@Carla")]
    );
}
