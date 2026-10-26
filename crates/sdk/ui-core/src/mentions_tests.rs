use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ObjType, ROOT};

use super::*;
use crate::record::{RecordId, RecordKind};

const ADA: &str = "u-ada";
const BEA: &str = "u-bea";
const CAI: &str = "u-cai";
const NOTE: &str = "tab:00000000000000a1";

fn name(id: &str) -> String {
    match id {
        ADA => "Ada".into(),
        BEA => "Bea".into(),
        _ => id.into(),
    }
}

/// A notebook as `tilo_resource::Collection` keeps it: one page, its title
/// and a paragraph whose text marks `mention` (as `tilo-doc` does), written
/// by `by`.
fn notebook(text: &str, mention: Option<(usize, usize, &str)>, by: &str) -> AutoCommit {
    let mut doc = AutoCommit::new();
    crate::record::init(
        &mut doc,
        &RecordId::parse(NOTE).unwrap(),
        &RecordKind::new("notes.notebook"),
        "room",
    );
    let pages = doc.put_object(ROOT, "pages", ObjType::Map).unwrap();
    let entry = doc.put_object(&pages, "p1", ObjType::Map).unwrap();
    doc.put(&entry, "k", "a").unwrap();
    let page = doc.put_object(&entry, "v", ObjType::Map).unwrap();
    doc.put(&page, "title", "Launch plan").unwrap();
    let body = doc.put_object(&page, "body", ObjType::Map).unwrap();
    let blocks = doc.put_object(&body, "blocks", ObjType::Map).unwrap();
    let block = doc.put_object(&blocks, "b7", ObjType::Text).unwrap();
    doc.splice_text(&block, 0, 0, text).unwrap();
    let length = text.chars().count();
    doc.mark(
        &block,
        Mark::new(AUTHOR_MARK.into(), by, 0, length),
        ExpandMark::None,
    )
    .unwrap();
    if let Some((start, end, person)) = mention {
        doc.mark(
            &block,
            Mark::new(MARK.into(), person, start, end),
            ExpandMark::None,
        )
        .unwrap();
    }
    doc.commit();
    doc
}

fn look<'a>(doc: &'a AutoCommit) -> Look<'a> {
    Look {
        app: "so.tilo.notes",
        record: NOTE,
        kind: "notes.notebook",
        doc,
        tree: None,
        quiet: false,
    }
}

fn copy(doc: &mut AutoCommit) -> AutoCommit {
    AutoCommit::load(&doc.save()).unwrap()
}

#[test]
fn a_mention_in_a_record_is_one_notice_for_the_reader_it_names() {
    let mut written = notebook("Ask @Bea about the date", Some((4, 8, BEA)), ADA);
    // Bea's device holds the replica the room sent her.
    let bea_copy = copy(&mut written);
    let mut bea = MentionInbox::new([BEA.to_owned()]);
    let told = bea.look(&look(&bea_copy), &name);
    assert_eq!(told.show.len(), 1, "{told:?}");
    let notice = &told.show[0];
    assert_eq!(notice.title, "Ada mentioned you in ‘Launch plan’");
    assert_eq!(notice.body, "Ask @Bea about the date");
    assert_eq!(
        notice.place,
        Place {
            app: "so.tilo.notes".into(),
            record: NOTE.into(),
            resource: "notes.notebook/pages/p1".into(),
        }
    );
    assert_eq!(
        Place::from_activation(&notice.place.activation()),
        Some(notice.place.clone())
    );

    // Someone else reading the same note is told nothing; the author either.
    let mut cai = MentionInbox::new([CAI.to_owned()]);
    assert_eq!(cai.look(&look(&bea_copy), &name), Told::default());
    let mut ada = MentionInbox::new([ADA.to_owned()]);
    assert_eq!(ada.look(&look(&bea_copy), &name), Told::default());
}

#[test]
fn reopening_or_reloading_tells_nothing_again() {
    let doc = notebook("Ask @Bea", Some((4, 8, BEA)), ADA);
    let mut bea = MentionInbox::new([BEA.to_owned()]);
    assert_eq!(bea.look(&look(&doc), &name).show.len(), 1);
    // The record opened again, and again changed elsewhere.
    assert_eq!(bea.look(&look(&doc), &name), Told::default());
    // The device reloaded: what was told is kept.
    let mut reloaded = MentionInbox::new([BEA.to_owned()]);
    reloaded.restore(&bea.told_state());
    assert!(reloaded.knows("so.tilo.notes", NOTE));
    assert_eq!(reloaded.look(&look(&doc), &name), Told::default());
}

