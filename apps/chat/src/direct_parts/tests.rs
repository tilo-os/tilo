use std::rc::Rc;

use ui::record::RecordId;
use ui::tilo_resource::Replica;

use super::*;

const ADA: &str = "adadadadadadadadadadadadadadadadadadadadadadadadadadadadadadadad";
const BOB: &str = "b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0";

fn pair() -> RecordId {
    RecordId::of_direct("direct_3f2a1c4b5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708")
        .unwrap()
}

fn device(author: &str, at: i64) -> Replica<DirectPart> {
    let mut replica = Replica::<DirectPart>::new().as_invoker(author);
    replica.set_now_ms(at);
    replica
}

fn send(device: &mut Replica<DirectPart>, id: &str, text: &str) -> Result<(), String> {
    device
        .apply(DirectPart::SEND.with(direct_part::Send {
            id: id.to_owned(),
            text: text.to_owned(),
        }))
        .map(drop)
        .map_err(|error| error.to_string())
}

fn state(device: &Replica<DirectPart>) -> Rc<DirectPart> {
    Rc::new(device.state().unwrap())
}

/// A part holding `said` by their authors, its clock at `at`.
fn part_of(said: &[(&str, &str)], at: i64) -> Rc<DirectPart> {
    let mut replica = device(ADA, at);
    for (index, (author, text)) in said.iter().enumerate() {
        replica = replica.as_invoker(author);
        send(&mut replica, &format!("dm_{index}"), text).unwrap();
    }
    state(&replica)
}

#[test]
fn parts_are_numbered_records_of_the_pairs_record() {
    let pair = pair();
    assert_eq!(part(&pair, 0), pair.part(0));
    assert_eq!(part_index(&pair, &pair.part(41)), Some(41));
    assert_eq!(part_index(&pair, &pair), None);
    assert_eq!(
        part_index(&pair, &RecordId::workspace_default().part(3)),
        None
    );
    assert_eq!(
        neighbour_wire::record_sync(pair.part(0).as_str()),
        neighbour_wire::RecordSync::Part {
            base: pair.as_str(),
            index: 0
        }
    );
}

#[test]
fn a_message_goes_to_the_next_part_once_the_head_is_full() {
    assert_eq!(post_part(4, PART_MESSAGES - 1, 0), 4);
    assert_eq!(post_part(4, PART_MESSAGES, 0), 5);
    assert_eq!(post_part(4, 10, PART_TEXT_BYTES), 5);
}

#[test]
fn a_part_started_later_carries_the_counts_before_it() {
    let first = part_of(&[(ADA, "one"), (BOB, "two"), (ADA, "three")], 1_000);
    let (ada, bob) = (
        ui::person_chat::person_key(ADA),
        ui::person_chat::person_key(BOB),
    );
    let carried = first.carried_after(0);
    assert_eq!(carried["0"][&ada], 2);
    assert_eq!(carried["0"][&bob], 1);
    // A part of its own counts what it holds, beside what came before.
    let second = part_of(&[(BOB, "four")], 2_000);
    let carried = second.carried_after(1);
    assert_eq!(carried["1"][&bob], 1);
    assert!(!carried.contains_key("0"));
}

#[test]
fn a_send_is_the_same_message_once() {
    let mut ada = device(ADA, 1_000);
    send(&mut ada, "dm_1", "hi").unwrap();
    send(&mut ada, "dm_1", "hi").unwrap();
    let part = ada.state().unwrap();
    assert_eq!(part.messages.len(), 1);
    let (id, message) = part.messages.iter().next().unwrap();
    assert_eq!(id.as_str(), "dm_1");
    assert_eq!(message.author, ui::person_chat::person_key(ADA));
    assert_eq!(message.text, "hi");
    assert_eq!(message.at, 1_000);
}

#[test]
fn an_empty_or_oversized_message_is_refused() {
    let mut ada = device(ADA, 1_000);
    assert!(send(&mut ada, "dm_1", "  ").is_err());
    assert!(send(&mut ada, "dm_2", &"x".repeat(MAX_TEXT_BYTES + 1)).is_err());
    assert!(send(&mut ada, "no spaces", "hi").is_err());
    assert!(ada.state().unwrap().messages.is_empty());
}

