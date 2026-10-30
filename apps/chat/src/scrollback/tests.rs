//! A device that joins a chat whose messages fill several parts: the workspace
//! sends it the head only, and it reads back through the earlier parts.

use crate::testing::MountedChat;
use ui::framework::testing::TestApp;
use ui::framework::{Chain, PartArrival};
use ui::tilo_resource::Id;

use crate::ChatShell;
use crate::parts::{ChatPart, PART_TEXT_BYTES, ReadMark};
use crate::record::{CHAT_CHANNEL, MAX_MESSAGE_TEXT_BYTES, chat};
use crate::resource::Chat;

type Window = TestApp<ChatShell>;

/// The channel the long posts that fill each part go to.
const ARCHIVE: &str = "c_archive";

/// A post as long as one may be: a part fills after a few dozen of them.
fn long_post(n: usize) -> String {
    let line = format!("filler {n} ");
    line.repeat(MAX_MESSAGE_TEXT_BYTES / line.len())
}

fn chain_of(window: &mut Window) -> Chain<ChatPart> {
    window
        .with_chat(|surface| surface.chain.clone())
        .flatten()
        .expect("the window holds the chat's messages")
}

/// The parts the window holds.
fn held(window: &mut Window) -> Vec<u64> {
    chain_of(window)
        .parts()
        .iter()
        .map(|(index, _)| *index)
        .collect()
}