#[test]
fn a_mention_edited_away_is_withdrawn_and_one_already_seen_is_not_told() {
    let mut doc = notebook("Ask @Bea", Some((4, 8, BEA)), ADA);
    let mut bea = MentionInbox::new([BEA.to_owned()]);
    let told = bea.look(&look(&doc), &name);
    let tag = told.show[0].tag.clone();
    // Ada takes the mention back out.
    let block = doc
        .get(ROOT, "pages")
        .ok()
        .flatten()
        .and_then(|(_, pages)| doc.get(&pages, "p1").ok().flatten())
        .and_then(|(_, entry)| doc.get(&entry, "v").ok().flatten())
        .and_then(|(_, page)| doc.get(&page, "body").ok().flatten())
        .and_then(|(_, body)| doc.get(&body, "blocks").ok().flatten())
        .and_then(|(_, blocks)| doc.get(&blocks, "b7").ok().flatten())
        .map(|(_, block)| block)
        .unwrap();
    doc.unmark(&block, MARK, 0, 8, ExpandMark::None).unwrap();
    doc.commit();
    let told = bea.look(&look(&doc), &name);
    assert_eq!(told.withdraw, vec![tag]);
    assert!(told.show.is_empty());

    // What the person was looking at when it arrived is seen, not news.
    let seen = notebook("Ask @Bea", Some((4, 8, BEA)), ADA);
    let mut looking = MentionInbox::new([BEA.to_owned()]);
    let quiet = Look {
        quiet: true,
        ..look(&seen)
    };
    assert_eq!(looking.look(&quiet, &name), Told::default());
    assert_eq!(looking.look(&look(&seen), &name), Told::default());
}

#[test]
fn a_message_in_a_chat_part_opens_the_message_in_the_chat() {
    // The chat record names its channels; its messages are in a part.
    // The app's own record of the workspace names no identity of its own.
    let mut chat = AutoCommit::new();
    let channels = chat.put_object(ROOT, "channels", ObjType::Map).unwrap();
    let entry = chat
        .put_object(&channels, "c_general", ObjType::Map)
        .unwrap();
    let channel = chat.put_object(&entry, "v", ObjType::Map).unwrap();
    chat.put(&channel, "name", "general").unwrap();

    let mut part = AutoCommit::new();
    let part_route = RecordId::package_primary().part(0).as_str().to_owned();
    let messages = part.put_object(ROOT, "messages", ObjType::Map).unwrap();
    let entry = part.put_object(&messages, "m1", ObjType::Map).unwrap();
    let message = part.put_object(&entry, "v", ObjType::Map).unwrap();
    part.put(&message, "author", ADA).unwrap();
    part.put(&message, "channel", "c_general").unwrap();
    let body = part.put_object(&message, "body", ObjType::List).unwrap();
    let block = part.insert_object(&body, 0, ObjType::Map).unwrap();
    part.put(&block, "text", "@Bea can you look?").unwrap();
    let marks = part.put_object(&block, "marks", ObjType::List).unwrap();
    let span = part.insert_object(&marks, 0, ObjType::Map).unwrap();
    part.put(&span, "start", 0_i64).unwrap();
    part.put(&span, "end", 4_i64).unwrap();
    part.put(&span, "name", MARK).unwrap();
    let value = part.put_object(&span, "value", ObjType::Map).unwrap();
    part.put(&value, VARIANT_KEY, "Str").unwrap();
    part.put(&value, "0", BEA).unwrap();

    let mut bea = MentionInbox::new([BEA.to_owned()]);
    let told = bea.look(
        &Look {
            app: "so.tilo.chat",
            record: &part_route,
            kind: "chat.workspace",
            doc: &part,
            tree: Some(&chat),
            quiet: false,
        },
        &name,
    );
    assert_eq!(told.show.len(), 1, "{told:?}");
    assert_eq!(told.show[0].title, "Ada mentioned you in #general");
    assert_eq!(told.show[0].body, "@Bea can you look?");
    assert_eq!(
        told.show[0].place,
        Place {
            app: "so.tilo.chat".into(),
            record: String::new(),
            resource: "chat.workspace/messages/m1".into(),
        }
    );
}
