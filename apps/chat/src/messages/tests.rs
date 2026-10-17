use std::rc::Rc;

use ui::framework::Chain;
use ui::tilo_resource::Id;

use super::*;
use crate::demo::Room;
use crate::parts::{PART_TEXT_BYTES, part_index};

/// A post as long as one may be: a part fills after a few dozen of them.
fn long_post(n: usize) -> String {
    let line = format!("post {n} ");
    line.repeat(MAX_MESSAGE_TEXT_BYTES / line.len())
}

/// Post [`long_post`]s in `#general` until one no longer fits part
/// `part`; that post's id, every one before it checked to be in `part`.
fn fill_part(room: &mut Room, part: u64) -> String {
    for n in 0..=PART_TEXT_BYTES / long_post(0).len() + 1 {
        let id = room.say("u_aria", "c_general", &long_post(n)).unwrap();
        match room.messages().part_of(&Id::from_raw(&id)) {
            Some(at) if at == part => {}
            _ => return id,
        }
    }
    panic!("part {part} never filled");
}

fn react(message: &str, emoji: &str) -> ui::tilo_resource::Invoke<ChatPart> {
    ChatPart::REACT.with(chat_part::React {
        message: Id::from_raw(message),
        emoji: emoji.into(),
    })
}

fn edit(message: &str, text: &str) -> ui::tilo_resource::Invoke<ChatPart> {
    ChatPart::EDIT_MESSAGE.with(chat_part::EditMessage {
        message: Id::from_raw(message),
        text: text.into(),
        body: None,
        mentions: None,
    })
}

#[test]
fn a_full_part_rolls_posts_over_and_changes_land_where_the_message_is() {
    let mut room = Room::demo().unwrap();
    let welcome = "m_welcome";
    assert_eq!(room.messages().part_of(&Id::from_raw(welcome)), Some(0));
    let mine = room
        .say(crate::model::ME, "c_general", "mine, to edit later")
        .unwrap();
    assert_eq!(room.messages().part_of(&Id::from_raw(&mine)), Some(0));
    // Long posts fill the head; the one after it is full starts part 1.
    let later = fill_part(&mut room, 0);
    assert_eq!(room.messages().part_of(&Id::from_raw(&later)), Some(1));
    assert_eq!(room.part_numbers(), vec![0, 1]);

    // A reaction, an edit and a reply to messages of the full part go to it.
    assert_eq!(
        room.apply_part_as(crate::model::ME, react(welcome, "🎉"))
            .unwrap(),
        0
    );
    assert_eq!(
        room.apply_part_as(crate::model::ME, edit(&mine, "mine, edited"))
            .unwrap(),
        0
    );
    let reply = crate::demo::post("c_general", "a reply in the old thread", Some(welcome));
    assert_eq!(room.apply_part_as("u_sora", reply).unwrap(), 0);
    // A post that is not a reply goes on in the head.
    assert_eq!(
        room.apply_part_as(
            "u_sora",
            crate::demo::post("c_general", "still the head", None)
        )
        .unwrap(),
        1
    );

    // A reader sees the channel across both parts, in order.
    let rows = room.message_rows("c_general");
    let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids.first(), Some(&welcome));
    let at = |id: &str| ids.iter().position(|row| *row == id).unwrap();
    assert!(at(&mine) < at(&later));
    let welcome_row = rows.iter().find(|row| row.id == welcome).unwrap();
    assert!(
        welcome_row
            .reactions
            .iter()
            .any(|reaction| reaction.emoji == "🎉")
    );
    assert_eq!(
        rows.iter().find(|row| row.id == mine).unwrap().text,
        "mine, edited"
    );
    assert!(
        rows.iter()
            .any(|row| row.parent.as_deref() == Some(welcome))
    );
    assert_eq!(rows.last().unwrap().text, "still the head");
}