#[test]
fn an_import_takes_each_stored_message_once() {
    let mut ada = device(ADA, 1_000);
    let stored = StoredMessage {
        id: "dm_old".into(),
        author: ui::person_chat::person_key(BOB),
        text: "before".into(),
        at: 1,
    };
    let import = |ada: &mut Replica<DirectPart>| {
        ada.apply(DirectPart::IMPORT.with(direct_part::Import {
            messages: vec![stored.clone()],
        }))
        .map(drop)
        .map_err(|error| error.to_string())
    };
    import(&mut ada).unwrap();
    import(&mut ada).unwrap();
    let state = ada.state().unwrap();
    assert_eq!(state.messages.len(), 1);
    assert_eq!(
        state.messages.get(&Id::from_raw("dm_old")).unwrap().text,
        "before"
    );
}

#[test]
fn a_stored_message_no_person_wrote_is_not_imported() {
    let mut ada = device(ADA, 1_000);
    let bad = StoredMessage {
        id: "dm_old".into(),
        author: "not a person".into(),
        text: "before".into(),
        at: 1,
    };
    assert!(
        ada.apply(DirectPart::IMPORT.with(direct_part::Import {
            messages: vec![bad],
        }))
        .is_err()
    );
    assert!(ada.state().unwrap().messages.is_empty());
}

#[test]
fn the_message_id_is_the_invocation() {
    let invocation = uuid::Uuid::nil();
    assert_eq!(message_id(invocation), format!("dm_{invocation}"));
    assert!(valid_message_id(&message_id(invocation)));
}

#[test]
fn reading_to_the_end_marks_the_newest_part_with_the_other_persons_messages() {
    let (_ada, bob) = (
        ui::person_chat::person_key(ADA),
        ui::person_chat::person_key(BOB),
    );
    let conversation = Conversation::new(vec![
        (0, part_of(&[(BOB, "one"), (ADA, "two")], 1_000)),
        (1, part_of(&[(ADA, "three")], 2_000)),
    ]);
    assert_eq!(
        conversation.read_to_end(&bob),
        DirectReadMark { part: 0, seen: 1 }
    );
    // A partner who said nothing there read nothing into it.
    assert_eq!(
        conversation
            .read_to_end("0101010101010101010101010101010101010101010101010101010101010101"),
        DirectReadMark::default()
    );
    // Nothing of theirs in the second part: the first is where they said
    // the last thing.
    let conversation = Conversation::new(vec![
        (0, part_of(&[(BOB, "one"), (BOB, "two")], 1_000)),
        (1, part_of(&[(ADA, "three")], 2_000)),
    ]);
    assert_eq!(
        conversation.read_to_end(&bob),
        DirectReadMark { part: 0, seen: 2 }
    );
}

#[test]
fn unread_reaches_back_through_the_parts_a_device_does_not_hold() {
    let (ada, bob) = (
        ui::person_chat::person_key(ADA),
        ui::person_chat::person_key(BOB),
    );
    // This device holds the head only: parts 0 and 1 are known through the
    // counts the head carries.
    let mut head = (*part_of(&[(BOB, "four"), (BOB, "five"), (BOB, "six")], 3_000)).clone();
    head.earlier.insert(
        "0".to_owned(),
        BTreeMap::from([(ada.clone(), 1), (bob.clone(), 2)]),
    );
    head.earlier
        .insert("1".to_owned(), BTreeMap::from([(bob.clone(), 1)]));
    let conversation = Conversation::new(vec![(2, Rc::new(head))]);
    // Read part 0 up to its first message of theirs: two there, one in part
    // 1, three in the head.
    let mark = DirectReadMark { part: 0, seen: 1 };
    assert_eq!(conversation.unread(&bob, mark), 2 - 1 + 1 + 3);
    // Everything of theirs up to the head is read.
    assert_eq!(
        conversation.unread(&bob, DirectReadMark { part: 2, seen: 3 }),
        0
    );
    // Nothing read yet: everything of theirs.
    assert_eq!(conversation.unread(&bob, DirectReadMark::default()), 6);
    // A partner who said nothing has nothing unread.
    assert_eq!(
        conversation.unread(
            "0101010101010101010101010101010101010101010101010101010101010101",
            DirectReadMark::default()
        ),
        0
    );
}

#[test]
fn a_later_read_mark_is_further() {
    let early = DirectReadMark { part: 2, seen: 900 };
    let later = DirectReadMark { part: 3, seen: 1 };
    assert!(later > early);
    // A full part read is before the next part's first message.
    let done = DirectReadMark {
        part: 2,
        seen: 2000,
    };
    assert!(later > done);
}