/// The texts of the channel in view, oldest first.
fn shown(window: &mut Window) -> Vec<String> {
    window
        .with_chat(|surface| {
            surface
                .messages
                .iter()
                .map(|message| message.text.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn create_archive(window: &mut Window) {
    window
        .invoke(
            Chat::CREATE_CHANNEL_WITH_ID.with(chat::CreateChannelWithId {
                id: ARCHIVE.into(),
                name: "archive".parse().unwrap(),
                category: None,
                kind: None,
                topic: None,
            }),
        )
        .unwrap();
}

/// A chat on `writer`'s device whose `#chat` has `per_part` short
/// messages in each of parts 0, 1 and 2, named `"<part>.<n>"`; the long
/// posts that fill each part go to `#archive`.
fn three_parts(writer: &mut Window, per_part: usize) {
    create_archive(writer);
    let chain = chain_of(writer);
    let mut part = 0;
    loop {
        for n in 0..per_part {
            let at = chain
                .stage_as(
                    &crate::demo::post(CHAT_CHANNEL, &format!("{part}.{n}"), None),
                    "u_aria",
                )
                .unwrap();
            assert_eq!(at, part);
        }
        if part == 2 {
            break;
        }
        for n in 0..=PART_TEXT_BYTES / long_post(0).len() + 1 {
            if chain
                .stage_as(&crate::demo::post(ARCHIVE, &long_post(n), None), "u_milo")
                .unwrap()
                != part
            {
                break;
            }
        }
        part += 1;
        assert_eq!(chain.head(), Some(part), "a full part rolls over");
    }
    writer.harness().relayout();
}

/// `reader` is a device that just joined: the workspace sends it the chain's
/// head only.
fn join(writer: &mut Window, reader: &mut Window) {
    chain_of(reader).hold(2).unwrap();
    writer
        .deliver_record_to(reader, &crate::parts::part(2))
        .unwrap();
    reader.harness().relayout();
}

fn loading(window: &mut Window) -> bool {
    window
        .with_chat(|surface| surface.scrollback.loading())
        .unwrap()
}

/// Where the message saying `text` was painted, from the view's top.
fn painted_at(window: &mut Window, text: &str) -> Option<f64> {
    window
        .with_chat(|surface| {
            let id = &surface
                .messages
                .iter()
                .find(|message| message.text == text)?
                .id;
            surface
                .message_rows
                .iter()
                .find(|(_, row)| row == id)
                .map(|(rect, _)| rect.y0)
        })
        .flatten()
}

fn open_channel(window: &mut Window, channel: &str) {
    window.with_chat(|surface| {
        let _ = surface.answer_effect(crate::effects::ChatEffect::OpenChannel {
            channel: channel.into(),
        });
    });
    window.harness().relayout();
}

#[test]
fn scrolling_back_opens_earlier_parts_in_order_without_moving_the_view() {
    let mut ada = Window::on_device("ada").unwrap();
    let mut bo = Window::on_device("bo").unwrap();
    // More than two views of messages in each part: the top of the head
    // is far from the view at the newest edge.
    let per_part = 150;
    three_parts(&mut ada, per_part);
    join(&mut ada, &mut bo);
    // At the newest edge, only the head is held and shown.
    assert_eq!(held(&mut bo), vec![2]);
    let texts = shown(&mut bo);
    assert_eq!(texts.first().map(String::as_str), Some("2.0"));
    assert_eq!(texts.len(), per_part);
    bo.harness().relayout();
    assert_eq!(held(&mut bo), vec![2], "nobody read back yet");

    // The reader scrolls to the top of what is held: the part before is
    // opened, and is on its way until the workspace's copy arrives.
    bo.with_chat(|surface| {
        surface.scroll = 1.0e6;
        surface.timeline_at_end = false;
    });
    bo.harness().relayout();
    bo.harness().relayout();
    assert_eq!(held(&mut bo), vec![1, 2]);
    assert_eq!(chain_of(&mut bo).waiting(1), Some(PartArrival::Arriving));
    assert!(loading(&mut bo));
    let top = painted_at(&mut bo, "2.0").expect("the reader looks back at the head's first");
    // Nothing more is asked for while part 1 is on its way.
    bo.harness().relayout();
    assert_eq!(held(&mut bo), vec![1, 2]);

    // Part 1 arrives: its messages show above, and the message the reader
    // was looking at stays where it was.
    ada.deliver_record_to(&mut bo, &crate::parts::part(1))
        .unwrap();
    bo.harness().relayout();
    assert_eq!(chain_of(&mut bo).waiting(1), None);
    let texts = shown(&mut bo);
    assert_eq!(texts.first().map(String::as_str), Some("1.0"));
    assert_eq!(texts.len(), 2 * per_part);
    let at = painted_at(&mut bo, "2.0").expect("still in view");
    assert!((at - top).abs() < 1.0, "it did not move: {top} -> {at}");

    // Reading on back reaches part 0, the chain's first.
    bo.with_chat(|surface| surface.scroll = 1.0e6);
    bo.harness().relayout();
    bo.harness().relayout();
    assert_eq!(held(&mut bo), vec![0, 1, 2]);
    ada.deliver_record_to(&mut bo, &crate::parts::part(0))
        .unwrap();
    bo.harness().relayout();
    let texts = shown(&mut bo);
    assert_eq!(texts.first().map(String::as_str), Some("0.0"));
    assert_eq!(texts.len(), 3 * per_part);
    assert!(!loading(&mut bo), "nothing is earlier than part 0");
}

#[test]
fn a_short_channel_reads_back_until_it_has_something_to_show() {
    let mut ada = Window::on_device("ada").unwrap();
    let mut bo = Window::on_device("bo").unwrap();
    three_parts(&mut ada, 2);
    join(&mut ada, &mut bo);
    // Two messages do not fill the view: the part before is opened at once.
    bo.harness().relayout();
    assert_eq!(held(&mut bo), vec![1, 2]);
    ada.deliver_record_to(&mut bo, &crate::parts::part(1))
        .unwrap();
    bo.harness().relayout();
    bo.harness().relayout();
    assert_eq!(held(&mut bo), vec![0, 1, 2]);
    ada.deliver_record_to(&mut bo, &crate::parts::part(0))
        .unwrap();
    bo.harness().relayout();
    assert_eq!(shown(&mut bo), ["0.0", "0.1", "1.0", "1.1", "2.0", "2.1"]);
}

#[test]
fn a_link_to_a_message_in_a_part_not_held_opens_parts_until_it_is_found() {
    let mut ada = Window::on_device("ada").unwrap();
    let mut bo = Window::on_device("bo").unwrap();
    three_parts(&mut ada, 30);
    let wanted = ada
        .with_chat(|surface| {
            surface
                .search_messages
                .iter()
                .find(|message| message.text == "0.4")
                .map(|message| message.id.clone())
        })
        .flatten()
        .expect("ada holds every part");
    join(&mut ada, &mut bo);
    // The link is opened as a search hit or an agent's `open` is.
    let link = crate::testing::item("messages", &wanted);
    bo.with_chat(|surface| surface.opener()(&link));
    bo.harness().relayout();
    for part in [1, 0] {
        bo.harness().relayout();
        assert!(held(&mut bo).contains(&part), "part {part} is opened");
        ada.deliver_record_to(&mut bo, &crate::parts::part(part))
            .unwrap();
        bo.harness().relayout();
    }
    bo.harness().relayout();
    assert_eq!(
        bo.with_chat(|surface| surface.revealed_message.clone())
            .flatten(),
        Some(wanted)
    );
}

/// A reader away for parts counts every message after their mark, from the
/// counts the head carries of the parts before it, without holding them.
#[test]
fn a_joined_device_counts_unread_across_parts_it_does_not_hold() {
    let mut ada = Window::on_device("ada").unwrap();
    let mut bo = Window::on_device("bo").unwrap();
    three_parts(&mut ada, 3);
    // Bo reads `#archive`, and read `#chat` on another device up to
    // the first message of part 1.
    create_archive(&mut bo);
    open_channel(&mut bo, ARCHIVE);
    bo.invoke(Chat::MARK_READ.with(chat::MarkRead {
        channel: Id::from_raw(CHAT_CHANNEL),
        part: 1,
        seen: 1,
    }))
    .unwrap();
    join(&mut ada, &mut bo);
    let unread = |bo: &mut Window| {
        bo.with_chat(|surface| {
            surface
                .channels
                .iter()
                .find(|channel| channel.id == CHAT_CHANNEL)
                .map(|channel| channel.unread)
        })
        .flatten()
    };
    bo.harness().relayout();
    // Two after the mark in part 1, three in part 2; part 0 is not read.
    assert_eq!(unread(&mut bo), Some(5));
    assert_eq!(held(&mut bo), vec![2], "nothing earlier is held for it");

    // Opening `#chat` reads back from its few messages; the line goes
    // before the first unread one once its part is here.
    open_channel(&mut bo, CHAT_CHANNEL);
    bo.harness().relayout();
    assert_eq!(held(&mut bo), vec![1, 2]);
    ada.deliver_record_to(&mut bo, &crate::parts::part(1))
        .unwrap();
    bo.harness().relayout();
    let first = bo
        .with_chat(|surface| {
            let id = surface.scrollback.first_unread.clone()?;
            surface
                .messages
                .iter()
                .find(|message| message.id == id)
                .map(|message| message.text.clone())
        })
        .flatten();
    assert_eq!(first.as_deref(), Some("1.1"));
}

#[test]
fn the_first_unread_message_follows_the_read_mark() {
    let mut ada = Window::on_device("ada").unwrap();
    three_parts(&mut ada, 3);
    let messages = crate::testing::mounted_messages(&mut ada);
    let general = Id::from_raw(CHAT_CHANNEL);
    let mark = ReadMark { part: 1, seen: 2 };
    let text = |messages: &crate::messages::Messages, id: Option<&Id>| {
        id.and_then(|id| messages.get(id))
            .map(|message| message.text.clone())
    };
    assert_eq!(messages.unread(&general, mark), 4);
    assert_eq!(
        text(&messages, messages.first_unread(&general, mark)).as_deref(),
        Some("1.2")
    );
    // Held from the head only, the count reaches back through what the head
    // carries of part 1, and the line goes before the first message held
    // after the mark.
    let head = crate::messages::Messages::new(
        messages
            .parts()
            .iter()
            .filter(|(index, _)| *index == 2)
            .cloned()
            .collect(),
    );
    assert_eq!(head.unread(&general, mark), 4);
    assert_eq!(
        text(&head, head.first_unread(&general, mark)).as_deref(),
        Some("2.0")
    );
}

/// A never-read channel counts its whole history, and a mark in the first
/// part every message since, however few parts are held.
#[test]
fn unread_counts_reach_the_first_part_without_holding_it() {
    let mut ada = Window::on_device("ada").unwrap();
    let mut bo = Window::on_device("bo").unwrap();
    let per_part = 4;
    three_parts(&mut ada, per_part);
    join(&mut ada, &mut bo);
    // The head only, as the workspace sent it: what the window reads back to
    // for its short channel is left out.
    let messages = crate::messages::Messages::new(
        chain_of(&mut bo)
            .parts()
            .into_iter()
            .filter(|(index, _)| *index == 2)
            .collect(),
    );
    let general = Id::from_raw(CHAT_CHANNEL);
    assert_eq!(messages.unread(&general, ReadMark::default()), 3 * per_part);
    assert_eq!(
        messages.unread(&general, ReadMark { part: 0, seen: 1 }),
        3 * per_part - 1
    );
    let archive = Id::from_raw(ARCHIVE);
    let everywhere = crate::testing::mounted_messages(&mut ada);
    assert_eq!(
        messages.unread(&archive, ReadMark::default()),
        everywhere.unread(&archive, ReadMark::default()),
        "the long posts that filled the parts are counted as the writer counts them"
    );
}

/// Part 0, which the workspace's hosts have none of: the timeline says earlier
/// messages are not available, asks for nothing more and spins nothing,
/// until it arrives after all.
#[test]
fn an_earlier_part_the_workspace_lacks_ends_in_a_final_state() {
    let mut ada = Window::on_device("ada").unwrap();
    let mut bo = Window::on_device("bo").unwrap();
    three_parts(&mut ada, 2);
    join(&mut ada, &mut bo);
    bo.harness().relayout();
    ada.deliver_record_to(&mut bo, &crate::parts::part(1))
        .unwrap();
    bo.harness().relayout();
    bo.harness().relayout();
    assert_eq!(held(&mut bo), vec![0, 1, 2]);
    assert_eq!(chain_of(&mut bo).waiting(0), Some(PartArrival::Arriving));

    bo.room_answered(crate::parts::part(0));
    bo.harness().relayout();
    let state = |bo: &mut Window| {
        bo.with_chat(|surface| {
            let pager = surface.scrollback.pager.as_ref().unwrap();
            (
                pager.unavailable(),
                pager.asked().is_some(),
                surface.reads_back(),
            )
        })
        .unwrap()
    };
    assert_eq!(
        state(&mut bo),
        (true, false, true),
        "said, nothing spinning"
    );
    assert_eq!(chain_of(&mut bo).waiting(0), Some(PartArrival::Unavailable));
    bo.harness().relayout();
    assert_eq!(held(&mut bo), vec![0, 1, 2], "nothing more is asked for");

    ada.deliver_record_to(&mut bo, &crate::parts::part(0))
        .unwrap();
    bo.harness().relayout();
    assert_eq!(state(&mut bo), (false, false, false));
    assert_eq!(shown(&mut bo), ["0.0", "0.1", "1.0", "1.1", "2.0", "2.1"]);
}

/// A window that has not chosen its channel yet (its chat record still on
/// its way when the head is taken up) shows no timeline, and reads nothing
/// back for it.
#[test]
fn no_channel_in_view_reads_nothing_back() {
    let mut ada = Window::on_device("ada").unwrap();
    let mut bo = Window::on_device("bo").unwrap();
    three_parts(&mut ada, 150);
    join(&mut ada, &mut bo);
    bo.with_chat(|surface| {
        surface.active_channel.clear();
        surface.messages.clear();
    });
    bo.harness().relayout();
    bo.harness().relayout();
    assert_eq!(held(&mut bo), vec![2]);
}

/// A channel whose messages are all in the head (here one made after the
/// earlier parts filled) reads nothing back: the head's counts of the parts
/// before say it has none there.
#[test]
fn a_channel_with_nothing_earlier_reads_nothing_back() {
    let mut ada = Window::on_device("ada").unwrap();
    let mut bo = Window::on_device("bo").unwrap();
    three_parts(&mut ada, 2);
    bo.invoke(
        Chat::CREATE_CHANNEL_WITH_ID.with(chat::CreateChannelWithId {
            id: "c_quiet".into(),
            name: "quiet".parse().unwrap(),
            category: None,
            kind: None,
            topic: None,
        }),
    )
    .unwrap();
    open_channel(&mut bo, "c_quiet");
    join(&mut ada, &mut bo);
    bo.harness().relayout();
    bo.harness().relayout();
    assert_eq!(held(&mut bo), vec![2]);
    assert!(!bo.with_chat(|surface| surface.reads_back()).unwrap());
}