#[test]
fn a_post_is_read_against_the_chat_record_where_it_is_invoked() {
    let mut room = Room::demo().unwrap();
    // A voice channel takes no posts, and an unknown channel none either.
    assert!(room.say("u_aria", "c_standup", "hello?").is_err());
    assert!(room.say("u_aria", "c_nowhere", "hello?").is_err());
    // A mention is resolved against the record's people, whatever the
    // caller sent.
    let mut post = crate::demo::post("c_general", "@Aria Chen look", None);
    post.arguments.insert(
        "mentions".into(),
        serde_json::json!([{ "user": "u_nobody", "start": 0, "len": 3, "is_agent": false }]),
    );
    room.apply_part_as("u_milo", post).unwrap();
    let row = room.message_rows("c_general").pop().unwrap();
    assert_eq!(row.mentions.len(), 1);
    assert_eq!(row.mentions[0].user, "u_aria");
}

#[test]
fn the_window_chain_rolls_over_and_reads_across_its_parts() {
    let chat = Rc::new(crate::demo::demo(crate::model::ME));
    let chain: Chain<ChatPart> =
        Chain::detached("so.tilo.chat", crate::model::ME, move || Rc::clone(&chat)).unwrap();
    for invoke in crate::resource::demo_part_invokes() {
        chain.invoke(&invoke).unwrap();
    }
    assert_eq!(chain.head(), Some(0));
    let mut next = 0;
    for n in 0..=PART_TEXT_BYTES / long_post(0).len() + 1 {
        next = chain
            .stage_as(
                &crate::demo::post("c_general", &long_post(n), None),
                "u_aria",
            )
            .unwrap();
        if next != 0 {
            break;
        }
    }
    assert_eq!(next, 1, "a full head rolls the next post over");
    let first = chain
        .invoke(&crate::demo::post("c_general", "the first of part 1", None))
        .unwrap();
    assert_eq!(first, 1, "posts go on in the new head");
    assert_eq!(chain.head(), Some(1));
    // The new part is the chain's part 1, as every device names it.
    assert_eq!(part_index(&crate::parts::part(1)), Some(1));
    // A reaction to the demo's first message lands in part 0.
    assert_eq!(chain.invoke(&react("m_welcome", "👍")).unwrap(), 0);
    let held = Messages::new(chain.parts());
    assert_eq!(held.parts().len(), 2);
    assert_eq!(held.part_of(&Id::from_raw("m_welcome")), Some(0));
    let rows = crate::demo::demo(crate::model::ME).message_rows(&held, "c_general");
    assert_eq!(rows.last().unwrap().text, "the first of part 1");
    assert!(
        rows.iter()
            .find(|row| row.id == "m_welcome")
            .unwrap()
            .reactions
            .iter()
            .any(|reaction| reaction.emoji == "👍" && reaction.users == [crate::model::ME])
    );
}

#[test]
fn read_marks_are_positions_in_the_chain() {
    let mut room = Room::demo().unwrap();
    let general = Id::from_raw("c_general");
    fill_part(&mut room, 0);
    room.say("u_milo", "c_general", "in part 1").unwrap();
    let held = room.messages();
    let end = held.read_to_end(&general);
    assert_eq!(end, crate::parts::ReadMark { part: 1, seen: 2 });
    assert_eq!(held.unread(&general, end), 0);
    let before = crate::parts::ReadMark { part: 0, seen: 1 };
    assert_eq!(held.unread(&general, before), held.top_level(&general) - 1);
    room.apply_as(
        crate::model::ME,
        crate::resource::Chat::MARK_READ.with(crate::record::chat::MarkRead {
            channel: general.clone(),
            part: 1,
            seen: 2,
        }),
    )
    .unwrap();
    assert_eq!(room.read().unread(&held, "c_general", crate::model::ME), 0);
    // A mark never moves back.
    room.apply_as(
        crate::model::ME,
        crate::resource::Chat::MARK_READ.with(crate::record::chat::MarkRead {
            channel: general,
            part: 0,
            seen: 1,
        }),
    )
    .unwrap();
    assert_eq!(room.read().read_mark("c_general", crate::model::ME), end);
}
